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
    button::{Button, ButtonVariants},
    label::Label,
    resizable::{resizable_panel, v_resizable},
    tab::{Tab, TabBar},
    *,
};

use super::communication_log_panel::CommunicationLogPanel;
use super::meter_history::{self, FreezeFilter};
use crate::settings::parameter_dialogs::{
    BaudrateDialog, ClearOperationDialog, ClearType, PasswordDialog, SyncConfirmDialog,
    TimeSettingDialog, TouConfigDialog,
};
use crate::settings::SimulationConfigPanel;
use gpui_component::notification::Notification;
use meter_core::snapshot::{EventSnapshot, FreezeSnapshotSummary, LoadRecordSnapshot};

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
    /// 加载完成前 UI 用 snapshot 里的实时快照兜底（见 `sync_freezes_list`）。
    freeze_history: Option<Vec<FreezeSnapshotSummary>>,
    freeze_history_loading: bool,
    /// 数据库里最近的负荷记录采样；切到"负荷记录"tab 时按需异步加载。
    load_profile_history: Option<Vec<meter_core::snapshot::LoadRecordSnapshot>>,
    load_profile_history_loading: bool,

    // ---- 事件记录 / 冻结数据 / 负荷记录三个 tab 的虚拟滚动列表状态 ----
    // 与 CommunicationLogPanel 同一套模式：ListState 常驻、每次 render 时按
    // 最新数据 reset 条目数；items 缓存本轮渲染用的数据，供 cx.processor
    // 的逐行渲染回调按下标读取。
    events_list_state: ListState,
    events_items: Vec<EventSnapshot>,
    freezes_list_state: ListState,
    freezes_items: Vec<FreezeSnapshotSummary>,
    /// 冻结数据的触发类型筛选，默认"全部"；可切到"月冻结（结算日）"只看结算日数据。
    freeze_filter: FreezeFilter,
    load_profile_list_state: ListState,
    load_profile_items: Vec<LoadRecordSnapshot>,
    /// 异步任务（参数同步）完成后待弹的通知：(是否成功, 文案)。
    /// `Notification` 不是 Send，只能在主线程构建，所以这里只存消息。
    pending_notification: Option<(bool, String)>,
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
                // "应用到所有表"：同样的四组配置广播给全部表（含当前表，等效于
                // 应用 + 同步一步完成），各表 actor 自行落库。
                .on_sync_all(move |settings, window, cx| {
                    let backend = cx.global::<AppBackend>();
                    let count = backend.dispatch_all(
                        None,
                        MeterAction::ApplySimulationConfig {
                            config: settings.simulation,
                        },
                        cx,
                    );
                    backend.dispatch_all(
                        None,
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
                    backend.dispatch_all(
                        None,
                        MeterAction::ApplySettlementDays {
                            days: settings.settlement_days,
                            hours: settings.settlement_hours,
                        },
                        cx,
                    );
                    backend.dispatch_all(
                        None,
                        MeterAction::ApplyLoadRecordConfig {
                            mode_word: settings.load_record.mode_word,
                            start_time: settings.load_record.start_time,
                            intervals: settings.load_record.intervals,
                        },
                        cx,
                    );
                    window.push_notification(
                        Notification::success(format!(
                            "配置同步：已向 {count} 块表下发，各表正在应用并写入数据库"
                        )),
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
            events_list_state: ListState::new(0, ListAlignment::Top, px(120.)),
            events_items: Vec::new(),
            freezes_list_state: ListState::new(0, ListAlignment::Top, px(120.)),
            freezes_items: Vec::new(),
            freeze_filter: FreezeFilter::default(),
            load_profile_list_state: ListState::new(0, ListAlignment::Top, px(120.)),
            load_profile_items: Vec::new(),
            pending_notification: None,
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
            let task = backend.load_load_profile_history(address, LOAD_PROFILE_HISTORY_LIMIT, cx);
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
            .unwrap_or_else(|| meter_core::simulation::local_now_as_utc().into())
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
            dialog.title("设置电表时间").w(px(500.)).content({
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
            dialog.title("修改密码").w(px(500.)).content({
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
            dialog.title("修改通信速率").w(px(500.)).content({
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

    /// "参数设置"页的一键同步入口：确认后读取当前表已生效的协议参数
    /// （时间 / 密码 / 通信速率 / 费率时段表），下发给其他所有表并落库。
    pub(crate) fn show_sync_parameters_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = self.address.clone();
        let view = cx.entity().downgrade();

        let dialog_entity = cx.new(|_| {
            SyncConfirmDialog::new(
                "一键同步参数到所有表",
                "将把当前表的电表时间、密码、通信速率、费率时段表同步到其他所有电表，并写入数据库覆盖其现有值。此操作不可撤销。",
                "开始同步",
            )
            .on_confirm(move |_, cx| {
                let backend = cx.global::<AppBackend>().clone();
                let task = backend.sync_protocol_parameters(address.clone(), cx);
                let view = view.clone();
                cx.spawn(async move |_, cx| {
                    // Notification 不是 Send，异步侧只回传 (成功?, 文案)，
                    // 由视图在主线程 render 时构建并弹出。
                    let result = match task.await {
                        Ok(count) => (true, format!("参数同步完成：已同步到 {count} 块表")),
                        Err(error) => (false, format!("参数同步失败：{error}")),
                    };
                    let _ = view.update(cx, |view, cx| {
                        view.pending_notification = Some(result);
                        cx.notify();
                    });
                })
                .detach();
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("参数同步").w(px(500.)).content({
                let dialog_entity = dialog_entity.clone();
                move |content, _, _| content.child(dialog_entity.clone())
            })
        })
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
            dialog.title(title).w(px(500.)).content({
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

    // ========================================================================
    // 事件记录 / 冻结数据 / 负荷记录：三个 tab 共用"虚拟滚动列表"的实现模式。
    // 每次进入对应 tab 渲染时，先用最新数据 `sync_*_list` 刷新 items + reset
    // ListState 的条目数（数量没变则不动，避免每帧都重建列表丢滚动位置），
    // 再用 gpui 原生 `list()` 逐行渲染，天然支持大量记录下的虚拟滚动。
    // ========================================================================

    fn sync_events_list(&mut self, snapshot: &MeterSnapshot) {
        let items = snapshot.events.clone();
        if self.events_list_state.item_count() != items.len() {
            self.events_list_state.reset(items.len());
        }
        self.events_items = items;
    }

    fn render_event_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(event) = self.events_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        div()
            .id(("event-item", ix))
            .w_full()
            .child(meter_history::render_event_item(&event, &theme))
            .into_any_element()
    }

    /// 合并快照里的实时冻结数据与数据库历史（按 `(trigger, snapshot_time_ms)` 去重），
    /// 再按当前 `freeze_filter` 过滤，逻辑与原先的 `meter_history::freezes` 保持一致。
    fn sync_freezes_list(&mut self, snapshot: &MeterSnapshot) {
        let merged: Vec<FreezeSnapshotSummary> = match self.freeze_history.as_deref() {
            Some(hist) => {
                let mut seen: std::collections::HashSet<(String, i64)> = hist
                    .iter()
                    .map(|f| (f.trigger.clone(), f.snapshot_time_ms))
                    .collect();
                let mut combined = hist.to_vec();
                for freeze in &snapshot.freezes {
                    if seen.insert((freeze.trigger.clone(), freeze.snapshot_time_ms)) {
                        combined.push(freeze.clone());
                    }
                }
                combined.sort_by_key(|f| std::cmp::Reverse(f.snapshot_time_ms));
                combined
            }
            None => snapshot.freezes.clone(),
        };
        let filter = self.freeze_filter;
        let filtered: Vec<FreezeSnapshotSummary> = merged
            .into_iter()
            .filter(|f| filter.matches(&f.trigger))
            .collect();
        if self.freezes_list_state.item_count() != filtered.len() {
            self.freezes_list_state.reset(filtered.len());
        }
        self.freezes_items = filtered;
    }

    fn render_freeze_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(freeze) = self.freezes_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        div()
            .id(("freeze-item", ix))
            .w_full()
            .child(meter_history::render_freeze_item(&freeze, &theme))
            .into_any_element()
    }

    /// 合并快照里的实时负荷记录与数据库历史（按 `(class_id, sample_time_ms)` 去重），
    /// 逻辑与原先的 `meter_history::load_profile` 保持一致。
    fn sync_load_profile_list(&mut self, snapshot: &MeterSnapshot) {
        let items: Vec<LoadRecordSnapshot> = match self.load_profile_history.as_deref() {
            Some(hist) => {
                let mut seen: std::collections::HashSet<(u8, i64)> = snapshot
                    .load_records
                    .iter()
                    .map(|r| (r.class_id, r.sample_time_ms))
                    .collect();
                let mut combined: Vec<LoadRecordSnapshot> = snapshot.load_records.clone();
                for record in hist {
                    if seen.insert((record.class_id, record.sample_time_ms)) {
                        combined.push(record.clone());
                    }
                }
                combined
            }
            None => snapshot.load_records.clone(),
        };
        if self.load_profile_list_state.item_count() != items.len() {
            self.load_profile_list_state.reset(items.len());
        }
        self.load_profile_items = items;
    }

    fn render_load_profile_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(record) = self.load_profile_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        div()
            .id(("load-record-item", ix))
            .w_full()
            .child(meter_history::render_load_record_item(&record, &theme))
            .into_any_element()
    }

    /// "事件记录"tab：标题 + 计数 + 虚拟滚动列表。
    fn render_events_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let snapshot = self.snapshot(cx);
        self.sync_events_list(&snapshot);
        let count = self.events_items.len();

        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .child(Label::new("事件记录").text_2xl().font_semibold())
            .child(
                Label::new(format!("共 {count} 条记录，按发生时间倒序显示"))
                    .text_sm()
                    .text_color(theme.muted_foreground),
            );
        col = if count == 0 {
            col.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("暂无事件记录"),
            )
        } else {
            col.child(
                div()
                    .id("events-list")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        list(
                            self.events_list_state.clone(),
                            cx.processor(Self::render_event_item),
                        )
                        .with_sizing_behavior(ListSizingBehavior::Auto)
                        .size_full(),
                    ),
            )
        };
        col.into_any_element()
    }

    /// "冻结数据"tab：标题 + 计数 + 触发类型筛选条（含"月冻结（结算日）"）+ 虚拟滚动列表。
    fn render_freezes_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let snapshot = self.snapshot(cx);
        self.sync_freezes_list(&snapshot);
        let count = self.freezes_items.len();
        let loading = self.freeze_history_loading && self.freeze_history.is_none();
        let current_filter = self.freeze_filter;

        let filter_bar =
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(
                    FreezeFilter::ALL
                        .into_iter()
                        .enumerate()
                        .map(|(idx, filter)| {
                            let selected = filter == current_filter;
                            let btn = Button::new(("freeze-filter", idx))
                                .label(filter.label())
                                .small();
                            let btn = if selected { btn.primary() } else { btn.ghost() };
                            btn.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.freeze_filter = filter;
                                cx.notify();
                            }))
                        }),
                );

        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .child(Label::new("冻结数据").text_2xl().font_semibold())
            .child(
                Label::new(if loading {
                    "正在从数据库加载历史冻结数据…".to_string()
                } else {
                    format!("共 {count} 条冻结快照，按冻结时间倒序显示")
                })
                .text_sm()
                .text_color(theme.muted_foreground),
            )
            .child(filter_bar);
        col = if count == 0 {
            col.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if loading {
                        "加载中…"
                    } else {
                        "暂无符合条件的冻结数据"
                    }),
            )
        } else {
            col.child(
                div()
                    .id("freezes-list")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        list(
                            self.freezes_list_state.clone(),
                            cx.processor(Self::render_freeze_item),
                        )
                        .with_sizing_behavior(ListSizingBehavior::Auto)
                        .size_full(),
                    ),
            )
        };
        col.into_any_element()
    }

    /// "负荷记录"tab：标题 + 计数 + 虚拟滚动列表。
    fn render_load_profile_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let snapshot = self.snapshot(cx);
        self.sync_load_profile_list(&snapshot);
        let count = self.load_profile_items.len();
        let loading = self.load_profile_history_loading && self.load_profile_history.is_none();

        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .child(Label::new("负荷记录").text_2xl().font_semibold())
            .child(
                Label::new(if loading {
                    "正在从数据库加载负荷记录…".to_string()
                } else if self.load_profile_history.is_some() {
                    format!("共 {count} 条记录，按采样时间倒序显示")
                } else {
                    format!("最近 {count} 条记录（实时快照），按采样时间倒序显示")
                })
                .text_sm()
                .text_color(theme.muted_foreground),
            );
        col = if count == 0 {
            col.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if loading {
                        "加载中…"
                    } else {
                        "暂无负荷记录采样"
                    }),
            )
        } else {
            col.child(
                div()
                    .id("load-profile-list")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        list(
                            self.load_profile_list_state.clone(),
                            cx.processor(Self::render_load_profile_item),
                        )
                        .with_sizing_behavior(ListSizingBehavior::Auto)
                        .size_full(),
                    ),
            )
        };
        col.into_any_element()
    }
}

impl Render for MeterDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((success, message)) = self.pending_notification.take() {
            let notification = if success {
                Notification::success(message)
            } else {
                Notification::error(message)
            };
            window.push_notification(notification, cx);
        }
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
            // 事件记录/冻结数据/负荷记录三个 tab 用 gpui 原生 list() 做虚拟滚动，
            // 自己管理滚动，因此下面包裹容器不能再叠加 overflow_y_scrollbar。
            DetailTab::Events => self.render_events_tab(cx),
            DetailTab::Freezes => self.render_freezes_tab(cx),
            DetailTab::LoadProfile => self.render_load_profile_tab(cx),
        };
        let uses_virtual_list = matches!(
            self.active_tab,
            DetailTab::Events | DetailTab::Freezes | DetailTab::LoadProfile
        );
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
                        .child(resizable_panel().child({
                            let inner = div().size_full().p_6().child(content);
                            if uses_virtual_list {
                                // 虚拟滚动列表自己处理滚动，外层只需要是个有限高度的
                                // flex 容器（min_h_0 让 flex_1 的列表能正确收缩）。
                                inner.flex().flex_col().min_h_0().into_any_element()
                            } else {
                                inner.overflow_y_scrollbar().into_any_element()
                            }
                        }))
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
