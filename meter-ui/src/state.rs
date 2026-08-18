// UI 状态管理 - 被动接收 meter-core 推送的数据更新

use crate::types::MeterSnapshot;
use gpui::*;
use meter_core::ConnectionStatus;
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;

/// 实时曲线采样点（保留最近 HISTORY_CAPACITY 次快照）
#[derive(Clone, Copy, Debug)]
pub struct RealtimeSample {
    /// 虚拟时钟（毫秒时间戳）
    pub time_ms: i64,
    pub voltage_a: f32,
    pub voltage_b: f32,
    pub voltage_c: f32,
    pub current_a: f32,
    pub current_b: f32,
    pub current_c: f32,
    pub active_power_kw: f32,
    pub reactive_power_kvar: f32,
}

/// 实时曲线最多保留的采样点数（约 2 分钟 @1s 推送间隔）
const HISTORY_CAPACITY: usize = 120;

/// 单表状态 Entity
///
/// 设计原则：UI 侧只持有快照数据，不直接与 VirtualMeter 交互
/// 数据更新由 meter-core 后端主动推送
pub struct MeterState {
    pub address: String,
    pub snapshot: MeterSnapshot,
    /// 实时曲线历史（随快照推送滚动追加）
    pub history: VecDeque<RealtimeSample>,
}

// 实现 EventEmitter trait（即使我们不发射事件，也需要实现）
impl EventEmitter<()> for MeterState {}

impl MeterState {
    /// 创建新的MeterState（初始快照为默认值，等待 MeterActor 推送）
    pub fn new(address: String) -> Self {
        Self {
            address: address.clone(),
            snapshot: MeterSnapshot::default_with_address(address),
            history: VecDeque::new(),
        }
    }

    /// 用已知快照创建 MeterState（用于启动时已经从数据库恢复出配置的场景，
    /// 避免 UI 先渲染一份默认快照、等第一次 tick 推送才刷新成真实数据——
    /// 像"模拟配置"这种只在构造时读取一次快照的表单组件，如果构造时拿到
    /// 的是默认值，之后不会自动刷新，只有切换电表重新构造才会更新）
    pub fn with_snapshot(snapshot: MeterSnapshot) -> Self {
        Self {
            address: snapshot.address.clone(),
            snapshot,
            history: VecDeque::new(),
        }
    }

    /// 启动后台监听任务，被动接收来自 meter-core 的状态更新
    ///
    /// 使用 cx.spawn() + entity.update() 模式，这是 GPUI 推荐的异步更新方式
    pub fn start_update_loop(
        entity: Entity<Self>,
        mut update_rx: mpsc::UnboundedReceiver<MeterSnapshot>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(new_snapshot) = update_rx.recv().await {
                let result = this.update(cx, |state, cx| {
                    // 同步采样点（虚拟时间未推进时跳过，避免平线堆积）
                    if new_snapshot.virtual_time_ms
                        != state.history.back().map(|s| s.time_ms).unwrap_or(0)
                    {
                        state.history.push_back(RealtimeSample {
                            time_ms: new_snapshot.virtual_time_ms,
                            voltage_a: new_snapshot.voltage_a,
                            voltage_b: new_snapshot.voltage_b,
                            voltage_c: new_snapshot.voltage_c,
                            current_a: new_snapshot.current_a,
                            current_b: new_snapshot.current_b,
                            current_c: new_snapshot.current_c,
                            active_power_kw: new_snapshot.active_power_kw,
                            reactive_power_kvar: new_snapshot.reactive_power_kvar,
                        });
                        while state.history.len() > HISTORY_CAPACITY {
                            state.history.pop_front();
                        }
                    }
                    state.snapshot = new_snapshot;
                    cx.notify(); // 触发 UI 重新渲染
                });

                if result.is_err() {
                    // Entity 已被销毁，退出循环
                    break;
                }
            }
        })
        .detach();
    }
}

/// 全局表注册表
pub struct MeterRegistry {
    meters: HashMap<String, Entity<MeterState>>,
}

impl MeterRegistry {
    pub fn new() -> Self {
        Self {
            meters: HashMap::new(),
        }
    }

    pub fn register(&mut self, address: String, entity: Entity<MeterState>) {
        self.meters.insert(address, entity);
    }

    pub fn get(&self, address: &str) -> Option<&Entity<MeterState>> {
        self.meters.get(address)
    }

    pub fn all_addresses(&self) -> Vec<String> {
        let mut addresses: Vec<_> = self.meters.keys().cloned().collect();
        addresses.sort();
        addresses
    }

    pub fn count(&self) -> usize {
        self.meters.len()
    }
}

/// 全局注册表包装器
#[derive(Clone)]
pub struct GlobalMeterRegistry(pub Arc<RwLock<MeterRegistry>>);

impl Global for GlobalMeterRegistry {}

#[derive(Default)]
pub struct ConnectionStatusStore {
    snapshot: ConnectionStatus,
}

impl EventEmitter<()> for ConnectionStatusStore {}

impl ConnectionStatusStore {
    pub fn snapshot(&self) -> ConnectionStatus {
        self.snapshot.clone()
    }

    pub fn sync(&mut self, snapshot: ConnectionStatus) {
        self.snapshot = snapshot;
    }
}

#[derive(Clone)]
pub struct GlobalConnectionStatus(pub Entity<ConnectionStatusStore>);

impl Global for GlobalConnectionStatus {}