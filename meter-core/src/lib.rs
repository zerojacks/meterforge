// 虚拟 DL/T 645-2007 电表模拟器 - 核心引擎
// 版本: 0.1.0

pub mod actor;
pub mod communication_log;
pub mod connection;
pub mod error;
pub mod persistence;
pub mod protocol;
pub mod router;
pub mod simulation;
pub mod snapshot;
pub mod transport;

pub use connection::{
    ConnectionCommand, ConnectionManager, ConnectionResult, ConnectionStatus, SerialDataBits,
    SerialParity, SerialSettings, SerialStopBits,
};
pub use error::{MeterError, Result};
pub use snapshot::MeterSnapshot;

// 导出核心类型
pub use simulation::{
    EnergyType, FreezeType, LoadProfile, PhysicsConfig, PhysicsEngine, VirtualMeter,
    VirtualMeterConfig,
};

/// 快速创建默认虚拟电表
pub fn create_default_meter() -> VirtualMeter {
    VirtualMeter::default()
}

/// 创建自定义虚拟电表
pub fn create_meter(address: &str, load_profile: LoadProfile) -> Result<VirtualMeter> {
    let addr_bytes = crate::protocol::format::parse_address(address)?;

    let mut config = VirtualMeterConfig::default();
    config.address = addr_bytes;
    config.physics_config.load_model.profile = load_profile;

    Ok(VirtualMeter::new(config))
}
