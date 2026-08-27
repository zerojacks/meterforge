use gpui::*;
use meter_core::actor::{
    address_to_string, string_to_address, AdminCommand, MeterActor, MeterActorConfig,
    MeterActorHandle, MeterRegistry, TickMsg,
};
use meter_core::persistence::{PersistRequest, PersistenceWorker};
use meter_core::simulation::{LoadProfile, SimulationConfig, VirtualMeter, VirtualMeterConfig};
use meter_core::snapshot::FreezeSnapshotSummary;
use meter_core::ConnectionManager;
use parking_lot::RwLock;
use sqlx::SqlitePool;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, mpsc};

/// A presentation-level intent.  It deliberately contains no GPUI types and
/// lets views stay independent from the actor protocol.
#[derive(Debug, Clone)]
pub enum MeterAction {
    SetVirtualTime {
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    },
    ChangePassword {
        level: u8,
        new_password: [u8; 4],
    },
    SetBaudrate {
        baudrate: u8,
    },
    SetTouConfig {
        time_slots: Vec<(u8, u8, u8)>,
    },
    ClearMaxDemand,
    ClearMeter,
    ApplySimulationConfig {
        config: SimulationConfig,
    },
    ApplyFreezeConfig {
        timed_mode: u8,
        instant_mode: u8,
        appointment_mode: u8,
        hourly_mode: u8,
        daily_mode: u8,
        daily_time: [u8; 2],
        hourly_start: [u8; 5],
        hourly_interval_min: u8,
        appointment_time: [u8; 5],
    },
    ApplySettlementDays {
        days: [u8; 3],
        hours: [u8; 3],
    },
    ApplyLoadRecordConfig {
        mode_word: u8,
        start_time: [u8; 4],
        intervals: [u16; 8],
    },
    InjectFault {
        event_type: u8,
        phase: u8,
        active: bool,
    },
}

impl From<MeterAction> for AdminCommand {
    fn from(value: MeterAction) -> Self {
        match value {
            MeterAction::SetVirtualTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            } => AdminCommand::SetVirtualTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            },
            MeterAction::ChangePassword {
                level,
                new_password,
            } => AdminCommand::ChangePassword {
                level,
                new_password,
            },
            MeterAction::SetBaudrate { baudrate } => AdminCommand::SetBaudrate { baudrate },
            MeterAction::SetTouConfig { time_slots } => AdminCommand::SetTouConfig { time_slots },
            MeterAction::ClearMaxDemand => AdminCommand::ClearMaxDemand,
            MeterAction::ClearMeter => AdminCommand::ClearMeter,
            MeterAction::ApplySimulationConfig { config } => {
                AdminCommand::ApplySimulationConfig { config }
            }
            MeterAction::ApplyFreezeConfig {
                timed_mode,
                instant_mode,
                appointment_mode,
                hourly_mode,
                daily_mode,
                daily_time,
                hourly_start,
                hourly_interval_min,
                appointment_time,
            } => AdminCommand::ApplyFreezeConfig {
                timed_mode,
                instant_mode,
                appointment_mode,
                hourly_mode,
                daily_mode,
                daily_time,
                hourly_start,
                hourly_interval_min,
                appointment_time,
            },
            MeterAction::ApplySettlementDays { days, hours } => {
                AdminCommand::ApplySettlementDays { days, hours }
            }
            MeterAction::ApplyLoadRecordConfig {
                mode_word,
                start_time,
                intervals,
            } => AdminCommand::ApplyLoadRecordConfig {
                mode_word,
                start_time,
                intervals,
            },
            MeterAction::InjectFault {
                event_type,
                phase,
                active,
            } => AdminCommand::InjectFault {
                event_type,
                phase,
                active,
            },
        }
    }
}

/// 当前表的协议参数（`AdminCommand::GetProtocolParameters` 返回 JSON 的
/// 反序列化目标），用于"一键同步参数到所有表"读取源表已生效的值。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProtocolParameters {
    pub virtual_time_ms: i64,
    pub baudrate: u8,
    pub passwords: [[u8; 4]; 10],
    pub time_slots: Vec<(u8, u8, u8)>,
}

