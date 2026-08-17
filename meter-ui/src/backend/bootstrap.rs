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
    router::RouterConfig,
    simulation::{LoadProfile, VirtualMeter, VirtualMeterConfig},
    ConnectionManager,
};
use parking_lot::RwLock;
use rand::Rng;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc, Mutex};

/// Starts the demonstration backend and binds its snapshots to the UI store.
/// The window and individual views do not participate in backend setup.
pub fn initialize(registry: Arc<RwLock<MeterRegistry>>, cx: &mut App) {
    let core_registry = Arc::new(Mutex::new(CoreMeterRegistry::new()));
    let (connections, receiver) = ConnectionManager::new(core_registry.clone());
    connections.start_router(core_registry.clone(), RouterConfig::default(), receiver);

    let handles = Arc::new(RwLock::new(HashMap::new()));
    cx.set_global(AppBackend::new(connections, handles.clone()));
    spawn_demo_meters(registry, core_registry, handles, cx);
}

fn spawn_demo_meters(
    registry: Arc<RwLock<MeterRegistry>>,
    core_registry: Arc<Mutex<CoreMeterRegistry>>,
    handles: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
    cx: &mut App,
) {
    let (tick_tx, _) = broadcast::channel::<TickMsg>(16);
    let mut registrations = Vec::new();
    let mut rng = rand::thread_rng();

    for number in 1..=10 {
        let address_bytes = [number as u8, 0, 0, 0, 0, 0];
        let address = format!("{number:012}");
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

        let (command_tx, command_rx) = mpsc::channel(100);
        let actor = MeterActor::new(
            VirtualMeter::new(config),
            tick_tx.subscribe(),
            command_rx,
            MeterActorConfig {
                address: address_bytes,
                cmd_queue_capacity: 100,
                enable_persistence: false,
                db_pool: None,
                registry_tx: None,
                snapshot_tx: Some(snapshot_tx),
            },
        );
        let handle = MeterActorHandle::new(command_tx, address_bytes);
        cx.background_executor()
            .spawn(async move {
                actor.run().await;
            })
            .detach();
        handles.write().insert(address, handle.clone());
        registrations.push((address_bytes, handle));
    }

    cx.background_executor()
        .spawn(async move {
            let mut registry = core_registry.lock().await;
            for (address, handle) in registrations {
                let _ = registry.register(address, handle);
            }
        })
        .detach();
    cx.background_executor()
        .spawn(async move {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                let _ = tick_tx.send(TickMsg {
                    wall_elapsed: Duration::from_secs(1),
                    time_scale: 1.0,
                });
            }
        })
        .detach();
}
