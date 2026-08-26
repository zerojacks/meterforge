use gpui::*;
use meter_core::actor::{string_to_address, AdminCommand, MeterActorHandle};
use meter_core::persistence::{PersistRequest, PersistenceWorker};
use meter_core::simulation::SimulationConfig;
use meter_core::snapshot::FreezeSnapshotSummary;
use meter_core::ConnectionManager;
use parking_lot::RwLock;
use sqlx::SqlitePool;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;

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
    /// 持久化句柄（`ENABLE_PERSISTENCE = false` 时为 None）：删除电表时
    /// 用于排空批量写队列并清理数据库。
    db_pool: Option<SqlitePool>,
    persist_tx: Option<mpsc::Sender<PersistRequest>>,
}

impl AppBackend {
    pub fn new(
        connections: ConnectionManager,
        meters: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
        persistence: Option<(SqlitePool, mpsc::Sender<PersistRequest>)>,
    ) -> Self {
        Self {
            connections,
            meters,
            db_pool: persistence.as_ref().map(|(pool, _)| pool.clone()),
            persist_tx: persistence.map(|(_, tx)| tx),
        }
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
            let results = futures::future::join_all(targets.iter().map(|handle| {
                handle.send_admin_command(AdminCommand::ClearLoadProfileHistory)
            }))
            .await;
            let success = results.iter().filter(|r| r.is_ok()).count();
            (success, total)
        })
    }

    /// 删除一块表（电表列表"删除"入口）：
    /// 1. 从路由注册表与 actor 句柄表摘除，之后的协议帧/admin 命令都不再命中它；
    /// 2. 优雅关闭 actor（它会把最终数据塞进持久化队列后停止）；
    /// 3. 等持久化队列排空（Barrier ack 时这批写入已提交）；
    /// 4. 清除该表在数据库的全部行——下次启动按 `meters` 表恢复存量表时，
    ///    被删除的表不会再被建回来。
    ///
    /// 步骤 3/4 涉及 sqlx 与 oneshot 等待，必须跑在 ConnectionManager 的专属
    /// tokio Runtime 上（GPUI 的 smol executor 缺 tokio 上下文会 panic）。
    pub fn remove_meter(&self, address: &str, cx: &App) -> Task<Result<(), String>> {
        let Some(handle) = self.meters.write().remove(address) else {
            return Task::ready(Err(format!("meter {address} not found")));
        };
        let core_registry = self.connections.registry();
        let runtime = self.connections.runtime_handle();
        let db_pool = self.db_pool.clone();
        let persist_tx = self.persist_tx.clone();
        let address = address.to_owned();

        cx.background_executor().spawn(async move {
            if let Ok(bytes) = string_to_address(&address) {
                core_registry.lock().await.unregister(&bytes);
            }
            if let Err(error) = handle.send_admin_command(AdminCommand::Shutdown).await {
                // actor 已退出时通道已关闭；数据库清理照常进行
                tracing::warn!(%address, %error, "删除电表时关闭 actor 失败，继续清理数据");
            }
            let join = runtime.spawn(async move {
                if let Some(tx) = persist_tx {
                    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                    if tx.send(PersistRequest::Barrier { ack: ack_tx })
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
        })
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
