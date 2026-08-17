// 顶部状态栏：保持与监控工作台一致的紧凑信息密度。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::label::Label;
use gpui_component::*;
use meter_core::ConnectionStatus;
use std::rc::Rc;

#[derive(IntoElement)]
pub struct AppTitleBar {
    total_meter_count: usize,
    online_meter_count: usize,
    connection_status: ConnectionStatus,
    on_settings: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

#[derive(IntoElement)]
pub struct SettingsTitleBar {
    title: SharedString,
}

struct SettingsTitleBarState {
    should_move: bool,
}

#[derive(IntoElement)]
struct SettingsCloseControl;

impl AppTitleBar {
    pub fn new(
        total_meter_count: usize,
        online_meter_count: usize,
        connection_status: ConnectionStatus,
    ) -> Self {
        Self {
            total_meter_count,
            online_meter_count,
            connection_status,
            on_settings: None,
        }
    }

    pub fn on_settings(
        mut self,
        callback: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_settings = Some(Rc::new(callback));
        self
    }
}

impl SettingsTitleBar {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

impl Render for SettingsTitleBarState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RenderOnce for SettingsCloseControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover_fg = cx.theme().danger_foreground;
        let hover_bg = cx.theme().danger;
        let active_bg = cx.theme().danger_active;

        div()
            .id("settings-close")
            .flex()
            .w(px(34.))
            .h_full()
            .flex_shrink_0()
            .justify_center()
            .content_center()
            .items_center()
            .text_color(cx.theme().foreground)
            .hover(|style| style.bg(hover_bg).text_color(hover_fg))
            .active(|style| style.bg(active_bg).text_color(hover_fg))
            .when(cfg!(target_os = "windows"), |this| {
                this.window_control_area(WindowControlArea::Close)
            })
            .when(cfg!(target_os = "linux"), |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    window.remove_window();
                })
            })
            .child(Icon::new(IconName::WindowClose).small())
    }
}

impl AppTitleBar {
    fn render_status_chip(
        label: &'static str,
        value: impl Into<SharedString>,
        active: bool,
        theme: &Theme,
    ) -> Div {
        let dot_color = if active {
            theme.success
        } else {
            theme.muted_foreground.opacity(0.65)
        };
        let value_color = if active {
            theme.foreground
        } else {
            theme.muted_foreground
        };

        h_flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .child(div().size_2().rounded_full().bg(dot_color))
            .child(
                Label::new(label)
                    .text_xs()
                    .text_color(theme.muted_foreground),
            )
            .child(
                Label::new(value)
                    .text_xs()
                    .font_semibold()
                    .max_w(px(180.))
                    .truncate()
                    .text_color(value_color),
            )
    }
}

impl RenderOnce for AppTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (tcp_server_value, tcp_server_active) =
            if let Some(address) = self.connection_status.tcp_server_addr {
                (SharedString::from(address), true)
            } else {
                (SharedString::from("未启动"), false)
            };
        let (tcp_client_value, tcp_client_active) =
            if let Some(address) = self.connection_status.tcp_client_addr {
                (SharedString::from(address), true)
            } else {
                (SharedString::from("未连接"), false)
            };
        let (serial_value, serial_active) = if let Some(path) = self.connection_status.serial_path {
            (SharedString::from(path), true)
        } else {
            (SharedString::from("未连接"), false)
        };

        TitleBar::new()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .child(Icon::new(IconName::Sun).size_4().text_color(theme.primary))
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .child(
                                Label::new("DL/T 645-2007 虚拟电表监控平台")
                                    .font_semibold()
                                    .truncate(),
                            )
                            .child(
                                h_flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme.secondary)
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(div().size_2().rounded_full().bg(theme.success))
                                    .child(
                                        Label::new("在线")
                                            .text_xs()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        Label::new(format!(
                                            "{}/{}",
                                            self.online_meter_count, self.total_meter_count
                                        ))
                                        .text_sm()
                                        .font_semibold(),
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    // TitleBar marks its content as a window drag area. Interactive controls
                    // must stop the press here, matching gpui-component's title-bar example.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Self::render_status_chip(
                            "TCP服务端",
                            tcp_server_value,
                            tcp_server_active,
                            theme,
                        )
                        .max_w(px(260.)),
                    )
                    .child(
                        Self::render_status_chip(
                            "TCP客户端",
                            tcp_client_value,
                            tcp_client_active,
                            theme,
                        )
                        .max_w(px(260.)),
                    )
                    .child(
                        Self::render_status_chip("串口", serial_value, serial_active, theme)
                            .max_w(px(220.)),
                    )
                    .when_some(self.on_settings, |this, on_settings| {
                        this.child(
                            Button::new("open-connection-settings")
                                .small()
                                .ghost()
                                .icon(IconName::Settings2)
                                .tooltip("连接设置")
                                .on_click(move |event, window, cx| on_settings(event, window, cx)),
                        )
                    }),
            )
    }
}

impl RenderOnce for SettingsTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_state(cx, |_, _| SettingsTitleBarState { should_move: false });

        div().flex_shrink_0().child(
            div()
                .id("settings-title-bar")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .h(px(34.))
                .border_b_1()
                .border_color(cx.theme().title_bar_border)
                .bg(cx.theme().tokens.title_bar)
                .child(
                    h_flex()
                        .id("settings-title-bar-drag")
                        .h_full()
                        .flex_1()
                        .items_center()
                        .px_3()
                        .when(!cfg!(target_family = "wasm"), |this| {
                            this.window_control_area(WindowControlArea::Drag)
                        })
                        .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
                            state.should_move = false;
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            window.listener_for(&state, |state, _, _, _| {
                                state.should_move = true;
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            window.listener_for(&state, |state, _, _, _| {
                                state.should_move = false;
                            }),
                        )
                        .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
                            if state.should_move {
                                state.should_move = false;
                                window.start_window_move();
                            }
                        }))
                        .child(Label::new(self.title).font_semibold()),
                )
                .child(SettingsCloseControl),
        )
    }
}
