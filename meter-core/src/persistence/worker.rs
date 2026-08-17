// PersistenceWorker - 持久化任务
//
// 设计说明（按设计方案第11节）：
// - 单一消费者：所有 MeterActor 共用一个 mpsc::Sender<PersistRequest>
// - 批量写入：攒够 batch_max_size 条或超时即开事务批量执行
// - WAL 模式：减少写锁竞争
// - 非阻塞：Actor 的 tick 循环不被磁盘 I/O 阻塞

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

use super::types::*;
use serde_json::{json, Value};

/// 持久化配置
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    pub db_path: String,
    pub batch_max_size: usize,
    pub batch_timeout_ms: u64,
    pub max_connections: u32,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            db_path: "./data/meters.db".to_string(),
            batch_max_size: 200,
            batch_timeout_ms: 1000, // 1秒超时
            max_connections: 4,
        }
    }
}

/// 持久化工作器
pub struct PersistenceWorker {
    pool: SqlitePool,
    config: PersistenceConfig,
    rx: mpsc::Receiver<PersistRequest>,
    buffer: Vec<PersistRequest>,
}

impl PersistenceWorker {
    /// 创建/打开一个 SqlitePool 并跑完 migration。
    ///
    /// 拆成独立函数是为了让调用方（bootstrap 层）能拿到同一个 `SqlitePool`：
    /// 一份给 `PersistenceWorker` 做批量写，一份（clone，sqlx 的 Pool 内部是 Arc）
    /// 挂到 `MeterActorConfig::db_pool` 上做低频的 admin 配置直写。
    /// 两边共用同一个连接池 / 同一份 WAL 配置，避免各起一个 pool 打同一个 db 文件。
    pub async fn connect_pool(config: &PersistenceConfig) -> Result<SqlitePool, sqlx::Error> {
        // 确保数据目录存在
        if let Some(parent) = Path::new(&config.db_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        info!("db {:?}", config.db_path);
        // 创建连接池
        let pool = SqlitePoolOptions::new()
            .max_connections(config.max_connections)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&config.db_path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal) // WAL 模式减少写锁
                    .synchronous(SqliteSynchronous::Normal) // Normal 足够安全且更快
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await?;

        // 运行数据库迁移
        sqlx::migrate!("./migrations").run(&pool).await?;
        Self::ensure_meter_schema(&pool).await?;

        info!("PersistenceWorker pool ready: db_path={}", config.db_path);

        Ok(pool)
    }

    /// 创建新的持久化工作器（内部自建连接池）。
    ///
    /// 保留给单元测试 / 独立小工具使用；`meter-ui` 的正式启动路径请改用
    /// [`PersistenceWorker::with_pool`]，以便和 `MeterActorConfig::db_pool`
    /// 共用同一个 `SqlitePool`。
    pub async fn new(
        config: PersistenceConfig,
        rx: mpsc::Receiver<PersistRequest>,
    ) -> Result<Self, sqlx::Error> {
        let pool = Self::connect_pool(&config).await?;
        Ok(Self::with_pool(pool, config, rx))
    }

    /// 用一个已经建好（并已跑过 migration）的 `SqlitePool` 构造持久化工作器。
    pub fn with_pool(
        pool: SqlitePool,
        config: PersistenceConfig,
        rx: mpsc::Receiver<PersistRequest>,
    ) -> Self {
        let batch_max_size = config.batch_max_size; // 保存值，避免移动后使用

        info!("PersistenceWorker initialized: db_path={}", config.db_path);

        Self {
            pool,
            config,
            rx,
            buffer: Vec::with_capacity(batch_max_size),
        }
    }

    /// 启动持久化工作循环
    pub async fn run(mut self) {
        info!("PersistenceWorker started");

        let mut flush_interval = interval(Duration::from_millis(self.config.batch_timeout_ms));

        loop {
            tokio::select! {
                // 接收新请求
                Some(req) = self.rx.recv() => {
                    self.buffer.push(req);

                    // 达到批量大小，立即刷新
                    if self.buffer.len() >= self.config.batch_max_size {
                        self.flush_batch().await;
                    }
                }

                // 超时刷新
                _ = flush_interval.tick() => {
                    if !self.buffer.is_empty() {
                        self.flush_batch().await;
                    }
                }

                // 通道关闭，执行最终flush并退出
                else => {
                    info!("PersistenceWorker channel closed, performing final flush");
                    if !self.buffer.is_empty() {
                        self.flush_batch().await;
                    }
                    break;
                }
            }
        }

        info!("PersistenceWorker stopped");
    }

