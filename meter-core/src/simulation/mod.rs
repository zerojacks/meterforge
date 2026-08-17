// 模拟数据生成模块 - 基于物理模型

pub mod di_handler;
pub mod physics_engine;
pub mod state;
pub mod virtual_meter;

pub use di_handler::DIHandler;
pub use physics_engine::{
    LoadModelConfig, LoadProfile, PhysicsConfig, PhysicsEngine, SimulationConfig,
};
pub use state::{
    EnergyType, FreezeTrigger, FreezeType, MeterState, TimeSlot, TimeSlotTable, TouConfig,
};
pub use virtual_meter::{address_to_string, string_to_address, VirtualMeter, VirtualMeterConfig};
