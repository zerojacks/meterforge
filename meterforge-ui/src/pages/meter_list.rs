// 监控工作台：左侧连接/表列表，右侧当前电表详情。

use super::MeterDetailView;
use crate::backend::AppBackend;
use crate::components::MeterCard;
use crate::settings::parameter_dialogs::{AddMeterView, ModifyAddressView, SyncConfirmDialog};
use crate::state::{GlobalMeterRegistry, MeterState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::*;
use std::collections::HashSet;

/// 电表列表面板顶部"批量清除"入口区分的两种历史数据类型。
#[derive(Clone, Copy)]
enum ClearAllKind {
    FreezeHistory,
    LoadProfileHistory,
}

impl ClearAllKind {
    fn label(self) -> &'static str {
        match self {
            ClearAllKind::FreezeHistory => "冻结历史数据",
            ClearAllKind::LoadProfileHistory => "负荷记录数据",
        }
    }

    fn dialog_text(self) -> (&'static str, &'static str, &'static str) {
        match self {
            ClearAllKind::FreezeHistory => (
                "清除全部表的冻结历史数据",
                "将清空所有电表的冻结历史快照（内存与数据库），冻结相关配置不受影响。此操作不可撤销。",
                "清除全部",
            ),
            ClearAllKind::LoadProfileHistory => (
                "清除全部表的负荷记录数据",
                "将清空所有电表的负荷记录历史采样（内存与数据库），负荷记录配置不受影响。此操作不可撤销。",
                "清除全部",
            ),
        }
    }
}

pub struct MeterListView {
    all_addresses: Vec<String>,
    selected_address: Option<String>,
    /// 批量删除用的勾选集合（"删除选中"入口）。与单选的 `selected_address`
    /// 相互独立：勾选只影响批量操作，行点击仍然只是切换右侧详情。
    checked_addresses: HashSet<String>,
    subscriptions: Vec<Subscription>,
    detail_view: Option<Entity<MeterDetailView>>,
    address_search: Entity<InputState>,
    /// 左侧表列表的虚拟滚动状态，与冻结数据/通信日志同一套 `gpui::list` 模式。
    list_state: ListState,
    /// 最近一次渲染的过滤结果缓存，供 `render_meter_item` 按下标取地址。
    filtered_items: Vec<String>,
    /// 批量清除操作（全部表）完成后待弹的通知：(是否成功, 文案)。
    /// `Notification` 不是 Send，只能在主线程构建，所以这里只存消息。
    pending_notification: Option<(bool, String)>,
}

