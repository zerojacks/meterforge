// Actor 模块 - 电表Actor实现
//
// 按设计方案 4.5 节实现 MeterActor

pub mod messages;
pub mod meter_actor;
pub mod registry;

pub use messages::{AdminCommand, EngineMsg, ProtocolCommand, RegistryMsg, TickMsg};
pub use meter_actor::{MeterActor, MeterActorConfig, MeterActorHandle};
pub use registry::{address_to_string, string_to_address, MeterRegistry};
