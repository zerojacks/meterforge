// 独立连接设置窗口：由 Settings 组件提供串口与 TCP 页面。

use crate::backend::AppBackend;
use crate::components::SettingsTitleBar;
use crate::state::GlobalConnectionStatus;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::*;
use meter_core::{
    ConnectionCommand, ConnectionResult, ConnectionStatus, SerialDataBits, SerialParity,
    SerialSettings, SerialStopBits,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionKind {
    Serial,
    TcpClient,
    TcpServer,
}

/// 设置页导航:连接相关页面 + 通用设置(外观、语言)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Connection(ConnectionKind),
    Appearance,
    Language,
}

pub struct ConnectionConfigView {
    search_input: Entity<InputState>,
    serial_select: Entity<SelectState<Vec<String>>>,
    baud_select: Entity<SelectState<Vec<String>>>,
    data_bits_select: Entity<SelectState<Vec<String>>>,
    parity_select: Entity<SelectState<Vec<String>>>,
    stop_bits_select: Entity<SelectState<Vec<String>>>,
    tcp_server_ip: Entity<InputState>,
    tcp_server_port: Entity<InputState>,
    tcp_client_ip: Entity<InputState>,
    tcp_client_port: Entity<InputState>,
    active_page: SettingsPage,
    theme_select: Entity<SelectState<Vec<String>>>,
    language_select: Entity<SelectState<Vec<String>>>,
    serial_ports: Vec<String>,
    selected_serial: usize,
    serial_refreshing: bool,
    pending_serial_result: Option<ConnectionResult>,
    connecting_kind: Option<ConnectionKind>,
    pending_notification: Option<ConnectionResult>,
    serial_connected: bool,
    tcp_client_connected: bool,
    tcp_server_running: bool,
}

