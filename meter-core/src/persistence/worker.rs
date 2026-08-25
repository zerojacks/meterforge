// PersistenceWorker - 持久化任务
//
// 设计说明（按设计方案第11节）：
// - 单一消费者：所有 MeterActor 共用一个 mpsc::Sender<PersistRequest>
// - 批量写入：攒够 batch_max_size 条或超时即开事务批量执行
// - WAL 模式：减少写锁竞争
// - 非阻塞：Actor 的 tick 循环不被磁盘 I/O 阻塞

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info};

use super::types::*;
use serde_json::{json, Value};

/// 持久化配置
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    pub db_path: String,
    pub batch_max_size: usize,
    pub batch_timeout_ms: u64,
    pub max_connections: u32,
    /// `load_profile_records` 每个 (address, class_id) 序列最多保留的行数——
    /// 不同于冻结快照按协议 DI0 语义逐条挪号，负荷记录每类各自独立采样间隔，
    /// 快的可能每分钟一条 × 每表最多 6 类，必须有容量上限才不会无限增长。
    /// 超出部分由 `load_profile_cleanup_interval_secs` 定期清理。
    pub load_profile_max_records_per_class: u32,
    /// 负荷记录清理任务的运行间隔（秒）
    pub load_profile_cleanup_interval_secs: u64,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            db_path: "./data/meters.db".to_string(),
            batch_max_size: 200,
            batch_timeout_ms: 1000, // 1秒超时
            max_connections: 4,
            load_profile_max_records_per_class: 2000,
            load_profile_cleanup_interval_secs: 600, // 10 分钟
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
        let mut load_profile_cleanup_interval = interval(Duration::from_secs(
            self.config.load_profile_cleanup_interval_secs,
        ));
        // 第一次 tick 立即触发，跳过它，避免启动瞬间就跑一次清理（此时数据量还很小）
        load_profile_cleanup_interval.tick().await;

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

                // 定期清理超出容量上限的负荷记录（见 load_profile_max_records_per_class）
                _ = load_profile_cleanup_interval.tick() => {
                    if let Err(e) = self.cleanup_load_profile_records().await {
                        error!("负荷记录清理失败: {}", e);
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

    /// 清理超出 `load_profile_max_records_per_class` 容量的负荷记录
    ///
    /// 与冻结快照按协议 DI0 语义逐条挪号不同，负荷记录没有对应的协议
    /// 语义容量（协议只规定了采样间隔，没规定保留多少条），这里用
    /// `ROW_NUMBER() OVER (PARTITION BY address, class_id ORDER BY
    /// sample_time_ms DESC)` 给每个 (表, 类别) 序列内部按新到旧编号，删掉
    /// 超出容量的部分——效果上相当于每个序列各自维护一个固定大小的"环形
    /// 缓冲"，但用一条 DELETE 语句覆盖全部电表、全部类别，不用按地址/类别
    /// 逐个查询。`load_profile_records` 是 `WITHOUT ROWID` 表、主键是
    /// `(address, class_id, sample_time_ms)` 复合列，没有单独的 `id` 列，
    /// 所以用行值（row value）比较 `(address, class_id, sample_time_ms) IN
    /// (...)` 而不是按 `id` 过滤。
    async fn cleanup_load_profile_records(&self) -> Result<(), sqlx::Error> {
        let capacity = self.config.load_profile_max_records_per_class as i64;

        let result = sqlx::query(
            r#"
            DELETE FROM load_profile_records
            WHERE (address, class_id, sample_time_ms) IN (
                SELECT address, class_id, sample_time_ms FROM (
                    SELECT address, class_id, sample_time_ms, ROW_NUMBER() OVER (
                        PARTITION BY address, class_id
                        ORDER BY sample_time_ms DESC
                    ) AS rn
                    FROM load_profile_records
                )
                WHERE rn > ?
            )
            "#,
        )
        .bind(capacity)
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!("负荷记录清理：删除 {} 条超出容量的历史记录", deleted);
        }

        Ok(())
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

            PersistRequest::WriteEnergyRegister(register) => {
                self.write_energy_register(tx, register).await
            }

            PersistRequest::UpdateMaxDemand(entry) => self.update_max_demand(tx, entry).await,

            PersistRequest::WriteLoadRecord(row) => {
                self.write_load_record_tx(tx, row).await
            }

            PersistRequest::WriteSettlementEnergies(row) => {
                self.write_settlement_energies(tx, row).await
            }

            PersistRequest::SaveVirtualTime {
                address,
                virtual_time,
                synced_at_ms,
                time_scale,
                simulation_config,
            } => {
                self.save_virtual_time_tx(
                    tx,
                    &address,
                    virtual_time,
                    synced_at_ms,
                    time_scale,
                    &simulation_config,
                )
                .await
            }
        }
    }

    /// 写入冻结快照
    ///
    /// 按协议 A.6 节语义：DI0=01 恒为"最近一次"，02 为"上一次"……每来一次新
    /// 冻结，已有记录整体挪号 +1，超出该触发类型容量（`FreezeTrigger::max_history_count()`，
    /// 与内存环形缓冲一致：定时12/瞬时3/切换类3/整点62/日62）的最旧记录被丢弃，
    /// 新快照落在 01。
    ///
    /// 挪号分两步（先挪到负数区间再翻正）：SQLite 对同一条 UPDATE 语句里
    /// 批量改写主键列不保证行内写入顺序，若直接 `occurrence_idx = occurrence_idx + 1`
    /// 升序写入，会在语句执行期间与尚未挪走的旧值发生 `(address, trigger_type,
    /// category, occurrence_idx)` 唯一约束瞬时冲突；先移到不可能重叠的负数区间
    /// 暂存，再统一翻正，可以安全避开这个问题。
    async fn write_freeze_snapshot(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        snapshot: FreezeSnapshotRow,
    ) -> Result<(), sqlx::Error> {
        use crate::simulation::FreezeTrigger;

        let now_ms = Utc::now().timestamp_millis();
        let snapshot_time_ms = snapshot.snapshot_time.timestamp_millis();
        let payload_json = serde_json::to_string(&snapshot.payload)
            .map_err(|e| sqlx::Error::Protocol(format!("JSON serialization error: {}", e)))?;

        let capacity = FreezeTrigger::from_di2(snapshot.trigger_type)
            .map(|t| t.max_history_count())
            .unwrap_or(12) as i64;

        // 1. 丢弃将被挤出协议 DI0 范围的最旧记录（挪号前 occurrence_idx == capacity，
        //    挪号后会变成 capacity+1，超出范围）
        sqlx::query(
            r#"
            DELETE FROM freeze_snapshots
            WHERE address = ? AND trigger_type = ? AND category = ?
              AND occurrence_idx >= ?
            "#,
        )
        .bind(&snapshot.meter_address)
        .bind(snapshot.trigger_type)
        .bind(snapshot.category)
        .bind(capacity)
        .execute(&mut **tx)
        .await?;

        // 2. 现存记录整体 occurrence_idx += 1（先负数暂存，避开唯一约束瞬时冲突）
        sqlx::query(
            r#"
            UPDATE freeze_snapshots
            SET occurrence_idx = -(occurrence_idx + 1)
            WHERE address = ? AND trigger_type = ? AND category = ?
            "#,
        )
        .bind(&snapshot.meter_address)
        .bind(snapshot.trigger_type)
        .bind(snapshot.category)
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE freeze_snapshots
            SET occurrence_idx = -occurrence_idx
            WHERE address = ? AND trigger_type = ? AND category = ? AND occurrence_idx < 0
            "#,
        )
        .bind(&snapshot.meter_address)
        .bind(snapshot.trigger_type)
        .bind(snapshot.category)
        .execute(&mut **tx)
        .await?;

        // 3. 新快照落在 01（协议语义：本次即"最近一次"）
        sqlx::query(
            r#"
            INSERT INTO freeze_snapshots (
                address, trigger_type, category, occurrence_idx,
                snapshot_time_ms, payload_json, created_at_ms
            ) VALUES (?, ?, ?, 1, ?, ?, ?)
            "#,
        )
        .bind(&snapshot.meter_address)
        .bind(snapshot.trigger_type)
        .bind(snapshot.category)
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

    /// 写入负荷记录（load_profile_records 表，JSON payload）
    ///
    /// 设计说明：
    /// - 主键 (address, class_id, sample_time_ms) 天然防重复采样
    /// - 主键冲突时使用 ON CONFLICT IGNORE，保留第一次采样值
    async fn write_load_record_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        row: LoadRecordRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let sample_time_ms = row.sample_time.timestamp_millis();
        let payload_json = serde_json::to_string(&row.payload)
            .map_err(|e| sqlx::Error::Protocol(format!("JSON serialization error: {}", e)))?;

        sqlx::query(
            r#"
            INSERT INTO load_profile_records (
                address, class_id, sample_time_ms, payload_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (address, class_id, sample_time_ms) DO NOTHING
            "#,
        )
        .bind(&row.meter_address)
        .bind(row.class_id)
        .bind(sample_time_ms)
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

    /// 批量写入结算日历史电能数据（settlement_day=1~24）
    ///
    /// 结算日转存后，将所有结算日槽位的历史电能数据写入 energy_registers 表。
    /// 
    /// 设计说明：
    /// - 只写入 settlement_day > 0 的历史数据（当前结算周期 settlement_day=0 由 write_energy_register 处理）
    /// - 使用 REPLACE INTO 或 ON CONFLICT REPLACE 语义，确保每次转存后覆盖旧值
    /// - Key = (address, energy_kind, rate_index, settlement_day)
    /// - energy_kind: DI2编码（01=正向有功, 02=反向有功, 03=组合无功1, 04=组合无功2等）
    /// - rate_index: DI1编码（00=总, 01~3F=费率1~63）
    /// - settlement_day: DI0编码（01~18H = 上1~24个结算日）
    async fn write_settlement_energies(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        row: SettlementEnergiesRow,
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let address = &row.meter_address;

        // 将 f64 转换为定点数（*100）
        let to_fp = |v: f64| (v * 100.0) as i64;

        // 批量写入所有结算日历史数据
        for ((settlement_day, energy_kind, rate_index), value) in row.energies {
            // 只写入 settlement_day > 0 的历史数据（0=当前周期，由 write_energy_register 处理）
            if settlement_day == 0 {
                continue;
            }

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
            .bind(settlement_day)
            .bind(to_fp(value))
            .bind(now_ms)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }

    /// 在事务内保存虚拟时间和配置（用于定期持久化）
    /// 
    /// 此函数会更新所有仿真相关的配置参数，但不会覆盖协议相关的配置（如冻结模式、负荷记录模式等）
    async fn save_virtual_time_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        address: &str,
        virtual_time: chrono::DateTime<chrono::Utc>,
        synced_at_ms: i64,
        time_scale: f64,
        simulation_config: &crate::simulation::SimulationConfig,
    ) -> Result<(), sqlx::Error> {
        // 锚点由调用方与 virtual_time 同一时刻采集，这里不能用 now()——请求
        // 经过批量队列后落盘可能延迟，若以落盘时刻为锚点，停机补时会重复
        // 计入这段队列延迟。
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
        let now_ms = chrono::Utc::now().timestamp_millis();
        let result = sqlx::query(
            r#"
            UPDATE meters
            SET meter_constant = ?,
                rated_voltage_mv = ?,
                rated_current_ma = ?,
                demand_period_min = ?,
                load_model_json = ?,
                virtual_time_ms = ?,
                virtual_time_synced_at_ms = ?,
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
        .bind(synced_at_ms)
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
                    virtual_time_ms, virtual_time_synced_at_ms, time_scale,
                    rated_frequency_hz, initial_power_factor, updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            .bind(synced_at_ms)
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
                .with_timezone(&chrono::Utc);

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

    /// 查询某地址全部冻结历史快照（供 UI 切到"冻结数据"标签页时加载使用）
    ///
    /// 只查 `category = 0xFF`（完整快照摘要行，写入路径见 `write_freeze_snapshot`），
    /// 按时间倒序返回，跨全部触发类型。`limit` 控制最多返回多少行，避免长期
    /// 运行后单次查询过大（正常情况下各触发类型容量之和有上限，不会太大）。
    pub async fn query_freeze_history(
        pool: &SqlitePool,
        address: &str,
        limit: i64,
    ) -> Result<Vec<FreezeSnapshotRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT trigger_type, category, occurrence_idx, snapshot_time_ms, payload_json
            FROM freeze_snapshots
            WHERE address = ? AND category = 255
            ORDER BY snapshot_time_ms DESC
            LIMIT ?
            "#,
        )
        .bind(address)
        .bind(limit)
        .fetch_all(pool)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let snapshot_time_ms: i64 = row.get("snapshot_time_ms");
            let payload_json: String = row.get("payload_json");

            let snapshot_time = chrono::DateTime::from_timestamp_millis(snapshot_time_ms)
                .ok_or_else(|| sqlx::Error::Protocol("Invalid timestamp".to_string()))?
                .with_timezone(&chrono::Utc);

            let payload: serde_json::Value = serde_json::from_str(&payload_json)
                .map_err(|e| sqlx::Error::Protocol(format!("JSON parse error: {}", e)))?;

            result.push(FreezeSnapshotRow {
                meter_address: address.to_string(),
                trigger_type: row.get("trigger_type"),
                category: row.get("category"),
                occurrence_idx: row.get("occurrence_idx"),
                snapshot_time,
                payload,
            });
        }

        Ok(result)
    }

    /// 删除某地址全部冻结历史快照（`freeze_snapshots` 表），供"清除历史数据"使用。
    /// 跨全部触发类型/序号一次性清空；返回实际删除的行数。
    pub async fn delete_freeze_history(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM freeze_snapshots WHERE address = ?")
            .bind(address)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    // ═════════════════════════════════════════════════════════════════════════
    // 负荷记录查询（load_profile_records 表，JSON payload）
    // ═════════════════════════════════════════════════════════════════════════

    /// 查询最早的负荷记录块（06-DI2-00-00）
    pub async fn query_load_records_earliest(
        pool: &SqlitePool,
        address: &str,
        class_id: u8,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT class_id, sample_time_ms, payload_json
            FROM load_profile_records
            WHERE address = ? AND class_id = ?
            ORDER BY sample_time_ms ASC
            LIMIT 1
            "#,
        )
        .bind(address)
        .bind(class_id)
        .fetch_all(pool)
        .await?;

        Self::parse_load_record_rows(address, rows)
    }

    /// 查询给定时间的负荷记录块（06-DI2-00-01）
    pub async fn query_load_records_at_time(
        pool: &SqlitePool,
        address: &str,
        class_id: u8,
        target_time: &chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        let target_ms = target_time.timestamp_millis();

        // 查询最接近目标时间的记录（取绝对差值最小的）
        let rows = sqlx::query(
            r#"
            SELECT class_id, sample_time_ms, payload_json
            FROM load_profile_records
            WHERE address = ? AND class_id = ?
            ORDER BY ABS(sample_time_ms - ?) ASC
            LIMIT 1
            "#,
        )
        .bind(address)
        .bind(class_id)
        .bind(target_ms)
        .fetch_all(pool)
        .await?;

        Self::parse_load_record_rows(address, rows)
    }

    /// 查询最近的负荷记录块（06-DI2-00-02）
    pub async fn query_load_records_latest(
        pool: &SqlitePool,
        address: &str,
        class_id: u8,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT class_id, sample_time_ms, payload_json
            FROM load_profile_records
            WHERE address = ? AND class_id = ?
            ORDER BY sample_time_ms DESC
            LIMIT 1
            "#,
        )
        .bind(address)
        .bind(class_id)
        .fetch_all(pool)
        .await?;

        Self::parse_load_record_rows(address, rows)
    }

    /// 删除某地址全部负荷记录历史（`load_profile_records` 表），供"清除历史数据"使用。
    /// 跨全部类别一次性清空；返回实际删除的行数。
    pub async fn delete_load_records(pool: &SqlitePool, address: &str) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM load_profile_records WHERE address = ?")
            .bind(address)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 查询每个类别的最后一次采样时间（用于重启后恢复采样状态）
    ///
    /// 返回：[Option<DateTime<Utc>>; 6]，索引对应类别1-6（索引0对应类别1）
    pub async fn restore_last_sample_times(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<[Option<DateTime<Utc>>; 6], sqlx::Error> {
        let mut last_sample_times = [None; 6];

        for class_id in 1..=6 {
            let row: Option<(i64,)> = sqlx::query_as(
                r#"
                SELECT sample_time_ms
                FROM load_profile_records
                WHERE address = ? AND class_id = ?
                ORDER BY sample_time_ms DESC
                LIMIT 1
                "#,
            )
            .bind(address)
            .bind(class_id as i64)
            .fetch_optional(pool)
            .await?;

            if let Some((timestamp_ms,)) = row {
                use chrono::TimeZone;
                if let Some(dt) = chrono::Utc.timestamp_millis_opt(timestamp_ms).single() {
                    last_sample_times[class_id - 1] = Some(dt);
                }
            }
        }

        Ok(last_sample_times)
    }

    /// 查询最近的负荷记录（跨类别，或按 `class_id` 过滤单一类别），不依赖任何
    /// "当前时间"参照——单纯按 `sample_time_ms DESC LIMIT max_records`。
    ///
    /// 专供 UI"负荷记录"标签页总览列表使用：`sample_time_ms` 是电表的
    /// 虚拟时钟，仿真通常开倍速运行，虚拟时钟可能已经跑到比真实墙上时钟
    /// 靠后很多（甚至几天），如果像 `query_load_records_range` 那样用
    /// `chrono::Utc::now()` 划定时间窗口上界，虚拟时间"处在真实时间的
    /// 未来"的记录会被整体过滤掉——数据库明明有数据，UI 却一条都看不到。
    /// 这里不设时间窗口，直接取最新的 N 条，彻底避开真实/虚拟时钟不一致
    /// 的问题。
    pub async fn query_recent_load_records(
        pool: &SqlitePool,
        address: &str,
        max_records: u32,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT class_id, sample_time_ms, payload_json
            FROM load_profile_records
            WHERE address = ?
            ORDER BY sample_time_ms DESC
            LIMIT ?
            "#,
        )
        .bind(address)
        .bind(max_records as i64)
        .fetch_all(pool)
        .await?;

        Self::parse_load_record_rows(address, rows)
    }

    /// 查询时间范围内的负荷记录（跨类别，或按 `class_id` 过滤单一类别）
    ///
    /// `class_id = None` 时不筛类别，跨全部 1~6 类混合返回，按时间升序——
    /// 调用方需要自行提供一个跟 `sample_time_ms`（虚拟时钟）同源的时间
    /// 范围，不要传真实墙上时钟（`chrono::Utc::now()`）：仿真通常开倍速
    /// 运行，虚拟时钟可能已经跑到比真实时间靠后很多，用真实时间当上界会
    /// 把这些"处在真实时间未来"的记录整体过滤掉。UI"负荷记录"标签页的
    /// 总览列表不需要时间窗口这个概念，改用 `query_recent_load_records`。
    ///
    /// `class_id = Some(n)` 时只返回该类别，适合 06-10-DI1-DI0（曲线查询）
    /// 这种场景：这类查询在协议里没有类别维度，如果不筛类别，会把可能拥有
    /// 不同采样间隔的多个类别的记录交织成一条时间序列，产出的"曲线"没有
    /// 稳定的采样间隔、也可能同一时刻出现多个点，没有实际意义。
    pub async fn query_load_records_range(
        pool: &SqlitePool,
        address: &str,
        class_id: Option<u8>,
        start_time: &chrono::DateTime<chrono::Utc>,
        end_time: &chrono::DateTime<chrono::Utc>,
        max_records: u32,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        let start_ms = start_time.timestamp_millis();
        let end_ms = end_time.timestamp_millis();

        let rows = if let Some(class_id) = class_id {
            sqlx::query(
                r#"
                SELECT class_id, sample_time_ms, payload_json
                FROM load_profile_records
                WHERE address = ?
                  AND class_id = ?
                  AND sample_time_ms >= ?
                  AND sample_time_ms <= ?
                ORDER BY sample_time_ms ASC
                LIMIT ?
                "#,
            )
            .bind(address)
            .bind(class_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(max_records as i64)
            .fetch_all(pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT class_id, sample_time_ms, payload_json
                FROM load_profile_records
                WHERE address = ?
                  AND sample_time_ms >= ?
                  AND sample_time_ms <= ?
                ORDER BY sample_time_ms ASC
                LIMIT ?
                "#,
            )
            .bind(address)
            .bind(start_ms)
            .bind(end_ms)
            .bind(max_records as i64)
            .fetch_all(pool)
            .await?
        };

        Self::parse_load_record_rows(address, rows)
    }

    /// 解析load_profile_records表查询结果为LoadRecordRow
    fn parse_load_record_rows(
        address: &str,
        rows: Vec<sqlx::sqlite::SqliteRow>,
    ) -> Result<Vec<LoadRecordRow>, sqlx::Error> {
        use sqlx::Row;

        let mut records = Vec::new();

        for row in rows {
            let class_id: u8 = row.get("class_id");
            let sample_time_ms: i64 = row.get("sample_time_ms");
            let payload_json: String = row.get("payload_json");

            let sample_time = chrono::DateTime::from_timestamp_millis(sample_time_ms)
                .ok_or_else(|| sqlx::Error::Protocol("Invalid timestamp".to_string()))?
                .with_timezone(&chrono::Utc);

            let payload: serde_json::Value = serde_json::from_str(&payload_json)
                .map_err(|e| sqlx::Error::Protocol(format!("JSON parse error: {}", e)))?;

            records.push(LoadRecordRow {
                meter_address: address.to_string(),
                class_id,
                sample_time,
                payload,
            });
        }

        Ok(records)
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

    /// 恢复结算日历史电能数据（settlement_day=1~24）
    ///
    /// 从 energy_registers 表中读取所有结算日历史数据（settlement_day > 0），
    /// 返回 HashMap<(settlement_day, energy_kind, rate_index), value>。
    /// 
    /// 调用时机：
    /// - 虚拟电表启动时，从数据库恢复结算日历史数据到内存的 settlement_energies HashMap
    /// 
    /// 设计说明：
    /// - 只查询 settlement_day > 0 的记录（0 表示当前结算周期，由 restore_energy_registers 处理）
    /// - Key = (settlement_day, energy_kind, rate_index)
    /// - Value = 电能值（kWh/kvarh），从定点数（*100）转换为浮点数
    pub async fn restore_settlement_energies(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<std::collections::HashMap<(u8, u8, u8), f64>, sqlx::Error> {
        use std::collections::HashMap;

        // 只加载最近6个结算日的数据（可根据需求调整）
        // DL/T645-2007协议支持12个结算日，但实际应用中可能只需要最近几个月
        const MAX_SETTLEMENT_DAYS: u8 = 12; // 改为6可以减半内存占用

        let rows = sqlx::query(
            r#"
            SELECT settlement_day, energy_kind, rate_index, value_fp
            FROM energy_registers
            WHERE address = ? 
              AND settlement_day > 0 
              AND settlement_day <= ?
            ORDER BY settlement_day, energy_kind, rate_index
            "#,
        )
        .bind(address)
        .bind(MAX_SETTLEMENT_DAYS as i64)
        .fetch_all(pool)
        .await?;

        let mut energies = HashMap::new();

        for row in rows {
            let settlement_day: u8 = row.get::<i64, _>("settlement_day") as u8;
            let energy_kind: u8 = row.get::<i64, _>("energy_kind") as u8;
            let rate_index: u8 = row.get::<i64, _>("rate_index") as u8;
            let value_fp: i64 = row.get("value_fp");

            // 将定点数转换回浮点数（除以100）
            let value = value_fp as f64 / 100.0;

            energies.insert((settlement_day, energy_kind, rate_index), value);
        }

        Ok(energies)
    }

    /// 查询单个结算日历史电能值（供 DIHandler 内存未命中时按需查询数据库）
    ///
    /// 与 `query_freeze_snapshot` 的按需回退设计一致：`settlement_energies`
    /// 内存 HashMap 正常情况下在电表连接时由 `restore_settlement_energies`
    /// 整体恢复，但如果恢复失败/尚未完成，或读取请求先于恢复完成到达，
    /// 单条查询可以兜底，避免直接返回错误的 0 值。
    pub async fn query_settlement_energy(
        pool: &SqlitePool,
        address: &str,
        energy_kind: u8,
        rate_index: u8,
        settlement_day: u8,
    ) -> Result<Option<f64>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT value_fp
            FROM energy_registers
            WHERE address = ? AND energy_kind = ? AND rate_index = ? AND settlement_day = ?
            "#,
        )
        .bind(address)
        .bind(energy_kind)
        .bind(rate_index)
        .bind(settlement_day)
        .fetch_optional(pool)
        .await?;

        Ok(row.map(|r| {
            let value_fp: i64 = r.get("value_fp");
            value_fp as f64 / 100.0
        }))
    }

    /// 查询某地址全部结算日历史电能（供 UI 切到"结算日电能"标签页时加载使用）
    ///
    /// 返回按 settlement_day/energy_kind/rate_index 排序的全部记录（settlement_day
    /// > 0），由调用方按需筛选/聚合成展示摘要。
    pub async fn query_settlement_energy_history(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<Vec<crate::persistence::SettlementEnergyDbRow>, sqlx::Error> {
        use crate::persistence::SettlementEnergyDbRow;

        let rows = sqlx::query(
            r#"
            SELECT settlement_day, energy_kind, rate_index, value_fp, updated_at_ms
            FROM energy_registers
            WHERE address = ? AND settlement_day > 0
            ORDER BY settlement_day, energy_kind, rate_index
            "#,
        )
        .bind(address)
        .fetch_all(pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let value_fp: i64 = row.get("value_fp");
                SettlementEnergyDbRow {
                    settlement_day: row.get::<i64, _>("settlement_day") as u8,
                    energy_kind: row.get::<i64, _>("energy_kind") as u8,
                    rate_index: row.get::<i64, _>("rate_index") as u8,
                    value: value_fp as f64 / 100.0,
                    updated_at_ms: row.get("updated_at_ms"),
                }
            })
            .collect())
    }

    /// 从数据库恢复虚拟时钟
    ///
    /// 读取 meters 表中保存的 virtual_time_ms 及其落盘锚点
    /// virtual_time_synced_at_ms，供调用方计算停机补时。
    pub async fn restore_virtual_time(
        pool: &SqlitePool,
        address: &str,
    ) -> Result<Option<RestoredVirtualTime>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT virtual_time_ms, virtual_time_synced_at_ms
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
                .with_timezone(&chrono::Utc);

            Ok(Some(RestoredVirtualTime {
                virtual_time: dt,
                synced_at_ms: row.get("virtual_time_synced_at_ms"),
            }))
        } else {
            Ok(None)
        }
    }

    /// 保存虚拟时间和时间倍率
    ///
    /// 用于快速保存虚拟时钟状态，不更新其他配置字段
    /// 如需更新完整的仿真配置，请使用 save_simulation_config
    ///
    /// `synced_at_ms` 是 `virtual_time` 快照对应的本地真实时间（停机补时锚点），
    /// 由调用方与 virtual_time 同一时刻采集。
    pub async fn save_virtual_time(
        pool: &SqlitePool,
        address: &str,
        virtual_time: chrono::DateTime<chrono::Utc>,
        synced_at_ms: i64,
        time_scale: f64,
    ) -> Result<(), sqlx::Error> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let virtual_time_ms = virtual_time.timestamp_millis();

        // UPDATE 对不存在的行是静默空操作（比如电表运行期间从未触发过任何
        // 配置保存，meters 行还没建），先确保行存在再更新
        Self::ensure_meter_row(pool, address).await?;

        // 只更新虚拟时间和时间倍率，不覆盖其他配置字段
        sqlx::query(
            r#"
            UPDATE meters
            SET virtual_time_ms = ?,
                virtual_time_synced_at_ms = ?,
                time_scale = ?,
                updated_at_ms = ?
            WHERE address = ?
            "#
        )
        .bind(virtual_time_ms)
        .bind(synced_at_ms)
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
        intervals: [u16; 8],
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

    /// 保存或更新协议参数（`comm_baud_json` + `passwords_json` + `tou_config_json["tou"]`）
    ///
    /// 虚拟时间不在这里写——它有独立锚点语义（`save_virtual_time`），由调用方处理。
    pub async fn save_protocol_parameters(
        pool: &SqlitePool,
        address: &str,
        baudrate: u8,
        passwords: &[[u8; 4]; 10],
        time_slots: &[(u8, u8, u8)],
    ) -> Result<(), sqlx::Error> {
        let now_ms = Utc::now().timestamp_millis();

        let mut extra = Self::read_extra_config(pool, address).await?;
        extra["tou"] = json!({
            "time_slots": time_slots
                .iter()
                .map(|(h, m, r)| [*h, *m, *r])
                .collect::<Vec<[u8; 3]>>(),
        });

        Self::ensure_meter_row(pool, address).await?;
        sqlx::query(
            r#"
            UPDATE meters
            SET comm_baud_json = ?,
                passwords_json = ?,
                tou_config_json = ?,
                updated_at_ms = ?
            WHERE address = ?
            "#,
        )
        .bind(json!({"baudrate": baudrate}).to_string())
        .bind(json!({"passwords": passwords}).to_string())
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
                   freeze_mode_word, load_record_mode_word,
                   comm_baud_json, passwords_json
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
        let mut load_record_intervals = [0u16; 8];
        if let Some(intervals) = load_record.get("intervals").and_then(|v| v.as_array()) {
            for (i, value) in intervals.iter().take(6).enumerate() {
                load_record_intervals[i] = value.as_u64().unwrap_or(0) as u16;
            }
        }

        // 协议参数（老库从未保存过时保持 None，由调用方沿用默认值）
        let baudrate: Option<u8> = {
            let raw: String = row.get("comm_baud_json");
            serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|v| v.get("baudrate").and_then(|x| x.as_u64()).map(|n| n as u8))
        };
        let passwords: Option<[[u8; 4]; 10]> = {
            let raw: String = row.get("passwords_json");
            serde_json::from_str::<Value>(&raw).ok().and_then(|v| {
                let arr = v.get("passwords")?.as_array()?;
                if arr.len() < 10 {
                    return None;
                }
                let mut out = [[0u8; 4]; 10];
                for (i, entry) in arr.iter().take(10).enumerate() {
                    let bytes = entry.as_array()?;
                    if bytes.len() < 4 {
                        return None;
                    }
                    for (j, byte) in bytes.iter().take(4).enumerate() {
                        out[i][j] = byte.as_u64()? as u8;
                    }
                }
                Some(out)
            })
        };
        let tou_time_slots: Option<Vec<(u8, u8, u8)>> = extra
            .get("tou")
            .and_then(|t| t.get("time_slots"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|slot| {
                        let bytes = slot.as_array()?;
                        if bytes.len() < 3 {
                            return None;
                        }
                        Some((
                            bytes[0].as_u64()? as u8,
                            bytes[1].as_u64()? as u8,
                            bytes[2].as_u64()? as u8,
                        ))
                    })
                    .collect::<Vec<_>>()
            });

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
            baudrate,
            passwords,
            tou_time_slots,
        }))
    }

}