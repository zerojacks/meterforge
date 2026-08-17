// 设置相关视图:连接配置、仿真参数、参数下发对话框。

mod connection_config;
pub mod parameter_dialogs;
mod simulation_config_dialog;

pub use connection_config::ConnectionConfigView;
pub use simulation_config_dialog::SimulationConfigPanel;