/// Long-lived backend services exposed to the presentation layer.
///
/// Keeping this as one GPUI global makes views easy to construct and test: no
/// view owns connection tasks, actor handles, or a Tokio runtime.
#[derive(Clone)]
pub struct AppBackend {
    pub connections: ConnectionManager,
    meters: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
    /// 持久化句柄（`ENABLE_PERSISTENCE = false` 时为 None）：删除/添加电表时
    /// 用于排空批量写队列并读写数据库。
    db_pool: Option<SqlitePool>,
    persist_tx: Option<mpsc::Sender<PersistRequest>>,
    /// 全局 tick 广播源；"添加表"运行时新 spawn 的 actor 需要订阅它才能
    /// 跟其余表一起走时（`bootstrap.rs::spawn_demo_meters` 里同一份）。
    tick_tx: broadcast::Sender<TickMsg>,
}

impl AppBackend {
    pub fn new(
        connections: ConnectionManager,
        meters: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
        persistence: Option<(SqlitePool, mpsc::Sender<PersistRequest>)>,
        tick_tx: broadcast::Sender<TickMsg>,
    ) -> Self {
        Self {
            connections,
            meters,
            db_pool: persistence.as_ref().map(|(pool, _)| pool.clone()),
            persist_tx: persistence.map(|(_, tx)| tx),
            tick_tx,
        }
    }

    /// 当前已存在的表地址集合（已排序），供"添加表"对话框做唯一性校验、
    /// 以及"复制自"下拉列出可选来源。
    pub fn meter_addresses(&self) -> Vec<String> {
        let mut addresses: Vec<String> = self.meters.read().keys().cloned().collect();
        addresses.sort();
        addresses
    }