impl MeterListView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
        let selected_address = all_addresses.first().cloned();
        let address_search = cx.new(|cx| InputState::new(_window, cx).placeholder("搜索电表地址"));
        let list_state = ListState::new(all_addresses.len(), ListAlignment::Top, px(60.));
        let mut view = Self {
            filtered_items: all_addresses.clone(),
            all_addresses,
            selected_address,
            checked_addresses: HashSet::new(),
            subscriptions: Vec::new(),
            detail_view: None,
            address_search: address_search.clone(),
            list_state,
            pending_notification: None,
        };
        view.subscriptions
            .push(cx.subscribe(&address_search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
        view.subscribe_to_meters(cx);
        view
    }

    fn subscribe_to_meters(&mut self, cx: &mut Context<Self>) {
        let entities: Vec<Entity<MeterState>> = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            self.all_addresses
                .iter()
                .filter_map(|address| registry.get(address).cloned())
                .collect()
        };
        for entity in entities {
            self.subscriptions
                .push(cx.observe(&entity, |_this, _entity, cx| cx.notify()));
        }
    }

    fn filtered_addresses(&self, cx: &App) -> Vec<String> {
        let query = self.address_search.read(cx).value().trim().to_owned();
        self.all_addresses
            .iter()
            .filter(|address| query.is_empty() || address.contains(query.as_str()))
            .cloned()
            .collect()
    }

    fn select_meter(&mut self, address: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_address = Some(address);
        cx.notify();
    }

    /// 电表列表面板顶部"添加表"入口：填新地址，可选"复制自"某块已有表
    /// （连同其仿真/协议/冻结/负荷记录配置与历史数据一起复制，仅地址不同）。
    /// 与删除相对称：确认后调用 `AppBackend::add_meter`，成功后在
    /// `add_new_meter` 里完成 UI 侧的 Entity 创建与注册。
    fn show_add_meter_dialog(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backend = cx.global::<AppBackend>().clone();
        let existing_addresses = backend.meter_addresses();
        let view = cx.entity().downgrade();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(560.), px(430.)),
                cx,
            ))),
            titlebar: Some(TitleBar::title_bar_options()),
            app_owns_titlebar_drag: true,
            window_min_size: Some(gpui::Size {
                width: px(480.),
                height: px(360.),
            }),
            kind: WindowKind::Normal,
            app_id: Some("MeterForge-add-meter".to_string()),
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            window.set_window_title("添加表");
            let dialog: Entity<AddMeterView> = cx.new(|cx| {
                AddMeterView::new(existing_addresses, window, cx).on_confirm(
                    move |addresses, source_address, _, cx| {
                        let backend = cx.global::<AppBackend>().clone();
                        let tasks = addresses
                            .into_iter()
                            .map(|address| backend.add_meter(address, source_address.clone(), cx))
                            .collect::<Vec<_>>();
                        let view = view.clone();
                        cx.spawn(async move |_, cx| {
                            for task in tasks {
                                match task.await {
                                    Ok(handle) => {
                                        let _ = view.update(cx, |view, cx| {
                                            view.add_new_meter(handle, cx);
                                            cx.notify();
                                        });
                                    }
                                    Err(error) => {
                                        let _ = view.update(cx, |view, _| {
                                            view.pending_notification =
                                                Some((false, format!("添加表失败：{error}")));
                                        });
                                        break;
                                    }
                                }
                            }
                        })
                        .detach();
                    },
                )
            });
            cx.new(|cx| Root::new(dialog, window, cx))
        })
        .expect("failed to open add meter window");
    }

    /// 后端 spawn 成功后完成 UI 侧收尾：创建 Entity、挂 update loop、注册进
    /// 全局注册表、刷新本地地址列表与订阅、选中新表。与
    /// `remove_deleted_meter` 对称，只是这边是新增。
    fn add_new_meter(&mut self, handle: crate::backend::NewMeterHandle, cx: &mut Context<Self>) {
        let crate::backend::NewMeterHandle {
            address,
            initial_snapshot,
            snapshot_rx,
        } = handle;
        let entity = cx.new(|_| MeterState::with_snapshot(initial_snapshot));
        entity.update(cx, |_, cx| {
            MeterState::start_update_loop(entity.clone(), snapshot_rx, cx)
        });
        cx.global::<GlobalMeterRegistry>()
            .0
            .write()
            .register(address.clone(), entity);
        self.all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
        self.selected_address = Some(address.clone());
        self.detail_view = None;
        self.subscriptions.clear();
        self.subscribe_to_meters(cx);
        self.pending_notification = Some((true, format!("已添加表 {address}")));
    }

    /// 批量清除入口共用的确认弹窗：`kind` 区分冻结历史 / 负荷记录历史，
    /// 确认后调用对应的 `AppBackend::clear_*_history_all`，完成后弹通知。
    /// 与单表清除（`MeterDetailView`）不同，这里操作的是全部电表，不看
    /// 当前选中/过滤状态。
    fn show_clear_all_dialog(
        &mut self,
        kind: ClearAllKind,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let (title, warning, confirm_label) = kind.dialog_text();

        let dialog_entity = cx.new(|_| {
            SyncConfirmDialog::new(warning, confirm_label).on_confirm(move |_, cx| {
                let backend = cx.global::<AppBackend>().clone();
                let task = match kind {
                    ClearAllKind::FreezeHistory => backend.clear_freeze_history_all(cx),
                    ClearAllKind::LoadProfileHistory => backend.clear_load_profile_history_all(cx),
                };
                let view = view.clone();
                cx.spawn(async move |_, cx| {
                    let (success, total) = task.await;
                    let message = format!("{}：{success}/{total} 块表清除成功", kind.label());
                    let _ = view.update(cx, |view, cx| {
                        view.pending_notification = Some((success == total, message));
                        // 批量清除作用于全部表，若当前正显示某块表的详情，它的
                        // 冻结历史/负荷记录缓存也已经过期（缓存合并的是清除前
                        // 查到的旧数据），这里一并刷新，避免面板里还残留旧记录。
                        if let Some(detail) = view.detail_view.clone() {
                            detail.update(cx, |detail, cx| match kind {
                                ClearAllKind::FreezeHistory => {
                                    detail.reset_freeze_history_cache(cx)
                                }
                                ClearAllKind::LoadProfileHistory => {
                                    detail.reset_load_profile_history_cache(cx)
                                }
                            });
                        }
                        cx.notify();
                    });
                })
                .detach();
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(title.to_string()).w(px(500.)).content({
                let dialog_entity = dialog_entity.clone();
                move |content, _, _| content.child(dialog_entity.clone())
            })
        })
    }

    fn show_clear_freeze_history_all_dialog(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_clear_all_dialog(ClearAllKind::FreezeHistory, event, window, cx);
    }

    fn show_clear_load_profile_history_all_dialog(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_clear_all_dialog(ClearAllKind::LoadProfileHistory, event, window, cx);
    }

    /// 删除单表入口的确认弹窗：确认后调用 `AppBackend::remove_meter`（从
    /// 路由/句柄表摘除、优雅关闭 actor，并在排空持久化队列后清除该表的
    /// 全部数据库记录），成功后再清理 UI 侧状态并弹通知。
    fn show_delete_meter_dialog(
        &mut self,
        address: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let warning: SharedString = format!(
            "将停止电表 {address} 的仿真、从列表移除，并清除该表的全部数据\
             （配置、电能、冻结历史、负荷记录），重启后不会恢复。此操作不可撤销。"
        )
        .into();

        let dialog_entity = cx.new(|_| {
            SyncConfirmDialog::new(warning, "删除").on_confirm({
                let view = view.clone();
                let address = address.clone();
                move |_, cx| {
                    let backend = cx.global::<AppBackend>().clone();
                    let task = backend.remove_meter(&address, cx);
                    let view = view.clone();
                    let address = address.clone();
                    cx.spawn(async move |_, cx| {
                        let result = task.await;
                        let _ = view.update(cx, |view, cx| {
                            match result {
                                Ok(()) => view.remove_deleted_meter(&address, cx),
                                Err(error) => {
                                    view.pending_notification =
                                        Some((false, format!("删除电表 {address} 失败：{error}")))
                                }
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            let title: SharedString = format!("删除电表 {address}").into();
            dialog.title(title).w(px(500.)).content({
                let dialog_entity = dialog_entity.clone();
                move |content, _, _| content.child(dialog_entity.clone())
            })
        })
    }

    /// 后端删除成功后清理 UI 侧状态：从全局注册表移除 entity、重建本地列表
    /// 与订阅；若删的是当前选中的表，回退到剩余的第一块（无剩余则显示占位）。
    fn remove_deleted_meter(&mut self, address: &str, cx: &mut Context<Self>) {
        cx.global::<GlobalMeterRegistry>().0.write().remove(address);
        self.all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
        if self.selected_address.as_deref() == Some(address) {
            self.selected_address = self.all_addresses.first().cloned();
            self.detail_view = None;
        }
        self.checked_addresses.remove(address);
        self.subscriptions.clear();
        self.subscribe_to_meters(cx);
        self.pending_notification = Some((true, format!("电表 {address} 已删除")));
    }

    /// 单个表项"修改地址"入口：弹独立窗口填新地址。确认后调用
    /// `AppBackend::update_meter_address`（按序同步 actor 内存地址、路由表、
    /// 数据库与句柄表），成功后在 `apply_address_change` 里完成 UI 侧注册表
    /// re-key 与选中态切换；表的配置与历史数据全部保留，仅地址变化。
    fn show_modify_address_dialog(
        &mut self,
        address: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backend = cx.global::<AppBackend>().clone();
        let existing_addresses = backend.meter_addresses();
        let view = cx.entity().downgrade();
        let current_address = address;

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                // 高度与"添加表"对话框看齐：校验失败时的内联错误框会额外占
                // 一行，窗口太矮会把"确定"按钮挤出可视区，导致没法点确认。
                size(px(520.), px(430.)),
                cx,
            ))),
            titlebar: Some(TitleBar::title_bar_options()),
            app_owns_titlebar_drag: true,
            window_min_size: Some(gpui::Size {
                width: px(480.),
                height: px(360.),
            }),
            kind: WindowKind::Normal,
            app_id: Some("MeterForge-modify-address".to_string()),
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            window.set_window_title("修改表地址");
            let dialog: Entity<ModifyAddressView> = cx.new(|cx| {
                ModifyAddressView::new(current_address, existing_addresses, window, cx).on_confirm(
                    move |old_address, new_address_bytes, _, cx| {
                        let backend = cx.global::<AppBackend>().clone();
                        let task =
                            backend.update_meter_address(&old_address, new_address_bytes, cx);
                        let view = view.clone();
                        let old_address = old_address.clone();
                        cx.spawn(async move |_, cx| match task.await {
                            Ok(new_address) => {
                                let _ = view.update(cx, |view, cx| {
                                    view.apply_address_change(&old_address, &new_address, cx);
                                    cx.notify();
                                });
                            }
                            Err(error) => {
                                let _ = view.update(cx, |view, _| {
                                    view.pending_notification = Some((
                                        false,
                                        format!("修改电表 {old_address} 地址失败：{error}"),
                                    ));
                                });
                            }
                        })
                        .detach();
                    },
                )
            });
            cx.new(|cx| Root::new(dialog, window, cx))
        })
        .expect("failed to open modify address window");
    }

    /// 后端改地址成功后完成 UI 侧收尾：全局注册表 re-key（entity 原样搬到新
    /// 地址下，更新其缓存的地址与快照地址，卡片不用等下一个 tick 才刷新）、
    /// 重建本地地址列表与订阅；若改的是当前选中的表，选中态切到新地址
    /// （右侧详情视图由 `selected_detail` 的地址比对自动重建）。勾选集合里
    /// 的旧地址同步换键。
    fn apply_address_change(
        &mut self,
        old_address: &str,
        new_address: &str,
        cx: &mut Context<Self>,
    ) {
        // 写锁守卫收在独立块里：若延续到 if-let 主体，`cx.global` 的不可变
        // 借用会和 `entity.update(cx, ..)` 的可变借用冲突。
        let renamed_entity = {
            let mut registry = cx.global::<GlobalMeterRegistry>().0.write();
            registry.update_address(old_address, new_address)
        };
        if let Some(entity) = renamed_entity {
            entity.update(cx, |state, _| {
                state.address = new_address.to_owned();
                state.snapshot.address = new_address.to_owned();
            });
        }
        self.all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
        if self.selected_address.as_deref() == Some(old_address) {
            self.selected_address = Some(new_address.to_owned());
            self.detail_view = None;
        }
        if self.checked_addresses.remove(old_address) {
            self.checked_addresses.insert(new_address.to_owned());
        }
        self.subscriptions.clear();
        self.subscribe_to_meters(cx);
        self.pending_notification = Some((
            true,
            format!("电表地址已修改：{old_address} → {new_address}"),
        ));
    }

    /// "删除选中"批量入口的确认弹窗：确认后调用 `AppBackend::remove_meters`
    /// （单个后台任务里串行逐表：摘句柄、关 actor、排空持久化队列、清库，
    /// 单块失败不中断），完成后按成功/失败情况更新 UI 并弹汇总通知。
    fn show_batch_delete_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<String> = self.checked_addresses.iter().cloned().collect();
        if targets.is_empty() {
            return;
        }
        let count = targets.len();
        let view = cx.entity().downgrade();
        let warning: SharedString = format!(
            "将停止这 {count} 块电表的仿真、从列表移除，并清除它们的全部数据\
             （配置、电能、冻结历史、负荷记录），重启后不会恢复。此操作不可撤销。"
        )
        .into();

        let dialog_entity = cx.new(|_| {
            SyncConfirmDialog::new(warning, "删除").on_confirm({
                let view = view.clone();
                move |_, cx| {
                    let backend = cx.global::<AppBackend>().clone();
                    let task = backend.remove_meters(targets.clone(), cx);
                    let view = view.clone();
                    let targets = targets.clone();
                    cx.spawn(async move |_, cx| {
                        let (_, failures) = task.await;
                        let failed: Vec<String> = failures
                            .iter()
                            .map(|(address, _)| address.clone())
                            .collect();
                        let succeeded: Vec<String> = targets
                            .iter()
                            .filter(|address| !failed.contains(address))
                            .cloned()
                            .collect();
                        let _ = view.update(cx, |view, cx| {
                            view.remove_deleted_meters(succeeded, failures, cx);
                            cx.notify();
                        });
                    })
                    .detach();
                }
            })
        });

        window.open_dialog(cx, move |dialog, _, _| {
            let title: SharedString = format!("删除选中的 {count} 块电表").into();
            dialog.title(title).w(px(500.)).content({
                let dialog_entity = dialog_entity.clone();
                move |content, _, _| content.child(dialog_entity.clone())
            })
        })
    }

    /// 批量删除完成后清理 UI 侧状态：把成功的表从全局注册表移除、重建本地
    /// 列表与订阅；若当前选中的表也被删了，回退到剩余的第一块（无剩余则
    /// 显示占位）。失败的表保留在勾选集合里，方便直接重试；通知按成功/
    /// 失败数汇总。
    fn remove_deleted_meters(
        &mut self,
        succeeded: Vec<String>,
        failures: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if !succeeded.is_empty() {
            {
                let mut registry = cx.global::<GlobalMeterRegistry>().0.write();
                for address in &succeeded {
                    registry.remove(address);
                }
            }
            self.all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
            if self
                .selected_address
                .as_ref()
                .is_some_and(|selected| succeeded.contains(selected))
            {
                self.selected_address = self.all_addresses.first().cloned();
                self.detail_view = None;
            }
            self.subscriptions.clear();
            self.subscribe_to_meters(cx);
        }

        // 勾选集合只保留删除失败的表，方便直接重试
        self.checked_addresses
            .retain(|address| failures.iter().any(|(failed, _)| failed == address));

        let message = if failures.is_empty() {
            format!("已删除 {} 块电表", succeeded.len())
        } else {
            let detail = failures
                .iter()
                .map(|(address, error)| format!("{address}（{error}）"))
                .collect::<Vec<_>>()
                .join("、");
            format!(
                "成功删除 {} 块，失败 {} 块：{detail}",
                succeeded.len(),
                failures.len()
            )
        };
        self.pending_notification = Some((failures.is_empty(), message));
    }

    /// 每次渲染前刷新过滤结果与 ListState 条目数（数量没变则不动，避免每帧
    /// 重建列表丢滚动位置）；搜索过滤导致数量变化时 reset 会回到顶部。
    /// 顺带把勾选集合收敛到仍然存在的表：删除/改名后残留的地址清掉，避免
    /// "删除选中"计数虚高或批量删除命中不存在的表。
    fn sync_meter_list(&mut self, cx: &App) {
        let filtered = self.filtered_addresses(cx);
        if self.list_state.item_count() != filtered.len() {
            self.list_state.reset(filtered.len());
        }
        self.filtered_items = filtered;
        if !self.checked_addresses.is_empty() {
            let live: HashSet<&String> = self.all_addresses.iter().collect();
            self.checked_addresses
                .retain(|address| live.contains(address));
        }
    }

    /// 单个表项：从注册表取最新快照渲染 MeterCard，只渲染可见范围内的行。
    /// 行首是批量删除用的勾选框，卡片右侧附"修改地址"与删除按钮，两者都先
    /// 拦截冒泡，避免同时触发行选中。
    fn render_meter_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(address) = self.filtered_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let selected = self.selected_address.as_ref() == Some(&address);
        let checked = self.checked_addresses.contains(&address);
        let snapshot = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            registry
                .get(&address)
                .map(|entity| entity.read(cx).snapshot.clone())
        };
        let Some(snapshot) = snapshot else {
            return div().into_any_element();
        };
        let checkbox_address = address.clone();
        let checkbox = div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Checkbox::new(("meter-check", ix))
                    .checked(checked)
                    .on_click(cx.listener(move |view, checked: &bool, _, cx| {
                        if *checked {
                            view.checked_addresses.insert(checkbox_address.clone());
                        } else {
                            view.checked_addresses.remove(&checkbox_address);
                        }
                        cx.notify();
                    })),
            );
        let modify_button = div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(("meter-modify-address", ix))
                    .icon(IconName::Replace)
                    .tooltip("修改地址")
                    .ghost()
                    .small()
                    .on_click({
                        let address = address.clone();
                        cx.listener(move |view, _, window, cx| {
                            view.show_modify_address_dialog(address.clone(), window, cx);
                        })
                    }),
            );
        let delete_button = div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(("meter-delete", ix))
                    .icon(IconName::CircleX)
                    .tooltip("删除电表")
                    .ghost()
                    .small()
                    .on_click({
                        let address = address.clone();
                        cx.listener(move |view, _, window, cx| {
                            view.show_delete_meter_dialog(address.clone(), window, cx);
                        })
                    }),
            );
        div()
            .id(("meter-list-item", ix))
            .w_full()
            .pb_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    view.select_meter(address.clone(), window, cx)
                }),
            )
            .child(
                MeterCard::new(snapshot)
                    .selected(selected)
                    .leading(checkbox)
                    .trailing(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(modify_button)
                            .child(delete_button),
                    ),
            )
            .into_any_element()
    }

    fn selected_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MeterDetailView>> {
        let address = self.selected_address.clone()?;
        let needs_new = self
            .detail_view
            .as_ref()
            .map(|view| view.read(cx).address() != address)
            .unwrap_or(true);
        if needs_new {
            self.detail_view = Some(cx.new(|cx| MeterDetailView::new(address, window, cx)));
        }
        self.detail_view.clone()
    }
}

