use super::AppBackend;
use crate::{
    state::{MeterRegistry, MeterState},
    types::MeterSnapshot,
};
use gpui::*;
use meter_core::{
    actor::{
        MeterActor, MeterActorConfig, MeterActorHandle, MeterRegistry as CoreMeterRegistry, TickMsg,
    },
    persistence::PersistenceConfig,
    router::RouterConfig,
    simulation::{LoadProfile, VirtualMeter, VirtualMeterConfig},
    ConnectionManager,
};
use parking_lot::RwLock;
use rand::Rng;
use sqlx::SqlitePool;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc, Mutex};

/// 是否为这次启动打开持久化。
///
/// demo 数据是纯本地仿真，跑起来就有 10 张表在走时；打开持久化后每张表的
/// 电能寄存器 / 负荷记录 / 冻结快照都会落到 `./data/meters.db`，admin 命令
/// （改参数、改密码等）也会真正写库。想临时关掉（比如只想看 UI 效果、不想
/// 产生 db 文件）就把这里改成 `false`——不再需要靠 `MeterActorConfig`
/// 里那个未接线的字段了，这里是真正生效的开关。
const ENABLE_PERSISTENCE: bool = true;

/// Starts the demonstration backend and binds its snapshots to the UI store.
/// The window and individual views do not participate in backend setup.
pub fn initialize(registry: Arc<RwLock<MeterRegistry>>, cx: &mut App) {
    let core_registry = Arc::new(Mutex::new(CoreMeterRegistry::new()));
    let (connections, receiver) = ConnectionManager::new(core_registry.clone());
    connections.start_router(core_registry.clone(), RouterConfig::default(), receiver);

    // PersistenceWorker + 它用的 SqlitePool 都要活在 ConnectionManager 那个
    // 专属 tokio Runtime 里（sqlx 的 runtime-tokio feature、tokio::time 都
    // 硬依赖真实的 tokio Runtime，GPUI 的 background_executor 是 smol，没有
    // 这个上下文，涉及这些的代码会直接 panic）。
    let persistence = if ENABLE_PERSISTENCE {
        match connections.start_persistence(PersistenceConfig::default()) {
            Ok((pool, persist_tx)) => Some((pool, persist_tx)),
            Err(error) => {
                tracing::error!("持久化初始化失败，本次运行将不落库: {}", error);
                None
            }
        }
    } else {
        None
    };

    let handles = Arc::new(RwLock::new(HashMap::new()));
    cx.set_global(AppBackend::new(connections.clone(), handles.clone()));
    spawn_demo_meters(registry, core_registry, handles, connections, persistence, cx);
}

fn spawn_demo_meters(
    registry: Arc<RwLock<MeterRegistry>>,
    core_registry: Arc<Mutex<CoreMeterRegistry>>,
    handles: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
    connections: ConnectionManager,
    persistence: Option<(SqlitePool, mpsc::Sender<meter_core::persistence::PersistRequest>)>,
    cx: &mut App,
) {
    let (tick_tx, _) = broadcast::channel::<TickMsg>(16);
    let mut registrations = Vec::new();
    let mut rng = rand::thread_rng();

    for number in 1..=10 {
        let address_bytes = [number as u8, 0, 0, 0, 0, 0];
        // 使用统一的地址格式化函数，确保与 VirtualMeter 内部使用的格式一致
        let address = meter_core::protocol::format::format_address(&address_bytes);
        let mut config = VirtualMeterConfig::default();
        config.address = address_bytes;
        config.physics_config.load_model.profile = if rng.gen_bool(0.5) {
            LoadProfile::Industrial
        } else {
            LoadProfile::Residential
        };

        let (snapshot_tx, snapshot_rx) = mpsc::unbounded_channel::<MeterSnapshot>();
        let entity = cx.new(|_| MeterState::new(address.clone()));
        entity.update(cx, |_, cx| {
            MeterState::start_update_loop(entity.clone(), snapshot_rx, cx)
        });
        registry.write().register(address.clone(), entity);

        // 有 persist_tx 就用 with_persistence 挂上去（高频、可容忍短暂丢失的
        // tick 驱动数据 -> PersistenceWorker 队列）；db_pool 是同一个连接池的
        // clone，给 admin 命令走的直写路径用（低频、要求写完才 ack）。
        let mut virtual_meter = match &persistence {
            Some((_, persist_tx)) => VirtualMeter::with_persistence(config, persist_tx.clone()),
            None => VirtualMeter::new(config),
        };
        let db_pool = persistence.as_ref().map(|(pool, _)| pool.clone());

        // 启动时从数据库把上次持久化的虚拟时钟/电能寄存器/表配置（仿真参数、
        // 冻结模式、结算日、负荷记录配置）读回来，覆盖掉刚刚 new() 出来的
        // 默认值——这是本轮要修的问题：之前只在改配置时写库，从没在启动时
        // 读过，所以每次重启 UI 看到的都是默认值。
        //
        // restore_from_database 是 async 的，这里在 GPUI 的同步启动路径里
        // 用 runtime_handle().block_on() 跑一次性等待，跟 connection.rs 里
        // start_tcp_server/start_tcp_client 的做法一致；10 张表、纯本地 SQLite
        // 读取，启动时阻塞一下可以接受。
        if let Some((pool, _)) = &persistence {
            match connections
                .runtime_handle()
                .block_on(virtual_meter.restore_from_database(pool))
            {
                Ok(true) => {
                    tracing::info!("[bootstrap] 表 {} 已从数据库恢复配置", address);
                }
                Ok(false) => {
                    // 数据库里还没有这张表的记录，是首次启动，保留默认值
                    tracing::info!("[bootstrap] 表 {} 是首次启动，使用默认配置", address);
                }
                Err(e) => {
                    tracing::warn!("[bootstrap] 表 {} 恢复数据库状态失败，使用默认配置: {}", address, e);
                }
            }
        }

        let (command_tx, command_rx) = mpsc::channel(100);
        let actor = MeterActor::new(
            virtual_meter,
            tick_tx.subscribe(),
            command_rx,
            MeterActorConfig {
                address: address_bytes,
                cmd_queue_capacity: 100,
                enable_persistence: persistence.is_some(),
                db_pool,
                registry_tx: None,
                snapshot_tx: Some(snapshot_tx),
            },
        );
        let handle = MeterActorHandle::new(command_tx, address_bytes);
        // 关键点：spawn 到 ConnectionManager 的专属 tokio Runtime 上，
        // 不能用 cx.background_executor()（GPUI/smol，没有 tokio Runtime
        // 上下文）。MeterActor::run() 内部用了 tokio::select!，
        // VirtualMeter 的 tick 路径现在改成了 try_send（不需要 Runtime），
        // 但 admin 命令路径里对 db_pool 的 sqlx 调用仍然需要真正的 tokio
        // Runtime，所以统一放到这里最安全。
        connections.spawn(async move {
            actor.run().await;
        });
        handles.write().insert(address, handle.clone());
        registrations.push((address_bytes, handle));
    }

    connections.spawn(async move {
        let mut registry = core_registry.lock().await;
        for (address, handle) in registrations {
            let _ = registry.register(address, handle);
        }
    });

    connections.spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = tick_tx.send(TickMsg {
                wall_elapsed: Duration::from_secs(1),
                time_scale: 1.0,
            });
        }
    });
}