    /// 批量刷新缓冲区
    async fn flush_batch(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let batch_size = self.buffer.len();

        // 开始事务
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!("Failed to begin transaction: {}", e);
                return;
            }
        };

        // 提取所有请求到临时向量，避免借用冲突
        let requests: Vec<_> = self.buffer.drain(..).collect();

        // 逐个执行写入
        for req in requests {
            if let Err(e) = self.execute_request(&mut tx, req).await {
                error!("Failed to execute persist request: {}", e);
                // 继续处理其他请求，不中断事务
            }
        }

        // 提交事务
        match tx.commit().await {
            Ok(_) => {
                info!("Flushed {} persist requests", batch_size);
            }
            Err(e) => {
                error!("Failed to commit transaction: {}", e);
            }
        }
    }

    /// 执行单个持久化请求
    async fn execute_request(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        req: PersistRequest,
    ) -> Result<(), sqlx::Error> {
        match req {
            PersistRequest::WriteFreezeSnapshot(snapshot) => {
                self.write_freeze_snapshot(tx, snapshot).await
            }

            PersistRequest::WriteEventRecord(record) => self.write_event_record(tx, record).await,

            PersistRequest::WriteLoadProfileRecord(record) => {
                self.write_load_profile_record(tx, record).await
            }

            PersistRequest::WriteEnergyRegister(register) => {
                self.write_energy_register(tx, register).await
            }

            PersistRequest::UpdateMaxDemand(entry) => self.update_max_demand(tx, entry).await,

            PersistRequest::WriteLoadProfileSample { address, sample } => {
                self.write_load_profile_sample_tx(tx, &address, &sample)
                    .await
            }

            PersistRequest::SaveVirtualTime {
                address,
                virtual_time,
                time_scale,
                simulation_config,
            } => self.save_virtual_time_tx(tx, &address, virtual_time, time_scale, &simulation_config).await,
        }
    }

    /// 写入冻结快照
    async fn write_freeze_snapshot(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        snapshot: FreezeSnapshotRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let snapshot_time_ms = snapshot.snapshot_time.timestamp_millis();
        let payload_json = serde_json::to_string(&snapshot.payload)
            .map_err(|e| sqlx::Error::Protocol(format!("JSON serialization error: {}", e)))?;

        sqlx::query(
            r#"
            INSERT INTO freeze_snapshots (
                address, trigger_type, category, occurrence_idx,
                snapshot_time_ms, payload_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(address, trigger_type, category, occurrence_idx)
            DO UPDATE SET
                snapshot_time_ms = excluded.snapshot_time_ms,
                payload_json = excluded.payload_json,
                created_at_ms = excluded.created_at_ms
            "#,
        )
        .bind(&snapshot.meter_address)
        .bind(snapshot.trigger_type)
        .bind(snapshot.category)
        .bind(snapshot.occurrence_idx)
        .bind(snapshot_time_ms)
        .bind(payload_json)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// 写入事件记录
    async fn write_event_record(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        record: EventRecordRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let start_time_ms = record.start_time.timestamp_millis();
        let end_time_ms = record.end_time.map(|t| t.timestamp_millis());
        let payload_json = serde_json::to_string(&record.payload)
            .map_err(|e| sqlx::Error::Protocol(format!("JSON serialization error: {}", e)))?;

        sqlx::query(
            r#"
            INSERT INTO event_records (
                address, event_kind, sub_kind, occurrence_idx,
                start_time_ms, end_time_ms, payload_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(address, event_kind, sub_kind, occurrence_idx)
            DO UPDATE SET
                start_time_ms = excluded.start_time_ms,
                end_time_ms = excluded.end_time_ms,
                payload_json = excluded.payload_json,
                created_at_ms = excluded.created_at_ms
            "#,
        )
        .bind(&record.meter_address)
        .bind(record.event_kind)
        .bind(record.sub_kind)
        .bind(record.occurrence_idx)
        .bind(start_time_ms)
        .bind(end_time_ms)
        .bind(payload_json)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// 写入负荷记录采样（支持LoadProfileSample）
    ///
    /// 按设计方案，负荷记录采样数据直接落库，不维护内存历史
    pub async fn write_load_profile_sample(
        pool: &SqlitePool,
        address: &str,
        sample: &crate::simulation::state::LoadProfileSample,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let sample_time_ms = sample.sample_time.timestamp_millis();

        // 将浮点值转换为定点数（*10000）以避免浮点精度问题
        let value_fp = (sample.value * 10000.0) as i64;

        sqlx::query(
            r#"
            INSERT INTO load_profile_samples (
                address, data_type, channel, sample_time_ms, value_fp, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(address)
        .bind(sample.data_type as u8)
        .bind(sample.channel)
        .bind(sample_time_ms)
        .bind(value_fp)
        .bind(now_ms)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 批量写入负荷记录采样
    pub async fn write_load_profile_samples_batch(
        pool: &SqlitePool,
        address: &str,
        samples: &[crate::simulation::state::LoadProfileSample],
    ) -> Result<(), sqlx::Error> {
        if samples.is_empty() {
            return Ok(());
        }

        let mut tx = pool.begin().await?;

        for sample in samples {
            let now_ms = Utc::now().timestamp_millis();
            let sample_time_ms = sample.sample_time.timestamp_millis();
            let value_fp = (sample.value * 10000.0) as i64;

            sqlx::query(
                r#"
                INSERT INTO load_profile_samples (
                    address, data_type, channel, sample_time_ms, value_fp, created_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(address)
            .bind(sample.data_type as u8)
            .bind(sample.channel)
            .bind(sample_time_ms)
            .bind(value_fp)
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// 在事务内写入单条负荷记录采样（load_profile_samples 表）
    async fn write_load_profile_sample_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        address: &str,
        sample: &crate::simulation::state::LoadProfileSample,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let sample_time_ms = sample.sample_time.timestamp_millis();
        // 定点数存储：真实值 * 10000，避免浮点精度问题
        let value_fp = (sample.value * 10000.0) as i64;

        sqlx::query(
            r#"
            INSERT INTO load_profile_samples (
                address, data_type, channel, sample_time_ms, value_fp, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(address)
        .bind(sample.data_type as u8)
        .bind(sample.channel)
        .bind(sample_time_ms)
        .bind(value_fp)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// 写入负荷记录（旧版本，兼容LoadProfileRecordRow）
    async fn write_load_profile_record(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        record: LoadProfileRecordRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let recorded_at_ms = record.recorded_at.timestamp_millis();
        let payload_json = serde_json::to_string(&record.payload)
            .map_err(|e| sqlx::Error::Protocol(format!("JSON serialization error: {}", e)))?;

        sqlx::query(
            r#"
            INSERT INTO load_profile_records (
                address, channel, data_type, recorded_at_ms, payload_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&record.meter_address)
        .bind(record.channel)
        .bind(record.data_type)
        .bind(recorded_at_ms)
        .bind(payload_json)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// 写入电能寄存器（批量转换为多条记录）
    async fn write_energy_register(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        register: EnergyRegisterRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let address = &register.meter_address;

        // 将 f64 转换为定点数（*100）
        let to_fp = |v: f64| (v * 100.0) as i64;

        // 批量写入所有电能寄存器
        // DI2编码：01=组合有功(正向), 02=组合有功(反向), 03=组合无功1, 04=组合无功2
        // DI1编码：00=总, 01-04=费率1-4
        // DI0编码：00=当前结算日

        let entries = vec![
            // 组合有功正向（01-00-00）
            (0x01, 0x00, to_fp(register.combined_active_positive)),
            // 组合有功反向（02-00-00）
            (0x02, 0x00, to_fp(register.combined_active_negative)),
            // 组合无功1（03-00-00）
            (0x03, 0x00, to_fp(register.combined_reactive_positive)),
            // 组合无功2（04-00-00）
            (0x04, 0x00, to_fp(register.combined_reactive_negative)),
            // 费率1有功（01-01-00）
            (0x01, 0x01, to_fp(register.rate1_active_positive)),
            // 费率2有功（01-02-00）
            (0x01, 0x02, to_fp(register.rate2_active_positive)),
            // 费率3有功（01-03-00）
            (0x01, 0x03, to_fp(register.rate3_active_positive)),
            // 费率4有功（01-04-00）
            (0x01, 0x04, to_fp(register.rate4_active_positive)),
        ];

        for (energy_kind, rate_index, value_fp) in entries {
            sqlx::query(
                r#"
                INSERT INTO energy_registers (
                    address, energy_kind, rate_index, settlement_day, value_fp, updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(address, energy_kind, rate_index, settlement_day)
                DO UPDATE SET
                    value_fp = excluded.value_fp,
                    updated_at_ms = excluded.updated_at_ms
                "#,
            )
            .bind(address)
            .bind(energy_kind)
            .bind(rate_index)
            .bind(0x00) // settlement_day=00（当前结算日）
            .bind(value_fp)
            .bind(now_ms)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    /// 更新最大需量
    async fn update_max_demand(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        entry: MaxDemandRow,
    ) -> Result<(), sqlx::Error> {
        let occurred_at_ms = entry.occurred_at.timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO max_demand (
                address, demand_kind, rate_index, value_fp, occurred_at_ms
            ) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(address, demand_kind, rate_index)
            DO UPDATE SET
                value_fp = excluded.value_fp,
                occurred_at_ms = excluded.occurred_at_ms
            "#,
        )
        .bind(&entry.meter_address)
        .bind(entry.demand_kind)
        .bind(entry.rate_index)
        .bind(entry.value_fp)
        .bind(occurred_at_ms)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// 在事务内保存虚拟时间和配置（用于定期持久化）
    /// 
    /// 此函数会更新所有仿真相关的配置参数，但不会覆盖协议相关的配置（如冻结模式、负荷记录模式等）
    async fn save_virtual_time_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        address: &str,
        virtual_time: chrono::DateTime<chrono::Local>,
        time_scale: f64,
        simulation_config: &crate::simulation::SimulationConfig,
    ) -> Result<(), sqlx::Error> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let virtual_time_ms = virtual_time.timestamp_millis();

        // 转换配置参数
        let meter_constant = simulation_config.meter_constant as i64;
        let rated_voltage_mv = (simulation_config.rated_voltage * 1000.0).round() as i64;
        let rated_current_ma = (simulation_config.rated_current * 1000.0).round() as i64;
        let demand_period_min = simulation_config.demand_period_minutes as i64;
        let rated_frequency_hz = simulation_config.rated_frequency;
        let initial_power_factor = simulation_config.power_factor;

        // 序列化 load_model
        let profile_val = match simulation_config.load_model.profile {
            crate::simulation::physics_engine::LoadProfile::Residential => {
                serde_json::Value::String("Residential".into())
            }
            crate::simulation::physics_engine::LoadProfile::Industrial => {
                serde_json::Value::String("Industrial".into())
            }
            crate::simulation::physics_engine::LoadProfile::Commercial => {
                serde_json::Value::String("Commercial".into())
            }
            crate::simulation::physics_engine::LoadProfile::Fixed(f) => serde_json::json!({"Fixed": f}),
        };

        let load_model_json = serde_json::json!({
            "profile": profile_val,
            "voltage_noise_v": simulation_config.load_model.voltage_noise_v,
            "frequency_noise_hz": simulation_config.load_model.frequency_noise_hz,
            "power_factor_noise": simulation_config.load_model.power_factor_noise,
            "power_factor_min": simulation_config.load_model.power_factor_min,
            "power_factor_max": simulation_config.load_model.power_factor_max,
            "phase_current_factors": simulation_config.load_model.phase_current_factors,
        })
        .to_string();

        // 尝试更新现有记录
        let result = sqlx::query(
            r#"
            UPDATE meters
            SET meter_constant = ?,
                rated_voltage_mv = ?,
                rated_current_ma = ?,
                demand_period_min = ?,
                load_model_json = ?,
                virtual_time_ms = ?,
                time_scale = ?,
                rated_frequency_hz = ?,
                initial_power_factor = ?,
                updated_at_ms = ?
            WHERE address = ?
            "#
        )
        .bind(meter_constant)
        .bind(rated_voltage_mv)
        .bind(rated_current_ma)
        .bind(demand_period_min)
        .bind(&load_model_json)
        .bind(virtual_time_ms)
        .bind(time_scale)
        .bind(rated_frequency_hz)
        .bind(initial_power_factor)
        .bind(now_ms)
        .bind(address)
        .execute(&mut **tx)
        .await?;

        // 如果记录不存在，插入新记录（使用默认值）
        if result.rows_affected() == 0 {
            sqlx::query(
                r#"
                INSERT INTO meters (
                    address, meter_constant, rated_voltage_mv, rated_current_ma,
                    demand_period_min, sliding_window_min, freeze_mode_word, load_record_mode_word,
                    settlement_days_json, tou_config_json, passwords_json, comm_baud_json, load_model_json,
                    virtual_time_ms, time_scale, rated_frequency_hz, initial_power_factor, updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#
            )
            .bind(address)
            .bind(meter_constant)
            .bind(rated_voltage_mv)
            .bind(rated_current_ma)
            .bind(demand_period_min)
            .bind(0) // sliding_window_min - 默认值
            .bind(0) // freeze_mode_word - 默认值
            .bind(0) // load_record_mode_word - 默认值
            .bind("[]") // settlement_days_json - 默认空数组
            .bind("{}") // tou_config_json - 默认空对象
            .bind("{}") // passwords_json - 默认空对象
            .bind("{}") // comm_baud_json - 默认空对象
            .bind(load_model_json)
            .bind(virtual_time_ms)
            .bind(time_scale)
            .bind(rated_frequency_hz)
            .bind(initial_power_factor)
            .bind(now_ms)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    /// 查询冻结快照（供 DIHandler 读取使用）
    pub async fn query_freeze_snapshot(
        pool: &SqlitePool,
        address: &str,
        trigger_type: u8,
        category: u8,
        occurrence_idx: u8,
    ) -> Result<Option<FreezeSnapshotRow>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT trigger_type, category, occurrence_idx, snapshot_time_ms, payload_json
            FROM freeze_snapshots
            WHERE address = ? AND trigger_type = ? AND category = ? AND occurrence_idx = ?
            "#,
        )
        .bind(address)
        .bind(trigger_type)
        .bind(category)
        .bind(occurrence_idx)
        .fetch_optional(pool)
        .await?;

        if let Some(row) = row {
            let snapshot_time_ms: i64 = row.get("snapshot_time_ms");
            let payload_json: String = row.get("payload_json");

            let snapshot_time = chrono::DateTime::from_timestamp_millis(snapshot_time_ms)
                .ok_or_else(|| sqlx::Error::Protocol("Invalid timestamp".to_string()))?
                .with_timezone(&chrono::Local);

            let payload: serde_json::Value = serde_json::from_str(&payload_json)
                .map_err(|e| sqlx::Error::Protocol(format!("JSON parse error: {}", e)))?;

            Ok(Some(FreezeSnapshotRow {
                meter_address: address.to_string(),
                trigger_type: row.get("trigger_type"),
                category: row.get("category"),
                occurrence_idx: row.get("occurrence_idx"),
                snapshot_time,
                payload,
            }))
        } else {
            Ok(None)
        }
    }

    /// 查询负荷记录采样数据（供 DIHandler 读取使用）
    ///
    /// 参数：
    /// - pool: 数据库连接池
    /// - address: 电表地址
    /// - data_type: 数据类型（01=电压，02=电流等）
    /// - channel: 通道（00=总，01=A相，02=B相，03=C相）
    /// - start_time: 起始时间
    /// - end_time: 结束时间
    /// - max_records: 最大返回记录数
    ///
    /// 返回：按时间升序排列的采样记录列表
    pub async fn query_load_profile_samples(
        pool: &SqlitePool,
        address: &str,
        data_type: u8,
        channel: u8,
        start_time: &chrono::DateTime<chrono::Local>,
        end_time: &chrono::DateTime<chrono::Local>,
        max_records: u32,
    ) -> Result<Vec<LoadProfileSampleRow>, sqlx::Error> {
        let start_ms = start_time.timestamp_millis();
        let end_ms = end_time.timestamp_millis();

        let rows = sqlx::query(
            r#"
            SELECT sample_time_ms, data_type, channel, value_fp
            FROM load_profile_samples
            WHERE address = ? 
              AND data_type = ? 
              AND channel = ?
              AND sample_time_ms >= ?
              AND sample_time_ms <= ?
            ORDER BY sample_time_ms ASC
            LIMIT ?
            "#,
        )
        .bind(address)
        .bind(data_type)
        .bind(channel)
        .bind(start_ms)
        .bind(end_ms)
        .bind(max_records as i64)
        .fetch_all(pool)
        .await?;

        let mut samples = Vec::new();

        for row in rows {
            let sample_time_ms: i64 = row.get("sample_time_ms");
            let sample_time = chrono::DateTime::from_timestamp_millis(sample_time_ms)
                .ok_or_else(|| sqlx::Error::Protocol("Invalid timestamp".to_string()))?
                .with_timezone(&chrono::Local);

            let value_fp: i64 = row.get("value_fp");
            let value = value_fp as f64 / 10000.0; // 恢复浮点值（除以10000）

            samples.push(LoadProfileSampleRow {
                meter_address: address.to_string(),
                sample_time,
                data_type: row.get("data_type"),
                channel: row.get("channel"),
                value,
            });
        }

        Ok(samples)
    }

    /// 从数据库恢复电能寄存器
    ///
    /// 读取指定地址的所有电能寄存器值，返回HashMap供MeterState使用
    pub async fn restore_energy_registers(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<std::collections::HashMap<(u8, u8), f64>, sqlx::Error> {
        use std::collections::HashMap;

        let rows = sqlx::query(
            r#"
            SELECT energy_kind, rate_index, value_fp
            FROM energy_registers
            WHERE address = ? AND settlement_day = 0
            "#,
        )
        .bind(address)
        .fetch_all(pool)
        .await?;

        let mut registers = HashMap::new();

        for row in rows {
            let energy_kind: u8 = row.get::<i64, _>("energy_kind") as u8;
            let rate_index: u8 = row.get::<i64, _>("rate_index") as u8;
            let value_fp: i64 = row.get("value_fp");

            // 将定点数转换回浮点数（除以100）
            let value = value_fp as f64 / 100.0;

            registers.insert((energy_kind, rate_index), value);
        }

        Ok(registers)
    }

    /// 从数据库恢复虚拟时钟
    ///
    /// 读取meters表中保存的virtual_time_ms
    pub async fn restore_virtual_time(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Local>>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT virtual_time_ms
            FROM meters
            WHERE address = ?
            "#,
        )
        .bind(address)
        .fetch_optional(pool)
        .await?;

        if let Some(row) = row {
            let virtual_time_ms: i64 = row.get("virtual_time_ms");

            let dt = chrono::DateTime::from_timestamp_millis(virtual_time_ms)
                .ok_or_else(|| sqlx::Error::Protocol("Invalid timestamp".to_string()))?
                .with_timezone(&chrono::Local);

            Ok(Some(dt))
        } else {
            Ok(None)
        }
    }

    /// 保存虚拟时间和时间倍率
    ///
    /// 用于快速保存虚拟时钟状态，不更新其他配置字段
    /// 如需更新完整的仿真配置，请使用 save_simulation_config
    pub async fn save_virtual_time(
        pool: &SqlitePool,
        address: &str,
        virtual_time: chrono::DateTime<chrono::Local>,
        time_scale: f64,
    ) -> Result<(), sqlx::Error> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let virtual_time_ms = virtual_time.timestamp_millis();

        // 只更新虚拟时间和时间倍率，不覆盖其他配置字段
        sqlx::query(
            r#"
            UPDATE meters
            SET virtual_time_ms = ?,
                time_scale = ?,
                updated_at_ms = ?
            WHERE address = ?
            "#
        )
        .bind(virtual_time_ms)
        .bind(time_scale)
        .bind(now_ms)
        .bind(address)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 确保 `meters` 表包含协议参数字段（兼容历史库）
    async fn ensure_meter_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let rows = sqlx::query("PRAGMA table_info(meters)")
            .fetch_all(pool)
            .await?;

        let mut names = std::collections::HashSet::new();
        for row in rows {
            let name: String = row.get("name");
            names.insert(name);
        }

        for (column, default) in [("rated_frequency_hz", "50.0"), ("initial_power_factor", "0.95")] {
            if !names.contains(column) {
                let sql = format!(
                    "ALTER TABLE meters ADD COLUMN {} REAL NOT NULL DEFAULT {}",
                    column, default
                );
                sqlx::query(&sql).execute(pool).await?;
            }
        }

        Ok(())
    }

    /// 确保 `meters` 表存在该地址的默认行（INSERT OR IGNORE）
    /// 
    /// 使用的默认值：
    /// - meter_constant: 1600 (脉冲常数)
    /// - rated_voltage: 220V
    /// - rated_current: 5A
    /// - demand_period: 15min
    /// - rated_frequency: 50Hz
    /// - initial_power_factor: 0.95
    /// 
    /// 注意：这些默认值仅在首次创建记录时使用，后续会通过 save_simulation_config 等函数更新
    async fn ensure_meter_row(pool: &SqlitePool, address: &str) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO meters (
                address, meter_constant, rated_voltage_mv, rated_current_ma,
                demand_period_min, sliding_window_min, freeze_mode_word, load_record_mode_word,
                settlement_days_json, tou_config_json, passwords_json, comm_baud_json, load_model_json,
                virtual_time_ms, time_scale, rated_frequency_hz, initial_power_factor, updated_at_ms
            ) VALUES (?, 1600, 220000, 5000, 15, 0, 0, 0, '[]', '{}', '{}', '{}', '{}', 0, 1.0, 50.0, 0.95, ?)
            "#,
        )
        .bind(address)
        .bind(now_ms)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 读取并规范化 `tou_config_json`（兼容旧版整段 freeze JSON）
    async fn read_extra_config(pool: &SqlitePool, address: &str) -> Result<Value, sqlx::Error> {
        let row = sqlx::query("SELECT tou_config_json FROM meters WHERE address = ?")
            .bind(address)
            .fetch_optional(pool)
            .await?;

        let mut root = row
            .map(|row| {
                let raw: String = row.get("tou_config_json");
                serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
            })
            .unwrap_or_else(|| json!({}));

        if root.get("freeze").is_none() && root.get("timed_mode").is_some() {
            let legacy = std::mem::take(&mut root);
            root = json!({ "freeze": legacy });
        }

        Ok(root)
    }

    fn parse_load_profile(value: &Value) -> crate::simulation::physics_engine::LoadProfile {
        use crate::simulation::physics_engine::LoadProfile;
        match value {
            Value::String(name) => match name.as_str() {
                "Industrial" => LoadProfile::Industrial,
                "Commercial" => LoadProfile::Commercial,
                "Fixed" => LoadProfile::Fixed(0.5),
                _ => LoadProfile::Residential,
            },
            Value::Object(map) if map.contains_key("Fixed") => {
                let factor = map
                    .get("Fixed")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
                LoadProfile::Fixed(factor)
            }
            _ => LoadProfile::Residential,
        }
    }

    fn load_model_from_json(raw: &str) -> crate::simulation::physics_engine::LoadModelConfig {
        use crate::simulation::physics_engine::LoadModelConfig;
        let v: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
        let default = LoadModelConfig::default();
        let factors: Vec<f64> = v
            .get("phase_current_factors")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_f64())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut phase = default.phase_current_factors;
        for (i, value) in factors.into_iter().take(3).enumerate() {
            phase[i] = value;
        }
        LoadModelConfig {
            profile: v
                .get("profile")
                .map(Self::parse_load_profile)
                .unwrap_or(default.profile),
            voltage_noise_v: v
                .get("voltage_noise_v")
                .and_then(|x| x.as_f64())
                .unwrap_or(default.voltage_noise_v),
            frequency_noise_hz: v
                .get("frequency_noise_hz")
                .and_then(|x| x.as_f64())
                .unwrap_or(default.frequency_noise_hz),
            power_factor_noise: v
                .get("power_factor_noise")
                .and_then(|x| x.as_f64())
                .unwrap_or(default.power_factor_noise),
            power_factor_min: v
                .get("power_factor_min")
                .and_then(|x| x.as_f64())
                .unwrap_or(default.power_factor_min),
            power_factor_max: v
                .get("power_factor_max")
                .and_then(|x| x.as_f64())
                .unwrap_or(default.power_factor_max),
            phase_current_factors: phase,
        }
    }

    fn simulation_from_row(
        meter_constant: i64,
        rated_voltage_mv: i64,
        rated_current_ma: i64,
        demand_period_min: i64,
        time_scale: f64,
        rated_frequency_hz: f64,
        initial_power_factor: f64,
        load_model_json: &str,
    ) -> crate::simulation::SimulationConfig {
        use crate::simulation::SimulationConfig;
        let load_model = Self::load_model_from_json(load_model_json);
        SimulationConfig {
            load_model,
            rated_voltage: rated_voltage_mv as f64 / 1000.0,
            rated_current: rated_current_ma as f64 / 1000.0,
            rated_frequency: rated_frequency_hz,
            power_factor: initial_power_factor,
            meter_constant: meter_constant.max(1) as u32,
            demand_period_minutes: demand_period_min.clamp(1, 120) as u16,
            time_scale,
        }
    }

    fn freeze_fields_from_json(freeze: &Value) -> (
        u8,
        u8,
        u8,
        u8,
        u8,
        [u8; 2],
        [u8; 5],
        u8,
        [u8; 5],
    ) {
        let u8_at = |key: &str| freeze.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        let arr2 = |key: &str| -> [u8; 2] {
            let a = freeze
                .get(key)
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u8))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            [a.first().copied().unwrap_or(0), a.get(1).copied().unwrap_or(0)]
        };
        let arr5 = |key: &str| -> [u8; 5] {
            let a = freeze
                .get(key)
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u8))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            [
                a.first().copied().unwrap_or(0),
                a.get(1).copied().unwrap_or(0),
                a.get(2).copied().unwrap_or(0),
                a.get(3).copied().unwrap_or(0),
                a.get(4).copied().unwrap_or(0),
            ]
        };
        (
            u8_at("timed_mode"),
            u8_at("instant_mode"),
            u8_at("appointment_mode"),
            u8_at("hourly_mode"),
            u8_at("daily_mode"),
            arr2("daily_time"),
            arr5("hourly_start"),
            u8_at("hourly_interval_min"),
            arr5("appointment_time"),
        )
    }

    /// 保存或更新仿真/协议相关的表参量
    pub async fn save_simulation_config(
        pool: &SqlitePool,
        address: &str,
        sim: &crate::simulation::SimulationConfig,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();

        let profile_val = match sim.load_model.profile {
            crate::simulation::physics_engine::LoadProfile::Residential => {
                Value::String("Residential".into())
            }
            crate::simulation::physics_engine::LoadProfile::Industrial => {
                Value::String("Industrial".into())
            }
            crate::simulation::physics_engine::LoadProfile::Commercial => {
                Value::String("Commercial".into())
            }
            crate::simulation::physics_engine::LoadProfile::Fixed(f) => json!({"Fixed": f}),
        };

        let load_model_json = json!({
            "profile": profile_val,
            "voltage_noise_v": sim.load_model.voltage_noise_v,
            "frequency_noise_hz": sim.load_model.frequency_noise_hz,
            "power_factor_noise": sim.load_model.power_factor_noise,
            "power_factor_min": sim.load_model.power_factor_min,
            "power_factor_max": sim.load_model.power_factor_max,
            "phase_current_factors": sim.load_model.phase_current_factors,
        })
        .to_string();

        let rated_voltage_mv = (sim.rated_voltage * 1000.0).round() as i64;
        let rated_current_ma = (sim.rated_current * 1000.0).round() as i64;

        Self::ensure_meter_row(pool, address).await?;
        sqlx::query(
            r#"
            UPDATE meters SET
                meter_constant = ?,
                rated_voltage_mv = ?,
                rated_current_ma = ?,
                demand_period_min = ?,
                rated_frequency_hz = ?,
                initial_power_factor = ?,
                load_model_json = ?,
                time_scale = ?,
                updated_at_ms = ?
            WHERE address = ?
            "#,
        )
        .bind(sim.meter_constant as i64)
        .bind(rated_voltage_mv)
        .bind(rated_current_ma)
        .bind(sim.demand_period_minutes as i64)
        .bind(sim.rated_frequency)
        .bind(sim.power_factor)
        .bind(load_model_json)
        .bind(sim.time_scale)
        .bind(now_ms)
        .bind(address)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 保存或更新结算日到 `meters.settlement_days_json`
    pub async fn save_settlement_days(
        pool: &SqlitePool,
        address: &str,
        days: [u8; 3],
        hours: [u8; 3],
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let settlement_json = json!({"days": days, "hours": hours}).to_string();
        Self::ensure_meter_row(pool, address).await?;
        sqlx::query(
            "UPDATE meters SET settlement_days_json = ?, updated_at_ms = ? WHERE address = ?",
        )
        .bind(settlement_json)
        .bind(now_ms)
        .bind(address)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 保存或更新负荷记录配置（模式字 + `tou_config_json.load_record`）
    pub async fn save_load_record_config(
        pool: &SqlitePool,
        address: &str,
        mode_word: u8,
        start_time: [u8; 4],
        intervals: [u16; 6],
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let mut extra = Self::read_extra_config(pool, address).await?;
        extra["load_record"] = json!({
            "start_time": start_time,
            "intervals": intervals,
        });
        Self::ensure_meter_row(pool, address).await?;
        sqlx::query(
            r#"
            UPDATE meters
            SET load_record_mode_word = ?, tou_config_json = ?, updated_at_ms = ?
            WHERE address = ?
            "#,
        )
        .bind(mode_word as i64)
        .bind(extra.to_string())
        .bind(now_ms)
        .bind(address)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 保存或更新冻结配置（`freeze_mode_word` + `tou_config_json.freeze`）
    pub async fn save_freeze_config(
        pool: &SqlitePool,
        address: &str,
        timed_mode: u8,
        instant_mode: u8,
        appointment_mode: u8,
        hourly_mode: u8,
        daily_mode: u8,
        daily_time: [u8; 2],
        hourly_start: [u8; 5],
        hourly_interval_min: u8,
        appointment_time: [u8; 5],
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let mut extra = Self::read_extra_config(pool, address).await?;
        extra["freeze"] = json!({
            "timed_mode": timed_mode,
            "instant_mode": instant_mode,
            "appointment_mode": appointment_mode,
            "hourly_mode": hourly_mode,
            "daily_mode": daily_mode,
            "daily_time": daily_time,
            "hourly_start": hourly_start,
            "hourly_interval_min": hourly_interval_min,
            "appointment_time": appointment_time,
        });
        Self::ensure_meter_row(pool, address).await?;
        sqlx::query(
            r#"
            UPDATE meters
            SET freeze_mode_word = ?, tou_config_json = ?, updated_at_ms = ?
            WHERE address = ?
            "#,
        )
        .bind(timed_mode as i64)
        .bind(extra.to_string())
        .bind(now_ms)
        .bind(address)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 从 `meters` 表恢复一表完整配置
    pub async fn restore_meter_config(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<Option<PersistedMeterSettings>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT meter_constant, rated_voltage_mv, rated_current_ma, demand_period_min,
                   time_scale, rated_frequency_hz, initial_power_factor,
                   load_model_json, settlement_days_json, tou_config_json,
                   freeze_mode_word, load_record_mode_word
            FROM meters
            WHERE address = ?
            "#,
        )
        .bind(address)
        .fetch_optional(pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let load_model_json: String = row.get("load_model_json");
        let rated_frequency_hz: f64 = row.get("rated_frequency_hz");
        let initial_power_factor: f64 = row.get("initial_power_factor");
        let simulation = Self::simulation_from_row(
            row.get("meter_constant"),
            row.get("rated_voltage_mv"),
            row.get("rated_current_ma"),
            row.get("demand_period_min"),
            row.get("time_scale"),
            rated_frequency_hz,
            initial_power_factor,
            &load_model_json,
        );

        let extra: Value = {
            let raw: String = row.get("tou_config_json");
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        };
        let extra = if extra.get("freeze").is_none() && extra.get("timed_mode").is_some() {
            json!({ "freeze": extra })
        } else {
            extra
        };
        let freeze = extra.get("freeze").cloned().unwrap_or_else(|| json!({}));
        let (
            timed_mode,
            instant_mode,
            appointment_mode,
            hourly_mode,
            daily_mode,
            daily_time,
            hourly_start,
            hourly_interval_min,
            appointment_time,
        ) = Self::freeze_fields_from_json(&freeze);

        let settlement: Value = {
            let raw: String = row.get("settlement_days_json");
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({"days": [0, 0, 0], "hours": [0, 0, 0]}))
        };
        let mut settlement_days = [0u8; 3];
        let mut settlement_hours = [0u8; 3];
        if let Some(days) = settlement.get("days").and_then(|v| v.as_array()) {
            for (i, value) in days.iter().take(3).enumerate() {
                settlement_days[i] = value.as_u64().unwrap_or(0) as u8;
            }
        }
        if let Some(hours) = settlement.get("hours").and_then(|v| v.as_array()) {
            for (i, value) in hours.iter().take(3).enumerate() {
                settlement_hours[i] = value.as_u64().unwrap_or(0) as u8;
            }
        }

        let load_record = extra.get("load_record").cloned().unwrap_or_else(|| json!({}));
        let load_record_mode_word: i64 = row.get("load_record_mode_word");
        let mut load_record_start_time = [0u8; 4];
        if let Some(st) = load_record.get("start_time").and_then(|v| v.as_array()) {
            for (i, value) in st.iter().take(4).enumerate() {
                load_record_start_time[i] = value.as_u64().unwrap_or(0) as u8;
            }
        }
        let mut load_record_intervals = [0u16; 6];
        if let Some(intervals) = load_record.get("intervals").and_then(|v| v.as_array()) {
            for (i, value) in intervals.iter().take(6).enumerate() {
                load_record_intervals[i] = value.as_u64().unwrap_or(0) as u16;
            }
        }

        Ok(Some(PersistedMeterSettings {
            simulation,
            timed_freeze_mode: timed_mode,
            instant_freeze_mode: instant_mode,
            appointment_freeze_mode: appointment_mode,
            hourly_freeze_mode: hourly_mode,
            daily_freeze_mode: daily_mode,
            daily_freeze_time: daily_time,
            hourly_freeze_start: hourly_start,
            hourly_freeze_interval_min: hourly_interval_min,
            appointment_freeze_time: appointment_time,
            settlement_days,
            settlement_hours,
            load_record_mode_word: load_record_mode_word as u8,
            load_record_start_time,
            load_record_intervals,
        }))
    }

}