impl ConnectionConfigView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let status = cx.global::<AppBackend>().connections.status();
        let serial_ports = Vec::new();
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search..."));
        let serial_select = cx.new(|cx| {
            SelectState::new(
                serial_ports.clone(),
                (!serial_ports.is_empty()).then_some(IndexPath::default()),
                window,
                cx,
            )
        });
        let mut make_select = |items: Vec<String>, selected: Option<IndexPath>, cx: &mut App| {
            cx.new(|cx| SelectState::new(items, selected, window, cx))
        };
        let default_index = Some(IndexPath::default());
        let baud_select = make_select(
            vec![
                "1200", "2400", "4800", "9600", "19200", "38400", "57600", "115200",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            default_index,
            cx,
        );
        let data_bits_select = make_select(
            vec!["8", "7", "6", "5"]
                .into_iter()
                .map(String::from)
                .collect(),
            default_index,
            cx,
        );
        let parity_select = make_select(
            vec!["偶校验", "无校验", "奇校验"]
                .into_iter()
                .map(String::from)
                .collect(),
            default_index,
            cx,
        );
        let stop_bits_select = make_select(
            vec!["1", "2"].into_iter().map(String::from).collect(),
            default_index,
            cx,
        );
        let dark_mode = cx.theme().mode.is_dark();
        let theme_select = make_select(
            vec!["浅色", "深色"].into_iter().map(String::from).collect(),
            Some(IndexPath::new(if dark_mode { 1 } else { 0 })),
            cx,
        );
        let language_select = make_select(
            vec!["简体中文", "English"]
                .into_iter()
                .map(String::from)
                .collect(),
            default_index,
            cx,
        );
        let make_input = |value: &str, placeholder: &str, cx: &mut App, window: &mut Window| {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            state.update(cx, |state, cx| state.set_value(value, window, cx));
            state
        };
        // 将 "ip:port" 拆为两段回填到独立输入框。
        let split_addr = |addr: &str| -> (String, String) {
            match addr.rsplit_once(':') {
                Some((ip, port)) => (ip.to_string(), port.to_string()),
                None => (addr.to_string(), "8645".to_string()),
            }
        };
        let (server_ip, server_port) =
            split_addr(status.tcp_server_addr.as_deref().unwrap_or("0.0.0.0:8645"));
        let tcp_server_ip = make_input(&server_ip, "0.0.0.0", cx, window);
        let tcp_server_port = make_input(&server_port, "8645", cx, window);
        let (client_ip, client_port) = split_addr(
            status
                .tcp_client_addr
                .as_deref()
                .unwrap_or("127.0.0.1:8645"),
        );
        let tcp_client_ip = make_input(&client_ip, "127.0.0.1", cx, window);
        let tcp_client_port = make_input(&client_port, "8645", cx, window);
        let view = Self {
            search_input,
            serial_select,
            baud_select,
            data_bits_select,
            parity_select,
            stop_bits_select,
            tcp_server_ip,
            tcp_server_port,
            tcp_client_ip,
            tcp_client_port,
            active_page: SettingsPage::Connection(ConnectionKind::Serial),
            theme_select,
            language_select,
            serial_ports,
            selected_serial: 0,
            serial_refreshing: false,
            pending_serial_result: None,
            connecting_kind: None,
            pending_notification: None,
            serial_connected: status.serial_path.is_some(),
            tcp_client_connected: status.tcp_client_addr.is_some(),
            tcp_server_running: status.tcp_server_addr.is_some(),
        };
        // 切换主题选项后立即应用到全局主题。
        cx.subscribe(
            &view.theme_select,
            |_, _, event: &SelectEvent<Vec<String>>, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let mode = if value == "深色" {
                        ThemeMode::Dark
                    } else {
                        ThemeMode::Light
                    };
                    Theme::change(mode, None, cx);
                    cx.notify();
                }
            },
        )
        .detach();
        view
    }

    fn sync_connection_status(&mut self, status: ConnectionStatus, cx: &mut Context<Self>) {
        self.serial_connected = status.serial_path.is_some();
        self.tcp_client_connected = status.tcp_client_addr.is_some();
        self.tcp_server_running = status.tcp_server_addr.is_some();

        let connection_store = cx.global::<GlobalConnectionStatus>().0.clone();
        connection_store.update(cx, |store, cx| {
            store.sync(status);
            cx.notify();
        });
    }

    fn apply_serial_result(
        &mut self,
        result: ConnectionResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.serial_ports = result.serial_ports;
        self.selected_serial = 0;
        self.serial_select.update(cx, |state, cx| {
            state.set_items(self.serial_ports.clone(), window, cx);
            state.set_selected_index(
                (!self.serial_ports.is_empty()).then_some(IndexPath::default()),
                window,
                cx,
            );
        });
        let notification = if result.success {
            Notification::success(result.message)
        } else {
            Notification::error(result.message)
        };
        window.push_notification(notification, cx);
        cx.notify();
    }
    pub fn refresh_serial_ports(&mut self, cx: &mut Context<Self>) {
        if self.serial_refreshing {
            return;
        }
        self.serial_refreshing = true;
        let result_rx = cx
            .global::<AppBackend>()
            .connections
            .execute_async(ConnectionCommand::ListSerialPorts);
        cx.spawn(async move |this, cx| {
            let result = result_rx.await.unwrap_or_else(|_| ConnectionResult {
                success: false,
                message: "连接服务已停止，未能读取本机串口".into(),
                serial_ports: Vec::new(),
            });
            let _ = this.update(cx, |view, cx| {
                view.serial_refreshing = false;
                view.pending_serial_result = Some(result);
                cx.notify();
            });
        })
        .detach();
    }
    fn set_page(&mut self, page: SettingsPage, _: &mut Window, cx: &mut Context<Self>) {
        self.active_page = page;
        cx.notify();
    }

    fn nav_label(page: SettingsPage) -> &'static str {
        match page {
            SettingsPage::Connection(ConnectionKind::Serial) => "串口连接",
            SettingsPage::Connection(ConnectionKind::TcpClient) => "TCP 客户端",
            SettingsPage::Connection(ConnectionKind::TcpServer) => "TCP 服务端",
            SettingsPage::Appearance => "外观",
            SettingsPage::Language => "语言",
        }
    }

    fn nav_keywords(page: SettingsPage) -> &'static [&'static str] {
        match page {
            SettingsPage::Connection(ConnectionKind::Serial) => {
                &["串口", "serial", "com", "波特率", "校验"]
            }
            SettingsPage::Connection(ConnectionKind::TcpClient) => {
                &["tcp", "客户端", "client", "远程连接", "server"]
            }
            SettingsPage::Connection(ConnectionKind::TcpServer) => {
                &["tcp", "服务端", "server", "监听", "端口"]
            }
            SettingsPage::Appearance => &["外观", "主题", "theme", "深色", "浅色", "dark", "light"],
            SettingsPage::Language => &["语言", "language", "中文", "english"],
        }
    }

    fn nav_matches(page: SettingsPage, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }

        Self::nav_label(page).to_lowercase().contains(&query)
            || Self::nav_keywords(page)
                .iter()
                .any(|keyword| keyword.to_lowercase().contains(&query))
    }

    fn visible_pages(query: &str) -> Vec<SettingsPage> {
        [
            SettingsPage::Connection(ConnectionKind::Serial),
            SettingsPage::Connection(ConnectionKind::TcpClient),
            SettingsPage::Connection(ConnectionKind::TcpServer),
            SettingsPage::Appearance,
            SettingsPage::Language,
        ]
        .into_iter()
        .filter(|page| Self::nav_matches(*page, query))
        .collect()
    }

    fn start_connection(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let SettingsPage::Connection(kind) = self.active_page else {
            return;
        };
        if self.connecting_kind == Some(kind) {
            return;
        }
        let serial = self
            .serial_select
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_default();
        let baud_rate = self
            .baud_select
            .read(cx)
            .selected_value()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2400);
        let data_bits = match self
            .data_bits_select
            .read(cx)
            .selected_value()
            .map(String::as_str)
        {
            Some("5") => SerialDataBits::Five,
            Some("6") => SerialDataBits::Six,
            Some("7") => SerialDataBits::Seven,
            _ => SerialDataBits::Eight,
        };
        let parity = match self
            .parity_select
            .read(cx)
            .selected_value()
            .map(String::as_str)
        {
            Some("无校验") => SerialParity::None,
            Some("奇校验") => SerialParity::Odd,
            _ => SerialParity::Even,
        };
        let stop_bits = match self
            .stop_bits_select
            .read(cx)
            .selected_value()
            .map(String::as_str)
        {
            Some("2") => SerialStopBits::Two,
            _ => SerialStopBits::One,
        };
        let command = match kind {
            ConnectionKind::Serial if self.serial_connected => ConnectionCommand::DisconnectSerial,
            ConnectionKind::Serial => ConnectionCommand::ConnectSerial {
                path: serial,
                settings: SerialSettings {
                    baud_rate,
                    data_bits,
                    parity,
                    stop_bits,
                },
            },
            ConnectionKind::TcpServer if self.tcp_server_running => {
                ConnectionCommand::StopTcpServer
            }
            ConnectionKind::TcpServer => ConnectionCommand::StartTcpServer {
                address: format!(
                    "{}:{}",
                    self.tcp_server_ip.read(cx).value(),
                    self.tcp_server_port.read(cx).value()
                ),
            },
            ConnectionKind::TcpClient if self.tcp_client_connected => {
                ConnectionCommand::DisconnectTcpClient
            }
            ConnectionKind::TcpClient => ConnectionCommand::ConnectTcpClient {
                address: format!(
                    "{}:{}",
                    self.tcp_client_ip.read(cx).value(),
                    self.tcp_client_port.read(cx).value()
                ),
            },
        };
        let manager = cx.global::<AppBackend>().connections.clone();
        let action_kind = kind;
        self.connecting_kind = Some(action_kind);
        cx.notify();
        let result_rx = manager.execute_async(command);
        cx.spawn(async move |this, cx| {
            let result = result_rx.await.unwrap_or_else(|_| ConnectionResult {
                success: false,
                message: "连接服务已停止，未能返回操作结果".into(),
                serial_ports: Vec::new(),
            });
            let status = manager.status();
            let _ = this.update(cx, |view, cx| {
                if result.success {
                    view.sync_connection_status(status, cx);
                }
                view.connecting_kind = None;
                view.pending_notification = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_section(
        &self,
        title: &'static str,
        description: &'static str,
        content: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> Div {
        v_flex()
            .gap_4()
            .p_6()
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(title).text_xl().font_semibold())
                    .child(
                        Label::new(description)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                div()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .p_5()
                    .child(content),
            )
    }

    /// 统一的表单行标签：等宽容器 + 右对齐，保证冒号与输入框起点对齐。
    fn form_label(text: &'static str) -> Label {
        Label::new(text).w(px(100.)).text_right().font_semibold()
    }

    fn render_serial_panel(
        &self,
        view: Entity<Self>,
        serial_select: Entity<SelectState<Vec<String>>>,
        baud_select: Entity<SelectState<Vec<String>>>,
        data_bits_select: Entity<SelectState<Vec<String>>>,
        parity_select: Entity<SelectState<Vec<String>>>,
        stop_bits_select: Entity<SelectState<Vec<String>>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let connecting = self.connecting_kind == Some(ConnectionKind::Serial);
        let connected = self.serial_connected;
        let button_label = if connected {
            "断开串口"
        } else {
            "连接串口"
        };

        self.render_section(
            "串口连接",
            "配置串口号、波特率、数据位、校验位与停止位。",
            v_flex()
                .gap_3()
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Self::form_label("串口："))
                                .child(
                                    Select::new(&serial_select)
                                        .flex_1()
                                        .placeholder("未找到本机串口"),
                                )
                                .child(
                                    Button::new("refresh-serial-port")
                                        .ghost()
                                        .icon(
                                            Icon::new(IconName::Redo2).path("icons/refresh-cw.svg"),
                                        )
                                        .xsmall()
                                        .w(px(28.))
                                        .loading(self.serial_refreshing)
                                        .disabled(self.serial_refreshing)
                                        .tooltip("刷新串口")
                                        .on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                view.update(cx, |view, cx| {
                                                    view.refresh_serial_ports(cx)
                                                });
                                            }
                                        }),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Self::form_label("波特率："))
                                .child(Select::new(&baud_select).flex_1().placeholder("波特率"))
                                .child(div().w(px(28.))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Self::form_label("数据位："))
                                .child(
                                    Select::new(&data_bits_select)
                                        .flex_1()
                                        .placeholder("数据位"),
                                )
                                .child(div().w(px(28.))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Self::form_label("校验："))
                                .child(Select::new(&parity_select).flex_1().placeholder("校验"))
                                .child(div().w(px(28.))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Self::form_label("停止位："))
                                .child(
                                    Select::new(&stop_bits_select)
                                        .flex_1()
                                        .placeholder("停止位"),
                                )
                                .child(div().w(px(28.))),
                        ),
                )
                .child(if connecting {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(Label::new("连接中…"))
                        .into_any_element()
                } else {
                    Button::new("serial-connect")
                        .label(button_label)
                        .when(connected, |button| button.danger())
                        .when(!connected, |button| button.success())
                        .on_click({
                            let view = view.clone();
                            move |event, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.set_page(
                                        SettingsPage::Connection(ConnectionKind::Serial),
                                        window,
                                        cx,
                                    );
                                    view.start_connection(event, window, cx);
                                });
                            }
                        })
                        .into_any_element()
                }),
            cx,
        )
        .into_any_element()
    }

    fn render_appearance_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme_select = self.theme_select.clone();
        self.render_section(
            "外观",
            "切换应用的浅色或深色主题。",
            v_flex().gap_3().child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Self::form_label("主题："))
                    .child(Select::new(&theme_select).w(px(220.)).placeholder("主题"))
                    .child(div().flex_1()),
            ),
            cx,
        )
        .into_any_element()
    }

    fn render_language_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let language_select = self.language_select.clone();
        self.render_section(
            "语言",
            "选择界面显示语言。",
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::form_label("界面语言："))
                        .child(
                            Select::new(&language_select)
                                .w(px(220.))
                                .placeholder("界面语言"),
                        )
                        .child(div().flex_1()),
                )
                .child(
                    Label::new("当前版本界面文案以中文为主，其他语言将在后续版本提供完整支持。")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                ),
            cx,
        )
        .into_any_element()
    }

    fn render_tcp_client_panel(
        &self,
        view: Entity<Self>,
        tcp_client_ip: Entity<InputState>,
        tcp_client_port: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let connecting = self.connecting_kind == Some(ConnectionKind::TcpClient);
        let connected = self.tcp_client_connected;
        let button_label = if connected {
            "断开客户端"
        } else {
            "连接服务器"
        };

        self.render_section(
            "TCP 客户端",
            "输入目标地址后主动连接到远程 TCP 服务端。",
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::form_label("服务器 IP："))
                        .child(Input::new(&tcp_client_ip).w(px(220.)).h(px(34.)))
                        .child(div().flex_1()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::form_label("端口："))
                        .child(Input::new(&tcp_client_port).w(px(120.)).h(px(34.)))
                        .child(div().flex_1()),
                )
                .child(if connecting {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(Label::new("连接中…"))
                        .into_any_element()
                } else {
                    Button::new("tcp-client-connect")
                        .label(button_label)
                        .when(connected, |button| button.danger())
                        .when(!connected, |button| button.success())
                        .on_click({
                            let view = view.clone();
                            move |event, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.set_page(
                                        SettingsPage::Connection(ConnectionKind::TcpClient),
                                        window,
                                        cx,
                                    );
                                    view.start_connection(event, window, cx);
                                });
                            }
                        })
                        .into_any_element()
                }),
            cx,
        )
        .into_any_element()
    }

    fn render_tcp_server_panel(
        &self,
        view: Entity<Self>,
        tcp_server_ip: Entity<InputState>,
        tcp_server_port: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let connecting = self.connecting_kind == Some(ConnectionKind::TcpServer);
        let running = self.tcp_server_running;
        let button_label = if running {
            "停止服务"
        } else {
            "启动服务"
        };

        self.render_section(
            "TCP 服务端",
            "配置本地监听地址，供外部 TCP 客户端接入。",
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::form_label("监听 IP："))
                        .child(Input::new(&tcp_server_ip).w(px(220.)).h(px(34.)))
                        .child(div().flex_1()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::form_label("端口："))
                        .child(Input::new(&tcp_server_port).w(px(120.)).h(px(34.)))
                        .child(div().flex_1()),
                )
                .child(if connecting {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child(Label::new("启动中…"))
                        .into_any_element()
                } else {
                    Button::new("tcp-server-connect")
                        .label(button_label)
                        .when(running, |button| button.danger())
                        .when(!running, |button| button.success())
                        .on_click({
                            let view = view.clone();
                            move |event, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.set_page(
                                        SettingsPage::Connection(ConnectionKind::TcpServer),
                                        window,
                                        cx,
                                    );
                                    view.start_connection(event, window, cx);
                                });
                            }
                        })
                        .into_any_element()
                }),
            cx,
        )
        .into_any_element()
    }
}

