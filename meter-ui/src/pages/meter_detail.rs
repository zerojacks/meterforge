//! Meter detail shell: navigation, dialog orchestration, and snapshot lookup.
//! The tab bodies and dialogs live in their own components.

use crate::{
    backend::{AppBackend, MeterAction},
    state::GlobalMeterRegistry,
    types::MeterSnapshot,
};
use chrono::{Datelike, Timelike, Utc};
use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{
    badge::Badge,
    label::Label,
    resizable::{resizable_panel, v_resizable},
    tab::{Tab, TabBar},
    *,
};

use super::communication_log_panel::CommunicationLogPanel;
use crate::settings::parameter_dialogs::{
    BaudrateDialog, ClearOperationDialog, ClearType, PasswordDialog, TimeSettingDialog,
    TouConfigDialog,
};
use crate::settings::SimulationConfigPanel;
use meter_core::snapshot::FreezeSnapshotSummary;

/// "负荷记录"标签页每次最多拉取的采样条数（跨全部数据类型/通道）。
const LOAD_PROFILE_HISTORY_LIMIT: u32 = 200;

#[derive(Clone, Copy, Default)]
enum DetailTab {
    #[default]
    RealTime,
    Parameters,
    Simulation,
    Events,
    Freezes,
    LoadProfile,
}

impl DetailTab {
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Parameters,
            2 => Self::Simulation,
            3 => Self::Events,
            4 => Self::Freezes,
            5 => Self::LoadProfile,
            _ => Self::RealTime,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::RealTime => 0,
            Self::Parameters => 1,
            Self::Simulation => 2,
            Self::Events => 3,
            Self::Freezes => 4,
            Self::LoadProfile => 5,
        }
    }
}

pub struct MeterDetailView {
    address: String,
    active_tab: DetailTab,
    log_panel: Entity<CommunicationLogPanel>,
    simulation_panel: Entity<SimulationConfigPanel>,
    /// 合并了数据库历史并去重后的冻结数据；切到"冻结数据"tab 时按需异步加载，
    /// 加载完成前 UI 用 snapshot 里的实时快照兜底（见 meter_history::freezes）。
    freeze_history: Option<Vec<FreezeSnapshotSummary>>,
    freeze_history_loading: bool,
    /// 数据库里最近的负荷记录采样；切到"负荷记录"tab 时按需异步加载。
    load_profile_history: Option<Vec<meter_core::snapshot::LoadRecordSnapshot>>,
    load_profile_history_loading: bool,
}

