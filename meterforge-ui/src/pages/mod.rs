// 页面级视图:主工作区、仪表列表、仪表详情及其标签页。

mod application_workspace;
mod custom_data;
mod communication_log_panel;
mod meter_detail;
mod meter_history;
mod meter_list;
mod meter_parameters;
mod meter_realtime;

pub use application_workspace::ApplicationWorkspace;
pub use meter_detail::MeterDetailView;
pub use meter_list::MeterListView;
