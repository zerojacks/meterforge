//! 应用根工作区：只负责编排页面区域与全局浮层，不承载具体业务细节。
use super::MeterListView;
use crate::components::AppTitleBar;
use crate::settings::ConnectionConfigView;
use crate::state::{GlobalConnectionStatus, GlobalMeterRegistry, MeterState};
use gpui::*;
use gpui_component::*;

pub struct ApplicationWorkspace {
    meter_workspace: Entity<MeterListView>,
    subscriptions: Vec<Subscription>,
}

impl ApplicationWorkspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut workspace = Self {
            meter_workspace: cx.new(|cx| MeterListView::new(window, cx)),
            subscriptions: Vec::new(),
        };
        workspace.subscribe_titlebar_sources(cx);
        workspace
    }

    fn subscribe_titlebar_sources(&mut self, cx: &mut Context<Self>) {
        let meter_entities: Vec<Entity<MeterState>> = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            registry
                .all_addresses()
                .into_iter()
                .filter_map(|address| registry.get(&address).cloned())
                .collect()
        };
        for entity in meter_entities {
            self.subscriptions
                .push(cx.observe(&entity, |_this, _entity, cx| cx.notify()));
        }
        let connection_status = cx.global::<GlobalConnectionStatus>().0.clone();
        self.subscriptions
            .push(cx.observe(&connection_status, |_this, _entity, cx| cx.notify()));
    }

    fn open_connection_settings(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(980.), px(680.)),
                cx,
            ))),
            titlebar: Some(TitleBar::title_bar_options()),
            app_owns_titlebar_drag: true,
            window_min_size: Some(gpui::Size {
                width: px(720.),
                height: px(500.),
            }),
            kind: WindowKind::Normal,
            app_id: Some("meter-engine-settings".to_string()),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| ConnectionConfigView::new(window, cx));
            view.update(cx, |view, cx| view.refresh_serial_ports(cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open connection settings window");
    }
}

impl Render for ApplicationWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (meter_count, online_count) = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            let total = registry.count();
            let online = registry
                .all_addresses()
                .into_iter()
                .filter(|address| {
                    registry
                        .get(address)
                        .map(|entity| entity.read(cx).snapshot.is_online)
                        .unwrap_or(false)
                })
                .count();
            (total, online)
        };
        let connection_status = cx.global::<GlobalConnectionStatus>().0.read(cx).snapshot();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                AppTitleBar::new(meter_count, online_count, connection_status).on_settings({
                    let workspace = cx.entity();
                    move |event, window, cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.open_connection_settings(event, window, cx)
                        });
                    }
                }),
            )
            .child(self.meter_workspace.clone())
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