impl Render for MeterListView {
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
        self.sync_meter_list(cx);
        let count = self.filtered_items.len();
        let checked_count = self.checked_addresses.len();
        let all_filtered_checked = count > 0
            && self
                .filtered_items
                .iter()
                .all(|address| self.checked_addresses.contains(address));
        let detail = self.selected_detail(window, cx);

        div().size_full().flex().flex_col().child(
            div().flex_1().min_h_0().child(
                h_resizable("meter-workbench")
                    .child(
                        resizable_panel()
                            .size(px(304.0))
                            .size_range(px(240.0)..px(460.0))
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .border_r_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .px_3()
                                            .py_2()
                                            .border_b_1()
                                            .border_color(theme.border)
                                            .child(Input::new(&self.address_search).small())
                                            .child(
                                                v_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("add-meter")
                                                            .label("添加表")
                                                            .small()
                                                            .primary()
                                                            .w_full()
                                                            .on_click(cx.listener(
                                                                Self::show_add_meter_dialog,
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("delete-checked-meters")
                                                            .label(format!(
                                                                "删除选中({checked_count})"
                                                            ))
                                                            .small()
                                                            .danger()
                                                            .w_full()
                                                            .disabled(checked_count == 0)
                                                            .on_click(cx.listener(
                                                                Self::show_batch_delete_dialog,
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("clear-freeze-history-all")
                                                            .label("清除历史数据")
                                                            .small()
                                                            .danger()
                                                            .w_full()
                                                            .on_click(cx.listener(
                                                                Self::show_clear_freeze_history_all_dialog,
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("clear-load-profile-history-all")
                                                            .label("清除负荷记录数据")
                                                            .small()
                                                            .danger()
                                                            .w_full()
                                                            .on_click(cx.listener(
                                                                Self::show_clear_load_profile_history_all_dialog,
                                                            )),
                                                    ),
                                            ),
                                    )
                                    // 列表头：全选（作用于当前搜索过滤结果）+ 已选计数，
                                    // 作为"删除选中"批量入口的勾选开关。
                                    .when(count > 0, |panel| {
                                        panel.child(
                                            h_flex()
                                                .items_center()
                                                .gap_2()
                                                .px_3()
                                                .py_1()
                                                .border_b_1()
                                                .border_color(theme.border)
                                                .child(
                                                    Checkbox::new("meter-check-all")
                                                        .label("全选")
                                                        .checked(all_filtered_checked)
                                                        .on_click(cx.listener(
                                                            move |view,
                                                                  checked: &bool,
                                                                  _,
                                                                  cx| {
                                                                let filtered = view
                                                                    .filtered_items
                                                                    .clone();
                                                                if *checked {
                                                                    view.checked_addresses
                                                                        .extend(filtered);
                                                                } else {
                                                                    for address in &filtered {
                                                                        view.checked_addresses
                                                                            .remove(address);
                                                                    }
                                                                }
                                                                cx.notify();
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Label::new(format!("已选 {checked_count} 块"))
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground),
                                                ),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("meter-list")
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .px_2()
                                            .py_3()
                                            .child(if count == 0 {
                                                v_flex()
                                                    .items_center()
                                                    .py_8()
                                                    .child(
                                                        Label::new("未找到匹配的电表地址")
                                                            .text_sm()
                                                            .text_color(theme.muted_foreground),
                                                    )
                                                    .into_any_element()
                                            } else {
                                                list(
                                                    self.list_state.clone(),
                                                    cx.processor(Self::render_meter_item),
                                                )
                                                .with_sizing_behavior(ListSizingBehavior::Auto)
                                                .size_full()
                                                .into_any_element()
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        resizable_panel().size_range(px(520.0)..Pixels::MAX).child(
                            div().size_full().min_w_0().child(
                                detail
                                    .map(|view| view.into_any_element())
                                    .unwrap_or_else(|| {
                                        div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                Label::new("选择一块电表以查看详情")
                                                    .text_color(theme.muted_foreground),
                                            )
                                            .into_any_element()
                                    }),
                            ),
                        ),
                    ),
            ),
        )
    }
}