    /// Dispatch a backend command from GPUI's executor.
    ///
    /// UI callbacks run on the native window thread, which does not have a
    /// Tokio reactor.  Using the GPUI background executor keeps the command
    /// asynchronous without calling `tokio::spawn` outside a Tokio runtime.
    pub fn dispatch(&self, address: String, action: MeterAction, cx: &App) {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            tracing::warn!(%address, "meter command ignored because actor was not found");
            return;
        };
        let command = action.into();
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = handle.send_admin_command(command).await {
                    tracing::warn!(%address, %error, "meter command failed");
                }
            })
            .detach();
    }

    /// 向所有表（可排除一块，比如同步的源表）广播同一条命令。
    ///
    /// 与 `dispatch` 一样 fire-and-forget：命令在各自 MeterActor 的 tokio
    /// 上下文里执行（含落库），失败只记日志，不阻塞 UI。返回值为目标表数，
    /// 供调用方提示。
    pub fn dispatch_all(&self, exclude: Option<&str>, action: MeterAction, cx: &App) -> usize {
        let targets: Vec<(String, MeterActorHandle)> = self
            .meters
            .read()
            .iter()
            .filter(|(address, _)| Some(address.as_str()) != exclude)
            .map(|(address, handle)| (address.clone(), handle.clone()))
            .collect();
        let count = targets.len();
        for (address, handle) in targets {
            let command = action.clone().into();
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = handle.send_admin_command(command).await {
                        tracing::warn!(%address, %error, "meter broadcast command failed");
                    }
                })
                .detach();
        }
        count
    }

    /// 一键同步协议参数：读取源表（参数设置页的当前表）已生效的
    /// 虚拟时间 / 密码 / 通信速率 / 费率时段表，下发给其余所有表。
    ///
    /// 只走 mpsc/oneshot 通道；数据库写入在目标表 MeterActor 的 tokio
    /// 上下文里完成，与 `load_freeze_history` 同一模式。返回成功同步的
    /// 表数，供 UI 弹通知。
    pub fn sync_protocol_parameters(
        &self,
        source: String,
        cx: &App,
    ) -> Task<Result<usize, String>> {
        let meters = self.meters.read().clone();
        let Some(origin) = meters.get(&source).cloned() else {
            return Task::ready(Err(format!("meter {source} not found")));
        };
        let targets: Vec<(String, MeterActorHandle)> = meters
            .into_iter()
            .filter(|(address, _)| address != &source)
            .collect();
        cx.background_executor().spawn(async move {
            let json = origin
                .send_admin_command(AdminCommand::GetProtocolParameters)
                .await?;
            let params: ProtocolParameters =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            let mut synced = 0usize;
            for (address, handle) in targets {
                // 每个目标表单独记录下发时刻，让接收端按传输耗时补偿虚拟时间。
                let command = AdminCommand::ApplyProtocolParameters {
                    virtual_time_ms: params.virtual_time_ms,
                    sent_at_ms: chrono::Utc::now().timestamp_millis(),
                    baudrate: params.baudrate,
                    passwords: params.passwords,
                    time_slots: params.time_slots.clone(),
                };
                match handle.send_admin_command(command).await {
                    Ok(_) => synced += 1,
                    Err(error) => tracing::warn!(%address, %error, "参数同步失败"),
                }
            }
            Ok(synced)
        })
    }

    /// 异步加载某块表的冻结历史（内存环形缓冲 + 数据库，已合并去重）。
    ///
    /// 与 `dispatch` 不同，这是一个查询：不直接操作数据库，只是通过 admin
    /// 通道把请求转发给对应的 `MeterActor`，由它在自己的异步上下文里完成
    /// 内存/数据库合并后把 JSON 结果传回来。调用方（UI 视图）应在切换到
    /// "冻结数据"标签页时调用一次，用 `cx.spawn` await 这个 Task。
    pub fn load_freeze_history(
        &self,
        address: String,
        cx: &App,
    ) -> Task<Result<Vec<FreezeSnapshotSummary>, String>> {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            return Task::ready(Err(format!("meter {address} not found")));
        };
        cx.background_executor().spawn(async move {
            let json = handle
                .send_admin_command(AdminCommand::LoadFreezeHistory)
                .await?;
            serde_json::from_str(&json).map_err(|e| e.to_string())
        })
    }

    /// 异步加载某块表最近的负荷记录采样（跨全部数据类型/通道，按时间倒序）。
    ///
    /// 负荷记录采样落库后不维护内存历史，因此每次调用都会打库；调用方
    /// （UI 视图）应仅在切换到"负荷记录"标签页且尚未加载过时调用一次。
    pub fn load_load_profile_history(
        &self,
        address: String,
        max_records: u32,
        cx: &App,
    ) -> Task<Result<Vec<meter_core::snapshot::LoadRecordSnapshot>, String>> {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            return Task::ready(Err(format!("meter {address} not found")));
        };
        cx.background_executor().spawn(async move {
            let json = handle
                .send_admin_command(AdminCommand::LoadLoadProfileHistory { max_records })
                .await?;
            serde_json::from_str(&json).map_err(|e| e.to_string())
        })
    }

    /// 清除某块表的冻结历史数据（内存环形缓冲 + 数据库 `freeze_snapshots` 表），
    /// 保留冻结相关配置。返回 actor 侧的结果文案（成功时含删除行数），供 UI 弹通知。
    pub fn clear_freeze_history(&self, address: String, cx: &App) -> Task<Result<String, String>> {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            return Task::ready(Err(format!("meter {address} not found")));
        };
        cx.background_executor().spawn(async move {
            handle
                .send_admin_command(AdminCommand::ClearFreezeHistory)
                .await
        })
    }

    /// 清除某块表的负荷记录历史数据（内存缓冲 + 数据库 `load_profile_records` 表），
    /// 保留负荷记录配置。返回 actor 侧的结果文案，供 UI 弹通知。
    pub fn clear_load_profile_history(
        &self,
        address: String,
        cx: &App,
    ) -> Task<Result<String, String>> {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            return Task::ready(Err(format!("meter {address} not found")));
        };
        cx.background_executor().spawn(async move {
            handle
                .send_admin_command(AdminCommand::ClearLoadProfileHistory)
                .await
        })
    }

    /// 批量清除所有表的冻结历史数据（电表列表面板顶部"清除历史数据"入口）。
    /// 与 `dispatch_all` 一样对每块表 fire 一条命令，但这里需要等待全部完成
    /// 才能统计成功数，供确认弹窗关闭后的通知使用，因此返回 `Task<(成功数, 总数)>`。
    pub fn clear_freeze_history_all(&self, cx: &App) -> Task<(usize, usize)> {
        let targets: Vec<MeterActorHandle> = self.meters.read().values().cloned().collect();
        let total = targets.len();
        cx.background_executor().spawn(async move {
            let results = futures::future::join_all(
                targets
                    .iter()
                    .map(|handle| handle.send_admin_command(AdminCommand::ClearFreezeHistory)),
            )
            .await;
            let success = results.iter().filter(|r| r.is_ok()).count();
            (success, total)
        })
    }

    /// 批量清除所有表的负荷记录历史数据（电表列表面板顶部"清除负荷记录数据"入口）。
    pub fn clear_load_profile_history_all(&self, cx: &App) -> Task<(usize, usize)> {
        let targets: Vec<MeterActorHandle> = self.meters.read().values().cloned().collect();
        let total = targets.len();
        cx.background_executor().spawn(async move {
            let results =
                futures::future::join_all(targets.iter().map(|handle| {
                    handle.send_admin_command(AdminCommand::ClearLoadProfileHistory)
                }))
                .await;
            let success = results.iter().filter(|r| r.is_ok()).count();
            (success, total)
        })
    }
}

