//! Meter UI application entry point.
//!
//! Bootstrap is intentionally small: it wires the presentation store to the
//! application services, then opens a GPUI `Root`.

mod assets;
mod backend;
mod components;
mod pages;
mod settings;
mod state;
mod types;

use assets::Assets;

use gpui::*;
use gpui_component::*;
use pages::ApplicationWorkspace;
use parking_lot::RwLock;
use state::{ConnectionStatusStore, GlobalConnectionStatus, GlobalMeterRegistry, MeterRegistry};
use std::sync::Arc;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);

        let meter_store = Arc::new(RwLock::new(MeterRegistry::new()));
        cx.set_global(GlobalMeterRegistry(meter_store.clone()));
        let connection_status = cx.new(|_| ConnectionStatusStore::default());
        cx.set_global(GlobalConnectionStatus(connection_status));
        backend::initialize(meter_store, cx);

        let mut window_size = size(px(1600.), px(1200.));
        if let Some(display) = cx.primary_display() {
            window_size.width = window_size.width.min(display.bounds().size.width * 0.85);
            window_size.height = window_size.height.min(display.bounds().size.height * 0.85);
        }
        let window_bounds = Bounds::centered(None, window_size, cx);

        cx.spawn(async move |cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                titlebar: Some(TitleBar::title_bar_options()),
                app_owns_titlebar_drag: true,
                window_min_size: Some(gpui::Size {
                    width: px(640.),
                    height: px(480.),
                }),
                kind: WindowKind::Normal,
                app_id: Some("meter-engine".to_string()),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| ApplicationWorkspace::new(window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("failed to open meter window");
        })
        .detach();

        // 注册退出回调，确保优雅关闭所有电表
        // 因为不能在闭包中借用 cx，我们先获取 backend 的克隆
        let backend_for_quit = cx.global::<backend::AppBackend>().clone();
        cx.on_app_quit(move |_| {
            let backend = backend_for_quit.clone();
            async move {
                tracing::info!("应用退出，正在优雅关闭所有电表...");
                backend.shutdown_all_meters();
                tracing::info!("所有电表已关闭");
            }
        })
        .detach();
    });

    tracing::info!("应用已退出");
}