impl MeterDetailView {
    pub fn new(address: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = cx.global::<AppBackend>().clone();
        let log_panel = cx.new(|cx| {
            CommunicationLogPanel::new(backend.connections.communication_logs(), window, cx)
        });
        let mut receiver = backend.connections.subscribe_communication_logs();
        let panel = log_panel.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(entry) = receiver.recv().await {
                if this
                    .update(cx, |_, cx| {
                        panel.update(cx, |panel, cx| panel.append(entry, cx));
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let simulation = cx
            .global::<GlobalMeterRegistry>()
            .0
            .read()
            .get(&address)
            .map(|entity| entity.read(cx).snapshot.simulation.clone())
            .unwrap_or_else(|| MeterSnapshot::default_with_address(address.clone()).simulation);
        let simulation_address = address.clone();
        let fault_address = address.clone();
        let simulation_panel = cx.new(|cx| {
            SimulationConfigPanel::new(&simulation, window, cx)
                .on_confirm(move |settings, _, cx| {
                    let backend = cx.global::<AppBackend>();
                    backend.dispatch(
                        simulation_address.clone(),
                        MeterAction::ApplySimulationConfig {
                            config: settings.simulation,
                        },
                        cx,
                    );
                    backend.dispatch(
                        simulation_address.clone(),
                        MeterAction::ApplyFreezeConfig {
                            timed_mode: settings.freeze.timed_mode,
                            instant_mode: settings.freeze.instant_mode,
                            appointment_mode: settings.freeze.appointment_mode,
                            hourly_mode: settings.freeze.hourly_mode,
                            daily_mode: settings.freeze.daily_mode,
                            daily_time: settings.freeze.daily_time,
                            hourly_start: settings.freeze.hourly_start,
                            hourly_interval_min: settings.freeze.hourly_interval_min,
                            appointment_time: settings.freeze.appointment_time,
                        },
                        cx,
                    );
                    backend.dispatch(
                        simulation_address.clone(),
                        MeterAction::ApplySettlementDays {
                            days: settings.settlement_days,
                            hours: settings.settlement_hours,
                        },
                        cx,
                    );
                    backend.dispatch(
                        simulation_address.clone(),
                        MeterAction::ApplyLoadRecordConfig {
                            mode_word: settings.load_record.mode_word,
                            start_time: settings.load_record.start_time,
                            intervals: settings.load_record.intervals,
                        },
                        cx,
                    );
                })
                .on_inject_fault(move |event_type, phase, active, _, cx| {
                    cx.global::<AppBackend>().dispatch(
                        fault_address.clone(),
                        MeterAction::InjectFault {
                            event_type,
                            phase,
                            active,
                        },
                        cx,
                    );
                })
        });
        Self {
            address,
            active_tab: DetailTab::default(),
            log_panel,
            simulation_panel,
            freeze_history: None,
            freeze_history_loading: false,
            load_profile_history: None,
            load_profile_history_loading: false,
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    fn select_tab(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = DetailTab::from_index(index);

        // 只在切进"冻结数据"tab、且还没加载过（也没有正在加载）时才发起查询，
        // 这样启动阶段和切到其它 tab 都不会碰数据库；切走再切回来也不会重复查。
        if matches!(self.active_tab, DetailTab::Freezes)
            && self.freeze_history.is_none()
            && !self.freeze_history_loading
        {
            self.freeze_history_loading = true;
            let address = self.address.clone();
            let backend = cx.global::<AppBackend>().clone();
            let task = backend.load_freeze_history(address, cx);
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.freeze_history_loading = false;
                    match result {
                        Ok(data) => {
                            this.freeze_history = Some(data);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "加载冻结历史失败");
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        // 同理，"负荷记录"tab 也只在首次切入、且还没加载过/没有正在加载时打库。
        if matches!(self.active_tab, DetailTab::LoadProfile)
            && self.load_profile_history.is_none()
            && !self.load_profile_history_loading
        {
            self.load_profile_history_loading = true;
            let address = self.address.clone();
            let backend = cx.global::<AppBackend>().clone();
            let task =
                backend.load_load_profile_history(address, LOAD_PROFILE_HISTORY_LIMIT, cx);
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.load_profile_history_loading = false;
                    match result {
                        Ok(data) => {
                            this.load_profile_history = Some(data);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "加载负荷记录失败");
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        cx.notify();
    }

    pub(crate) fn show_time_setting_dialog(
        &mut self,
        snapshot: &MeterSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = chrono::DateTime::from_timestamp_millis(snapshot.virtual_time_ms)
            .unwrap_or_else(|| Utc::now().into())
            .with_timezone(&chrono::Utc);
        let address = self.address.clone();

        // 只在“打开对话框”这一刻创建一次 Entity，避免 open_dialog 的 build
        // 闭包在每次 Root 重渲染时都重新 new 出一个全新的对话框状态。
        let dialog_entity = cx.new(|cx| {
            TimeSettingDialog::new(initial, window, cx).on_confirm(move |value, _, cx| {
                cx.global::<AppBackend>().dispatch(
                    address.clone(),
                    MeterAction::SetVirtualTime {
                        year: value.year() as u16,
                        month: value.month() as u8,
                        day: value.day() as u8,
                        hour: value.hour() as u8,
                        minute: value.minute() as u8,
                        second: value.second() as u8,
                    },
                    cx,
                );
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("设置电表时间")
                .w(px(500.))
                .content({
                    let dialog_entity = dialog_entity.clone();
                    move |content, _, _| content.child(dialog_entity.clone())
                })
        })
    }

    pub(crate) fn show_password_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = self.address.clone();

        let dialog_entity = cx.new(|cx| {
            PasswordDialog::new(window, cx).on_confirm(move |level, _, password, _, cx| {
                cx.global::<AppBackend>().dispatch(
                    address.clone(),
                    MeterAction::ChangePassword {
                        level,
                        new_password: password,
                    },
                    cx,
                );
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("修改密码")
                .w(px(500.))
                .content({
                    let dialog_entity = dialog_entity.clone();
                    move |content, _, _| content.child(dialog_entity.clone())
                })
        })
    }

    pub(crate) fn show_baudrate_dialog(
        &mut self,
        _: &MeterSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = self.address.clone();

        let dialog_entity = cx.new(|cx| {
            BaudrateDialog::new(0x20, window, cx).on_confirm(move |baudrate, _, _, cx| {
                cx.global::<AppBackend>().dispatch(
                    address.clone(),
                    MeterAction::SetBaudrate { baudrate },
                    cx,
                );
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("修改通信速率")
                .w(px(500.))
                .content({
                    let dialog_entity = dialog_entity.clone();
                    move |content, _, _| content.child(dialog_entity.clone())
                })
        })
    }

    pub(crate) fn show_tou_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = self.address.clone();

        let dialog_entity = cx.new(|cx| {
            TouConfigDialog::new(window, cx).on_confirm(move |slots, _, cx| {
                cx.global::<AppBackend>().dispatch(
                    address.clone(),
                    MeterAction::SetTouConfig { time_slots: slots },
                    cx,
                );
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("费率时段表配置")
                .w(px(600.))
                .h(px(500.))
                .content({
                    let dialog_entity = dialog_entity.clone();
                    move |content, _, _| content.child(dialog_entity.clone())
                })
        })
    }

    pub(crate) fn show_clear_demand_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_clear_dialog(
            ClearType::MaxDemand,
            MeterAction::ClearMaxDemand,
            "最大需量清零",
            window,
            cx,
        );
    }
    pub(crate) fn show_clear_meter_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_clear_dialog(
            ClearType::Meter,
            MeterAction::ClearMeter,
            "电表清零",
            window,
            cx,
        );
    }

    fn open_clear_dialog(
        &self,
        clear_type: ClearType,
        action: MeterAction,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = self.address.clone();

        let dialog_entity = cx.new(|cx| {
            ClearOperationDialog::new(clear_type, window, cx).on_confirm(move |_, _, _, cx| {
                cx.global::<AppBackend>()
                    .dispatch(address.clone(), action.clone(), cx);
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title)
                .w(px(500.))
                .content({
                    let dialog_entity = dialog_entity.clone();
                    move |content, _, _| content.child(dialog_entity.clone())
                })
        })
    }

    fn snapshot(&self, cx: &App) -> MeterSnapshot {
        cx.global::<GlobalMeterRegistry>()
            .0
            .read()
            .get(&self.address)
            .map(|entity| entity.read(cx).snapshot.clone())
            .unwrap_or_else(|| MeterSnapshot::default_with_address(self.address.clone()))
    }

    /// 实时曲线历史（与快照同源）
    fn realtime_history(
        &self,
        cx: &App,
    ) -> std::collections::VecDeque<crate::state::RealtimeSample> {
        cx.global::<GlobalMeterRegistry>()
            .0
            .read()
            .get(&self.address)
            .map(|entity| entity.read(cx).history.clone())
            .unwrap_or_default()
    }
}

impl Render for MeterDetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let snapshot = self.snapshot(cx);
        let history = self.realtime_history(cx);
        let content = match self.active_tab {
            DetailTab::RealTime => {
                super::meter_realtime::render(&snapshot, &history, &theme).into_any_element()
            }
            DetailTab::Parameters => {
                super::meter_parameters::render(self, &snapshot, cx).into_any_element()
            }
            DetailTab::Simulation => self.simulation_panel.clone().into_any_element(),
            DetailTab::Events => super::meter_history::events(&snapshot, &theme).into_any_element(),
            DetailTab::Freezes => super::meter_history::freezes(
                &snapshot,
                self.freeze_history.as_deref(),
                self.freeze_history_loading,
                &theme,
            )
            .into_any_element(),
            DetailTab::LoadProfile => super::meter_history::load_profile(
                &snapshot,
                self.load_profile_history.as_deref(),
                self.load_profile_history_loading,
                &theme,
            )
            .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(52.))
                    .px_5()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                Label::new(format!("电表 {}", self.address))
                                    .text_lg()
                                    .font_semibold(),
                            )
                            .child(Badge::new().child(if snapshot.is_online {
                                "在线"
                            } else {
                                "离线"
                            })),
                    )
                    .child({
                        let virtual_time =
                            chrono::DateTime::from_timestamp_millis(snapshot.virtual_time_ms)
                                .map(|dt| {
                                    dt.with_timezone(&Utc)
                                        .format("%Y-%m-%d %H:%M:%S")
                                        .to_string()
                                })
                                .unwrap_or_else(|| "--".to_string());
                        Label::new(format!("虚拟时间 {}", virtual_time))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                    }),
            )
            .child(
                TabBar::new("detail-tabs")
                    .px_4()
                    .selected_index(self.active_tab.index())
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        this.select_tab(*index, window, cx)
                    }))
                    .child(Tab::new().label("实时数据"))
                    .child(Tab::new().label("参数设置"))
                    .child(Tab::new().label("模拟配置"))
                    .child(Tab::new().label("事件记录"))
                    .child(Tab::new().label("冻结数据"))
                    .child(Tab::new().label("负荷记录")),
            )
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable(format!("detail-layout-{}", self.address))
                        .child(
                            resizable_panel().child(
                                div()
                                    .size_full()
                                    .overflow_y_scrollbar()
                                    .p_6()
                                    .child(content),
                            ),
                        )
                        .child(
                            resizable_panel()
                                .size(px(180.))
                                .size_range(px(120.)..px(480.))
                                .child(
                                    div()
                                        .size_full()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(self.log_panel.clone()),
                                ),
                        ),
                ),
            )
    }
}