/// `AppBackend::add_meter` 成功后交给调用方（UI 视图）用于完成剩余 Entity
/// 创建 + 注册的产物。Entity 创建必须在 GPUI 主线程做（`cx.new`），所以
/// `add_meter` 本身不在内部创建 UI Entity——它只负责 spawn actor、注册进
/// 路由表/句柄表，把"造 Entity 用得着的东西"打包返回，剩下的交给调用方在
/// `.update()` 回调里做，与 `remove_meter` 之后的 UI 清理（`MeterListView::
/// remove_deleted_meter`）是对称的分工。
pub struct NewMeterHandle {
    pub address: String,
    /// 初始快照，用于构造 `MeterState` Entity 的起始值（不用等第一次 tick）。
    pub initial_snapshot: crate::types::MeterSnapshot,
    /// 后续 tick 由 actor 持续推送到这里，交给 `MeterState::start_update_loop`
    /// 消费。
    pub snapshot_rx: mpsc::UnboundedReceiver<crate::types::MeterSnapshot>,
}

impl AppBackend {
    /// 新增一块表（可选从某个已存在的表复制配置和历史数据）。
    ///
    /// `source_address` 为 `Some` 时，先对源表走一次持久化队列排空
    /// （[`PersistRequest::Barrier`]，与 `remove_meter` 删除前的做法一致），
    /// 再把它在数据库层面的全部持久化数据复制到新地址，然后用
    /// `VirtualMeter::restore_from_database` 把这份数据读回新建的
    /// `VirtualMeter`——跟 bootstrap 启动时"从数据库恢复上次状态"走的是
    /// 同一条路径，只是数据来源换成了刚复制过来的另一块表。为 `None` 时是
    /// 全新表，使用默认仿真配置（居民负荷）。
    ///
    /// 数据库相关的部分（复制 + 恢复）用 `runtime.spawn(...).await` 跑在
    /// ConnectionManager 的专属 tokio Runtime 上——与 `remove_meter` 的
    /// db_pool/oneshot 处理同一手法，因为 sqlx 需要真正的 tokio 上下文，
    /// GPUI 的 smol background_executor 没有这个上下文。
    ///
    /// 成功后返回 [`NewMeterHandle`]，由调用方在 GPUI 主线程完成 Entity
    /// 创建与注册。
    pub fn add_meter(
        &self,
        new_address_bytes: [u8; 6],
        source_address: Option<String>,
        cx: &App,
    ) -> Task<Result<NewMeterHandle, String>> {
        let new_address = address_to_string(&new_address_bytes);
        if self.meters.read().contains_key(&new_address) {
            return Task::ready(Err(format!("地址 {new_address} 已存在")));
        }

        let runtime = self.connections.runtime_handle();
        let db_pool = self.db_pool.clone();
        let persist_tx = self.persist_tx.clone();
        let core_registry = self.connections.registry();
        let tick_tx = self.tick_tx.clone();
        let meters = self.meters.clone();
        let new_address_for_task = new_address.clone();

        cx.background_executor().spawn(async move {
            let persist_tx_for_barrier = persist_tx.clone();
            let source_for_db = source_address.clone();
            let db_pool_for_db = db_pool.clone();
            let new_address_for_db = new_address_for_task.clone();

            let join = runtime.spawn(async move {
                // 复制前先排空持久化队列：保证源表最新的批量写入（电能/
                // 负荷记录等）已经落库，不会被漏拷贝——跟删除电表前的
                // Barrier 用法一致。
                if let (Some(src), Some(pool)) = (&source_for_db, &db_pool_for_db) {
                    if let Some(tx) = &persist_tx_for_barrier {
                        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                        if tx
                            .send(PersistRequest::Barrier { ack: ack_tx })
                            .await
                            .is_ok()
                        {
                            let _ = ack_rx.await;
                        }
                    }
                    PersistenceWorker::copy_meter_data(pool, src, &new_address_for_db)
                        .await
                        .map_err(|e| format!("复制数据库数据失败: {e}"))?;
                }

                let mut config = VirtualMeterConfig::default();
                config.address = new_address_bytes;
                if source_for_db.is_none() {
                    config.physics_config.load_model.profile = LoadProfile::Residential;
                }

                let mut virtual_meter = match &persist_tx_for_barrier {
                    Some(tx) => VirtualMeter::with_persistence(config, tx.clone()),
                    None => VirtualMeter::new(config),
                };

                if let Some(pool) = &db_pool_for_db {
                    if source_for_db.is_some() {
                        virtual_meter
                            .restore_from_database(pool)
                            .await
                            .map_err(|e| format!("从数据库恢复新表状态失败: {e}"))?;
                    }
                }

                Ok::<VirtualMeter, String>(virtual_meter)
            });
            let virtual_meter = join.await.map_err(|e| format!("任务执行失败: {e}"))??;

            let initial_snapshot = crate::types::MeterSnapshot::from_state(
                virtual_meter.state(),
                virtual_meter.load_model_config(),
                true,
            );
            let (snapshot_tx, snapshot_rx) =
                mpsc::unbounded_channel::<crate::types::MeterSnapshot>();

            let (command_tx, command_rx) = mpsc::channel(100);
            let actor = MeterActor::new(
                virtual_meter,
                tick_tx.subscribe(),
                command_rx,
                MeterActorConfig {
                    address: new_address_bytes,
                    cmd_queue_capacity: 100,
                    enable_persistence: db_pool.is_some(),
                    db_pool,
                    registry_tx: None,
                    snapshot_tx: Some(snapshot_tx),
                },
            );
            let handle = MeterActorHandle::new(command_tx, new_address_bytes);
            runtime.spawn(async move {
                actor.run().await;
            });

            meters
                .write()
                .insert(new_address_for_task.clone(), handle.clone());
            let _ = core_registry
                .lock()
                .await
                .register(new_address_bytes, handle);

            Ok(NewMeterHandle {
                address: new_address_for_task,
                initial_snapshot,
                snapshot_rx,
            })
        })
    }