impl Render for ConnectionConfigView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(result) = self.pending_serial_result.take() {
            self.apply_serial_result(result, window, cx);
        }
        if let Some(result) = self.pending_notification.take() {
            let notification = if result.success {
                Notification::success(result.message)
            } else {
                Notification::error(result.message)
            };
            window.push_notification(notification, cx);
        }
        let view = cx.entity();
        let search_input = self.search_input.clone();
        let serial_select = self.serial_select.clone();
        let baud_select = self.baud_select.clone();
        let data_bits_select = self.data_bits_select.clone();
        let parity_select = self.parity_select.clone();
        let stop_bits_select = self.stop_bits_select.clone();
        let tcp_client_ip = self.tcp_client_ip.clone();
        let tcp_client_port = self.tcp_client_port.clone();
        let tcp_server_ip = self.tcp_server_ip.clone();
        let tcp_server_port = self.tcp_server_port.clone();
        let query = search_input.read(cx).value().to_string();
        let visible_pages = Self::visible_pages(&query);

        if let Some(first_visible) = visible_pages.first().copied() {
            if !visible_pages.contains(&self.active_page) {
                self.active_page = first_visible;
            }
        }
        let visible_connections: Vec<SettingsPage> = visible_pages
            .iter()
            .copied()
            .filter(|page| matches!(page, SettingsPage::Connection(_)))
            .collect();
        let visible_general: Vec<SettingsPage> = visible_pages
            .iter()
            .copied()
            .filter(|page| !matches!(page, SettingsPage::Connection(_)))
            .collect();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(SettingsTitleBar::new("设置"))
            .child(
                div().flex_1().child(
                    h_resizable("connection-settings")
                        .child(
                            resizable_panel().size(px(280.)).child(
                                Sidebar::<SidebarMenu>::new("connection-settings-sidebar")
                                    .side(Side::Left)
                                    .w(relative(1.))
                                    .border_0()
                                    .collapsed(false)
                                    .header(
                                        div().w_full().child(
                                            Input::new(&search_input).prefix(IconName::Search),
                                        ),
                                    )
                                    .child({
                                        let mut menu = SidebarMenu::new().child(
                                            SidebarMenuItem::new("连接设置")
                                                .default_open(true)
                                                .click_to_open(true)
                                                .children(visible_connections.iter().copied().map(
                                                    |page| {
                                                        SidebarMenuItem::new(Self::nav_label(page))
                                                            .active(self.active_page == page)
                                                            .on_click({
                                                                let view = view.clone();
                                                                move |_, window, cx| {
                                                                    view.update(cx, |view, cx| {
                                                                        view.set_page(
                                                                            page, window, cx,
                                                                        );
                                                                    });
                                                                }
                                                            })
                                                    },
                                                )),
                                        );
                                        if !visible_general.is_empty() {
                                            menu = menu.child(
                                                SidebarMenuItem::new("通用")
                                                    .default_open(true)
                                                    .click_to_open(true)
                                                    .children(visible_general.iter().copied().map(
                                                        |page| {
                                                            SidebarMenuItem::new(Self::nav_label(
                                                                page,
                                                            ))
                                                            .active(self.active_page == page)
                                                            .on_click({
                                                                let view = view.clone();
                                                                move |_, window, cx| {
                                                                    view.update(cx, |view, cx| {
                                                                        view.set_page(
                                                                            page, window, cx,
                                                                        );
                                                                    });
                                                                }
                                                            })
                                                        },
                                                    )),
                                            );
                                        }
                                        menu
                                    }),
                            ),
                        )
                        .child(resizable_panel().child(div().size_full().child(
                            if visible_pages.is_empty() {
                                v_flex()
                                    .size_full()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(Label::new("没有匹配的设置项").text_lg().font_semibold())
                                    .child(
                                        Label::new("请调整搜索关键词后重试。")
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .into_any_element()
                            } else {
                                div()
                                    .size_full()
                                    .overflow_y_scrollbar()
                                    .child(match self.active_page {
                                        SettingsPage::Connection(ConnectionKind::Serial) => self
                                            .render_serial_panel(
                                                view.clone(),
                                                serial_select,
                                                baud_select,
                                                data_bits_select,
                                                parity_select,
                                                stop_bits_select,
                                                cx,
                                            ),
                                        SettingsPage::Connection(ConnectionKind::TcpClient) => self
                                            .render_tcp_client_panel(
                                                view.clone(),
                                                tcp_client_ip,
                                                tcp_client_port,
                                                cx,
                                            ),
                                        SettingsPage::Connection(ConnectionKind::TcpServer) => self
                                            .render_tcp_server_panel(
                                                view.clone(),
                                                tcp_server_ip,
                                                tcp_server_port,
                                                cx,
                                            ),
                                        SettingsPage::Appearance => {
                                            self.render_appearance_panel(cx)
                                        }
                                        SettingsPage::Language => self.render_language_panel(cx),
                                    })
                                    .into_any_element()
                            },
                        ))),
                ),
            )
    }
}
