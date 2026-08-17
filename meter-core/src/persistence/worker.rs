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
    /// 创建新的持久化工作器
    pub async fn new(
        config: PersistenceConfig,
        rx: mpsc::Receiver<PersistRequest>,
    ) -> Result<Self, sqlx::Error> {
        // 确保数据目录存在
        if let Some(parent) = Path::new(&config.db_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }

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

        info!("PersistenceWorker initialized: db_path={}", config.db_path);

        let batch_max_size = config.batch_max_size; // 保存值，避免移动后使用

        Ok(Self {
            pool,
            config,
            rx,
            buffer: Vec::with_capacity(batch_max_size),
        })
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

    /// 保存或更新meters表记录
    ///
    /// 用于保存虚拟时钟和配置参数
    pub async fn save_meter_state(
        pool: &SqlitePool,
        address: &str,
        virtual_time: chrono::DateTime<chrono::Local>,
        time_scale: f64,
    ) -> Result<(), sqlx::Error> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let virtual_time_ms = virtual_time.timestamp_millis();

        // 简化版本：只保存最小必要字段，其他字段使用默认值
        sqlx::query(
            r#"
            INSERT INTO meters (
                address, meter_constant, rated_voltage_mv, rated_current_ma,
                demand_period_min, sliding_window_min, freeze_mode_word, load_record_mode_word,
                settlement_days_json, tou_config_json, passwords_json, comm_baud_json, load_model_json,
                virtual_time_ms, time_scale, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(address)
            DO UPDATE SET
                virtual_time_ms = excluded.virtual_time_ms,
                time_scale = excluded.time_scale,
                updated_at_ms = excluded.updated_at_ms
            "#
        )
        .bind(address)
        .bind(1600)  // meter_constant: 1600 imp/kWh (默认值)
        .bind(220000)  // rated_voltage_mv: 220V
        .bind(5000)    // rated_current_ma: 5A
        .bind(15)      // demand_period_min: 15分钟
        .bind(0)       // sliding_window_min: 0（无滑差）
        .bind(0)       // freeze_mode_word: 默认不启用
        .bind(0)       // load_record_mode_word: 默认不启用
        .bind("[]")    // settlement_days_json
        .bind("{}")    // tou_config_json
        .bind("{}")    // passwords_json
        .bind("{}")    // comm_baud_json
        .bind("{}")    // load_model_json
        .bind(virtual_time_ms)
        .bind(time_scale)
        .bind(now_ms)
        .execute(pool)
        .await?;

        Ok(())
    }
}