    /// 修改一块表的地址（电表列表"修改地址"入口），保留该表全部配置与历史
    /// 数据，仅地址变化。需要同步内存与数据库两边的五处状态，顺序如下：
    ///
    /// 1. [`AdminCommand::SetAddress`] 切换 actor 内存地址（仿真状态 + actor
    ///    配置）并等待回执——actor 单线程处理命令，回执后不再产生旧地址的
    ///    持久化写；
    /// 2. 核心路由表 re-key（新地址立即生效，旧地址的协议帧不再命中）；
    /// 3. 在专属 tokio Runtime 上 Barrier 排空持久化队列后，单事务完成
    ///    [`PersistenceWorker::rename_meter_data`]（复制旧→新 + 删除旧行）；
    /// 4. UI 句柄表 re-key（同步句柄缓存的地址字段）。
    ///
    /// 若第 2/3 步失败则 best-effort 回滚（把 actor 与路由表改回旧地址），
    /// 避免出现"内存里是新地址、数据库还是旧地址、重启后新旧两块表"的
    /// 不一致。成功返回新地址字符串，供 UI 侧完成注册表 re-key 与选中态切换。
    pub fn update_meter_address(
        &self,
        old_address: &str,
        new_address_bytes: [u8; 6],
        cx: &App,
    ) -> Task<Result<String, String>> {
        let old_address = old_address.to_owned();
        let new_address = address_to_string(&new_address_bytes);

        let (handle, old_bytes) = {
            let meters = self.meters.read();
            if new_address == old_address {
                return Task::ready(Err("新地址与当前地址相同".to_string()));
            }
            let Some(handle) = meters.get(&old_address).cloned() else {
                return Task::ready(Err(format!("meter {old_address} not found")));
            };
            if meters.contains_key(&new_address) {
                return Task::ready(Err(format!("地址 {new_address} 已存在")));
            }
            let Ok(old_bytes) = string_to_address(&old_address) else {
                return Task::ready(Err(format!("旧地址格式非法: {old_address}")));
            };
            (handle, old_bytes)
        };

        let core_registry = self.connections.registry();
        let runtime = self.connections.runtime_handle();
        let db_pool = self.db_pool.clone();
        let persist_tx = self.persist_tx.clone();
        let meters = self.meters.clone();
        let old_address_for_db = old_address.clone();
        let new_address_for_db = new_address.clone();
        let new_address_for_task = new_address.clone();

        cx.background_executor().spawn(async move {
            // 1. 切换 actor 内存地址；回执后 actor 只会产生新地址的持久化写
            if let Err(error) = handle
                .send_admin_command(AdminCommand::SetAddress {
                    address: new_address_bytes,
                })
                .await
            {
                return Err(format!("切换电表地址失败: {error}"));
            }

            // 2. 路由表 re-key；失败（新地址已被注册等竞态）则把 actor 切回去
            if let Err(error) = core_registry
                .lock()
                .await
                .update_address(&old_bytes, new_address_bytes)
            {
                let _ = handle
                    .send_admin_command(AdminCommand::SetAddress { address: old_bytes })
                    .await;
                return Err(format!("更新路由表失败: {error}"));
            }

            // 3. Barrier 排空 + 数据库改名（sqlx 需要 tokio 上下文）
            let db_result = runtime
                .spawn(async move {
                    if let Some(tx) = persist_tx {
                        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                        if tx
                            .send(PersistRequest::Barrier { ack: ack_tx })
                            .await
                            .is_ok()
                        {
                            let _ = ack_rx.await;
                        }
                    }
                    if let Some(pool) = db_pool {
                        PersistenceWorker::rename_meter_data(
                            &pool,
                            &old_address_for_db,
                            &new_address_for_db,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    }
                    Ok::<(), String>(())
                })
                .await
                .map_err(|error| format!("任务执行失败: {error}"))?;

            match db_result {
                // 4. 句柄表 re-key（同步句柄缓存的地址字段，保持与路由表一致）。
                // 必须只拿一次写锁：`if let` 条件里的临时写守卫会活到语句结束，
                // 若在块内再次 `meters.write()`，不可重入的 RwLock 会在同一线程
                // 上死锁——改名后 UI 不刷新（详情面板停留在旧地址）的根因。
                Ok(()) => {
                    let mut meters_guard = meters.write();
                    if let Some(mut handle) = meters_guard.remove(&old_address) {
                        handle.address = new_address_bytes;
                        meters_guard.insert(new_address_for_task.clone(), handle);
                    }
                    drop(meters_guard);
                    Ok(new_address_for_task)
                }
                // 数据库改名失败：回滚 actor 内存地址与路由表，恢复一致后报错
                Err(error) => {
                    let _ = handle
                        .send_admin_command(AdminCommand::SetAddress { address: old_bytes })
                        .await;
                    let _ = core_registry
                        .lock()
                        .await
                        .update_address(&new_address_bytes, old_bytes);
                    Err(format!("数据库改名失败: {error}"))
                }
            }
        })
    }

    /// 删除一块表（电表列表"删除"入口）：
    /// 1. 从路由注册表与 actor 句柄表摘除，之后的协议帧/admin 命令都不再命中它；
    /// 2. 优雅关闭 actor（它会把最终数据塞进持久化队列后停止）；
    /// 3. 等持久化队列排空（Barrier ack 时这批写入已提交）；
    /// 4. 清除该表在数据库的全部行——下次启动按 `meters` 表恢复存量表时，
    ///    被删除的表不会再被建回来。
    ///
    /// 具体流程在 [`Self::teardown_meter`] 里实现，与批量删除共用。
    pub fn remove_meter(&self, address: &str, cx: &App) -> Task<Result<(), String>> {
        let meters = self.meters.clone();
        let core_registry = self.connections.registry();
        let runtime = self.connections.runtime_handle();
        let db_pool = self.db_pool.clone();
        let persist_tx = self.persist_tx.clone();
        let address = address.to_owned();

        cx.background_executor().spawn(async move {
            Self::teardown_meter(
                &meters,
                &core_registry,
                &runtime,
                &db_pool,
                &persist_tx,
                &address,
            )
            .await
        })
    }

    /// 批量删除多块表（电表列表"删除选中"入口）。
    ///
    /// 与逐块 `remove_meter` 的差别只在编排：全部在同一个后台任务里**串行**
    /// 逐表执行（每块都要走 Barrier + 数据库事务，并发会让 SQLite 互相锁），
    /// 单块失败不中断——记录原因后继续删剩下的表。返回
    /// `(成功数, 失败列表（地址, 原因）)`，供 UI 弹汇总通知。
    pub fn remove_meters(
        &self,
        addresses: Vec<String>,
        cx: &App,
    ) -> Task<(usize, Vec<(String, String)>)> {
        let meters = self.meters.clone();
        let core_registry = self.connections.registry();
        let runtime = self.connections.runtime_handle();
        let db_pool = self.db_pool.clone();
        let persist_tx = self.persist_tx.clone();

        cx.background_executor().spawn(async move {
            let mut success = 0usize;
            let mut failures = Vec::new();
            for address in addresses {
                match Self::teardown_meter(
                    &meters,
                    &core_registry,
                    &runtime,
                    &db_pool,
                    &persist_tx,
                    &address,
                )
                .await
                {
                    Ok(()) => success += 1,
                    Err(error) => failures.push((address, error)),
                }
            }
            (success, failures)
        })
    }

    /// `remove_meter` / `remove_meters` 共用的单表删除流程：
    /// 摘句柄 → 注销路由 → 优雅关闭 actor → Barrier 排空 → 清库。
    ///
    /// Barrier 与数据库清理涉及 sqlx 与 oneshot 等待，必须跑在
    /// ConnectionManager 的专属 tokio Runtime 上（GPUI 的 smol executor
    /// 缺 tokio 上下文会 panic），因此 `runtime` 由调用方传入。
    async fn teardown_meter(
        meters: &Arc<RwLock<HashMap<String, MeterActorHandle>>>,
        core_registry: &Arc<tokio::sync::Mutex<MeterRegistry>>,
        runtime: &tokio::runtime::Handle,
        db_pool: &Option<SqlitePool>,
        persist_tx: &Option<mpsc::Sender<PersistRequest>>,
        address: &str,
    ) -> Result<(), String> {
        let Some(handle) = meters.write().remove(address) else {
            return Err(format!("meter {address} not found"));
        };
        if let Ok(bytes) = string_to_address(address) {
            core_registry.lock().await.unregister(&bytes);
        }
        if let Err(error) = handle.send_admin_command(AdminCommand::Shutdown).await {
            // actor 已退出时通道已关闭；数据库清理照常进行
            tracing::warn!(address, %error, "删除电表时关闭 actor 失败，继续清理数据");
        }
        let address = address.to_owned();
        let db_pool = db_pool.clone();
        let persist_tx = persist_tx.clone();
        let join = runtime.spawn(async move {
            if let Some(tx) = persist_tx {
                let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                if tx
                    .send(PersistRequest::Barrier { ack: ack_tx })
                    .await
                    .is_ok()
                {
                    let _ = ack_rx.await;
                }
            }
            if let Some(pool) = db_pool {
                PersistenceWorker::delete_meter_data(&pool, &address)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        });
        join.await.map_err(|error| error.to_string())?
    }

    /// Gracefully shutdown all meters (save virtual time and energy registers)
    ///
    /// This should be called before the application exits to ensure all
    /// data is persisted to the database.
    pub fn shutdown_all_meters(&self) {
        let meters = self.meters.read().clone();
        let connections = self.connections.clone();

        tracing::info!("开始优雅关闭所有电表，共 {} 个", meters.len());

        // 使用 ConnectionManager 的 runtime 来执行关闭操作
        // 使用 block_on 而不是 spawn，确保在函数返回前完成
        connections.runtime_handle().block_on(async move {
            let mut shutdown_tasks = Vec::new();

            for (address, handle) in meters {
                let addr = address.clone();
                let h = handle.clone();
                shutdown_tasks.push(async move {
                    match h.send_admin_command(AdminCommand::Shutdown).await {
                        Ok(_) => {
                            tracing::info!("电表 {} 关闭命令已发送", addr);
                        }
                        Err(e) => {
                            tracing::warn!("电表 {} 关闭失败: {}", addr, e);
                        }
                    }
                });
            }

            // 等待所有关闭命令发送完成
            futures::future::join_all(shutdown_tasks).await;

            // 给 Actor 一点时间完成 graceful_shutdown
            // 这里只需要很短的时间，因为 save_virtual_time 是直接数据库操作
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

            tracing::info!("所有电表关闭完成");
        });
    }
}

impl Global for AppBackend {}
