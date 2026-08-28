// 参数设置对话框集合
// 包含时间设置、密码修改、通信速率、费率时段表等所有配置对话框

use crate::components::SettingsTitleBar;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::{field, v_form};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectState};
use gpui_component::*;
// ============================================================================
// 时间设置对话框
// ============================================================================

#[allow(dead_code)]
pub struct TimeSettingDialog {
    focus_handle: FocusHandle,
    year_input: Entity<InputState>,
    month_input: Entity<InputState>,
    day_input: Entity<InputState>,
    hour_input: Entity<InputState>,
    minute_input: Entity<InputState>,
    second_input: Entity<InputState>,
    subscriptions: Vec<Subscription>,
    follow_current_time_on_confirm: bool,
    syncing_time_fields: bool,
    on_confirm:
        Option<Box<dyn Fn(chrono::DateTime<Utc>, &mut Window, &mut Context<Self>) + 'static>>,
}

impl TimeSettingDialog {
    pub fn new(
        current_time: chrono::DateTime<Utc>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let year_input = cx.new(|cx| InputState::new(window, cx).placeholder("年"));
        let month_input = cx.new(|cx| InputState::new(window, cx).placeholder("月"));
        let day_input = cx.new(|cx| InputState::new(window, cx).placeholder("日"));
        let hour_input = cx.new(|cx| InputState::new(window, cx).placeholder("时"));
        let minute_input = cx.new(|cx| InputState::new(window, cx).placeholder("分"));
        let second_input = cx.new(|cx| InputState::new(window, cx).placeholder("秒"));

        let mut dialog = Self {
            focus_handle,
            year_input,
            month_input,
            day_input,
            hour_input,
            minute_input,
            second_input,
            subscriptions: Vec::new(),
            follow_current_time_on_confirm: false,
            syncing_time_fields: false,
            on_confirm: None,
        };
        dialog.set_form_time(current_time, false, window, cx);
        dialog.subscribe_to_inputs(cx);
        dialog
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(chrono::DateTime<Utc>, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn subscribe_to_inputs(&mut self, cx: &mut Context<Self>) {
        for input in [
            self.year_input.clone(),
            self.month_input.clone(),
            self.day_input.clone(),
            self.hour_input.clone(),
            self.minute_input.clone(),
            self.second_input.clone(),
        ] {
            self.subscriptions
                .push(cx.subscribe(&input, |this, _, event, cx| {
                    if matches!(event, InputEvent::Change) && !this.syncing_time_fields {
                        this.follow_current_time_on_confirm = false;
                        cx.notify();
                    }
                }));
        }
    }

    fn set_form_time(
        &mut self,
        time: chrono::DateTime<Utc>,
        follow_current_time_on_confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.syncing_time_fields = true;
        self.year_input.update(cx, |state, cx| {
            state.set_value(time.year().to_string(), window, cx);
        });
        self.month_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", time.month()), window, cx);
        });
        self.day_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", time.day()), window, cx);
        });
        self.hour_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", time.hour()), window, cx);
        });
        self.minute_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", time.minute()), window, cx);
        });
        self.second_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", time.second()), window, cx);
        });
        self.syncing_time_fields = false;
        self.follow_current_time_on_confirm = follow_current_time_on_confirm;
    }

    fn parse_form_time(&self, cx: &App) -> Option<chrono::DateTime<Utc>> {
        let year = self
            .year_input
            .read(cx)
            .value()
            .parse::<i32>()
            .unwrap_or(2024);
        let month = self
            .month_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(1)
            .clamp(1, 12);
        let day = self
            .day_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(1)
            .clamp(1, 31);
        let hour = self
            .hour_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(0)
            .clamp(0, 23);
        let minute = self
            .minute_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(0)
            .clamp(0, 59);
        let second = self
            .second_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(0)
            .clamp(0, 59);

        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
    }

    fn set_current_time(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 虚拟时钟存"表面时间"：取本地时间的数字、贴 UTC 标签，
        // 保证表显/协议读数与用户本地钟一致
        self.set_form_time(meter_core::simulation::local_now_as_utc(), true, window, cx);
        cx.notify();
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let datetime = if self.follow_current_time_on_confirm {
            meter_core::simulation::local_now_as_utc()
        } else {
            match self.parse_form_time(cx) {
                Some(datetime) => datetime,
                None => return,
            }
        };

        if let Some(callback) = self.on_confirm.take() {
            callback(datetime, window, cx);
            self.on_confirm = Some(callback);
        }
        window.close_dialog(cx);
    }
}

impl Focusable for TimeSettingDialog {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TimeSettingDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .id("subscription-panel")
            .gap_4()
            .child(
                v_form()
                    .gap_4()
                    .child(
                        field().label("日期").child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(Input::new(&self.year_input).w(px(100.0)))
                                .child(Label::new("-").text_color(theme.colors.muted_foreground))
                                .child(Input::new(&self.month_input).w(px(70.0)))
                                .child(Label::new("-").text_color(theme.colors.muted_foreground))
                                .child(Input::new(&self.day_input).w(px(70.0))),
                        ),
                    )
                    .child(
                        field().label("时间").child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(Input::new(&self.hour_input).w(px(70.0)))
                                .child(Label::new(":").text_color(theme.colors.muted_foreground))
                                .child(Input::new(&self.minute_input).w(px(70.0)))
                                .child(Label::new(":").text_color(theme.colors.muted_foreground))
                                .child(Input::new(&self.second_input).w(px(70.0))),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Button::new("set-now")
                            .label("设置为当前时间")
                            .small()
                            .on_click(cx.listener(Self::set_current_time)),
                    )
                    .children(self.follow_current_time_on_confirm.then(|| {
                        Label::new("已锁定为确认瞬间的本地时间（数字与本地时钟一致）；手动修改任一字段后将改为按表单值提交。")
                            .text_xs()
                            .text_color(theme.colors.muted_foreground)
                    })),
            )
            .child(
                div().flex().flex_row().gap_2().justify_end().mt_4().child(
                    Button::new("confirm-time")
                        .label("确定")
                        .on_click(cx.listener(Self::handle_confirm)),
                ),
            )
    }
}

// ============================================================================
// 密码管理对话框
// ============================================================================

pub struct PasswordDialog {
    level_input: Entity<InputState>,
    old_password_input: Entity<InputState>,
    new_password_input: Entity<InputState>,
    confirm_password_input: Entity<InputState>,
    error_message: Option<SharedString>,
    on_confirm:
        Option<Box<dyn Fn(u8, [u8; 4], [u8; 4], &mut Window, &mut Context<Self>) + 'static>>,
}

impl PasswordDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let level_input = cx.new(|cx| InputState::new(window, cx).placeholder("权限级别(0-9)"));

        // 设置默认值为2级
        level_input.update(cx, |state, cx| {
            state.set_value("2".to_string(), window, cx);
        });

        let old_password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("旧密码（6位十六进制）"));

        let new_password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("新密码（6位十六进制）"));

        let confirm_password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("确认新密码"));

        Self {
            level_input,
            old_password_input,
            new_password_input,
            confirm_password_input,
            error_message: None,
            on_confirm: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(u8, [u8; 4], [u8; 4], &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn parse_password_hex(input: &str) -> Option<[u8; 4]> {
        if input.len() != 6 {
            return None;
        }

        // 解析6位十六进制字符串为3字节密码（PA + P0P1P2格式）
        let mut bytes = [0u8; 4];
        for (i, chunk) in input.as_bytes().chunks(2).enumerate() {
            if i >= 3 {
                break;
            }
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i + 1] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        Some(bytes)
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        // 获取权限级别
        let level_str = self.level_input.read(cx).value();
        let level = level_str.parse::<u8>().unwrap_or(2).clamp(0, 9);

        // 获取密码输入
        let old_pwd_str = self.old_password_input.read(cx).value();
        let new_pwd_str = self.new_password_input.read(cx).value();
        let confirm_pwd_str = self.confirm_password_input.read(cx).value();

        // 验证新密码一致性
        if new_pwd_str != confirm_pwd_str {
            self.error_message = Some("两次输入的新密码不一致".into());
            cx.notify();
            return;
        }

        // 解析密码
        let Some(mut old_password) = Self::parse_password_hex(&old_pwd_str) else {
            self.error_message = Some("旧密码格式错误（需要6位十六进制）".into());
            cx.notify();
            return;
        };

        let Some(mut new_password) = Self::parse_password_hex(&new_pwd_str) else {
            self.error_message = Some("新密码格式错误（需要6位十六进制）".into());
            cx.notify();
            return;
        };

        // 设置权限级别字节
        old_password[0] = level;
        new_password[0] = level;

        // 调用回调
        if let Some(callback) = self.on_confirm.take() {
            callback(level, old_password, new_password, window, cx);
            self.on_confirm = Some(callback);
        }
        window.close_dialog(cx);
    }
}

impl Render for PasswordDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error_msg = self.error_message.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                v_form()
                    .gap_4()
                    .child(
                        field()
                            .label("权限级别(0-9)")
                            .child(Input::new(&self.level_input).w(px(100.0))),
                    )
                    .child(
                        field()
                            .label("旧密码")
                            .child(Input::new(&self.old_password_input)),
                    )
                    .child(
                        field()
                            .label("新密码")
                            .child(Input::new(&self.new_password_input)),
                    )
                    .child(
                        field()
                            .label("确认新密码")
                            .child(Input::new(&self.confirm_password_input)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.colors.muted_foreground)
                    .child(
                    "0级=最高权限(厂家) | 1级=高级(电力公司) | 2级=中级(管理员) | 3-9级=普通(用户)",
                ),
            )
            .children(error_msg.map(|msg| {
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.colors.danger.opacity(0.1))
                    .border_1()
                    .border_color(theme.colors.danger)
                    .child(div().text_sm().text_color(theme.colors.danger).child(msg))
            }))
            .child(
                div().flex().flex_row().gap_2().justify_end().mt_4().child(
                    Button::new("confirm-password")
                        .label("确定")
                        .on_click(cx.listener(Self::handle_confirm)),
                ),
            )
    }
}

// ============================================================================
// 通信速率对话框
// ============================================================================

pub struct BaudrateDialog {
    baudrate_input: Entity<InputState>,
    password_input: Entity<InputState>,
    error_message: Option<SharedString>,
    on_confirm: Option<Box<dyn Fn(u8, [u8; 4], &mut Window, &mut Context<Self>) + 'static>>,
}

impl BaudrateDialog {
    pub fn new(current_baudrate: u8, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let baudrate_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("波特率代码(十六进制)"));

        // 设置当前波特率值
        baudrate_input.update(cx, |state, cx| {
            state.set_value(format!("{:02X}", current_baudrate), window, cx);
        });

        let password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("权限密码（8位十六进制，含级别）"));

        Self {
            baudrate_input,
            password_input,
            error_message: None,
            on_confirm: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(u8, [u8; 4], &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn parse_password(input: &str) -> Option<[u8; 4]> {
        if input.len() != 8 {
            return None;
        }

        let mut bytes = [0u8; 4];
        for (i, chunk) in input.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        Some(bytes)
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        let baudrate_str = self.baudrate_input.read(cx).value();
        let baudrate = u8::from_str_radix(&baudrate_str.to_string(), 16).unwrap_or(0x20);
        let pwd_str = self.password_input.read(cx).value();

        let Some(password) = Self::parse_password(&pwd_str) else {
            self.error_message = Some("密码格式错误（需要8位十六进制）".into());
            cx.notify();
            return;
        };

        if let Some(callback) = self.on_confirm.take() {
            callback(baudrate, password, window, cx);
            self.on_confirm = Some(callback);
        }
        window.close_dialog(cx);
    }
}

impl Render for BaudrateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error_msg = self.error_message.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                v_form()
                    .gap_4()
                    .child(
                        field()
                            .label("通信速率")
                            .child(Input::new(&self.baudrate_input).w(px(150.0))),
                    )
                    .child(
                        field()
                            .label("权限密码")
                            .child(Input::new(&self.password_input)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.colors.muted_foreground)
                    .child(
                    "02=600bps | 04=1200bps | 08=2400bps | 10=4800bps | 20=9600bps | 40=19200bps",
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.colors.muted_foreground)
                    .child("⚠️ 修改通信速率后需要重新连接电表"),
            )
            .children(error_msg.map(|msg| {
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.colors.danger.opacity(0.1))
                    .border_1()
                    .border_color(theme.colors.danger)
                    .child(div().text_sm().text_color(theme.colors.danger).child(msg))
            }))
            .child(
                div().flex().flex_row().gap_2().justify_end().mt_4().child(
                    Button::new("confirm-baudrate")
                        .label("确定")
                        .on_click(cx.listener(Self::handle_confirm)),
                ),
            )
    }
}

// ============================================================================
// 清零操作对话框
// ============================================================================

#[derive(Clone, Copy)]
pub enum ClearType {
    MaxDemand, // 最大需量清零
    Meter,     // 电表清零
}

pub struct ClearOperationDialog {
    clear_type: ClearType,
    password_input: Entity<InputState>,
    operator_code_input: Entity<InputState>,
    error_message: Option<SharedString>,
    on_confirm: Option<Box<dyn Fn([u8; 4], [u8; 4], &mut Window, &mut Context<Self>) + 'static>>,
}

impl ClearOperationDialog {
    pub fn new(clear_type: ClearType, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("权限密码（8位十六进制）"));

        let operator_code_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("操作者代码（8位十六进制）"));

        Self {
            clear_type,
            password_input,
            operator_code_input,
            error_message: None,
            on_confirm: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn([u8; 4], [u8; 4], &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn parse_hex_4bytes(input: &str) -> Option<[u8; 4]> {
        if input.len() != 8 {
            return None;
        }

        let mut bytes = [0u8; 4];
        for (i, chunk) in input.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        Some(bytes)
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        let pwd_str = self.password_input.read(cx).value();
        let op_str = self.operator_code_input.read(cx).value();

        let Some(password) = Self::parse_hex_4bytes(&pwd_str) else {
            self.error_message = Some("密码格式错误（需要8位十六进制）".into());
            cx.notify();
            return;
        };

        let Some(operator_code) = Self::parse_hex_4bytes(&op_str) else {
            self.error_message = Some("操作者代码格式错误（需要8位十六进制）".into());
            cx.notify();
            return;
        };

        if let Some(callback) = self.on_confirm.take() {
            callback(password, operator_code, window, cx);
            self.on_confirm = Some(callback);
        }
        window.close_dialog(cx);
    }
}

impl Render for ClearOperationDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error_msg = self.error_message.clone();

        let (title, warning) = match self.clear_type {
            ClearType::MaxDemand => (
                "最大需量清零",
                "此操作将清除最大需量记录和发生时间，操作不可恢复",
            ),
            ClearType::Meter => (
                "电表清零（危险操作）",
                "此操作将清除所有电能量、需量、冻结和事件数据，操作不可恢复！",
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Label::new(title).text_lg().font_semibold())
                    .child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .bg(theme.colors.danger.opacity(0.1))
                            .border_1()
                            .border_color(theme.colors.danger)
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.colors.danger)
                                    .child(warning),
                            ),
                    ),
            )
            .child(
                v_form()
                    .gap_4()
                    .child(
                        field()
                            .label("权限密码")
                            .child(Input::new(&self.password_input)),
                    )
                    .child(
                        field()
                            .label("操作者代码")
                            .child(Input::new(&self.operator_code_input)),
                    ),
            )
            .children(error_msg.map(|msg| {
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.colors.danger.opacity(0.1))
                    .border_1()
                    .border_color(theme.colors.danger)
                    .child(div().text_sm().text_color(theme.colors.danger).child(msg))
            }))
            .child(
                div().flex().flex_row().gap_2().justify_end().mt_4().child(
                    Button::new("confirm-clear")
                        .label("确认清零")
                        .on_click(cx.listener(Self::handle_confirm)),
                ),
            )
    }
}

// ============================================================================
// TOU费率时段表对话框
// ============================================================================

pub struct TouConfigDialog {
    num_rates_input: Entity<InputState>,
    time_slots: Vec<TimeSlotInput>,
    error_message: Option<SharedString>,
    on_confirm: Option<Box<dyn Fn(Vec<(u8, u8, u8)>, &mut Window, &mut Context<Self>) + 'static>>,
}

struct TimeSlotInput {
    hour_input: Entity<InputState>,
    minute_input: Entity<InputState>,
    rate_input: Entity<InputState>,
}

impl TouConfigDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let num_rates_input = cx.new(|cx| InputState::new(window, cx).placeholder("费率数(1-63)"));

        // 设置初始费率数
        num_rates_input.update(cx, |state, cx| {
            state.set_value("4".to_string(), window, cx);
        });

        // 创建默认的4费率时段（尖峰平谷）
        let time_slots = vec![
            Self::create_time_slot(0, 0, 4, window, cx),  // 00:00 谷
            Self::create_time_slot(8, 0, 2, window, cx),  // 08:00 峰
            Self::create_time_slot(12, 0, 3, window, cx), // 12:00 平
            Self::create_time_slot(18, 0, 1, window, cx), // 18:00 尖
            Self::create_time_slot(22, 0, 4, window, cx), // 22:00 谷
        ];

        Self {
            num_rates_input,
            time_slots,
            error_message: None,
            on_confirm: None,
        }
    }

    fn create_time_slot(
        hour: u8,
        minute: u8,
        rate: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TimeSlotInput {
        let hour_input = cx.new(|cx| InputState::new(window, cx));
        let minute_input = cx.new(|cx| InputState::new(window, cx));
        let rate_input = cx.new(|cx| InputState::new(window, cx));

        // 设置初始值
        hour_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", hour), window, cx);
        });
        minute_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", minute), window, cx);
        });
        rate_input.update(cx, |state, cx| {
            state.set_value(rate.to_string(), window, cx);
        });

        TimeSlotInput {
            hour_input,
            minute_input,
            rate_input,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(Vec<(u8, u8, u8)>, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn add_time_slot(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.time_slots.len() >= 14 {
            self.error_message = Some("最多支持14个时段".into());
            cx.notify();
            return;
        }

        self.time_slots
            .push(Self::create_time_slot(0, 0, 1, window, cx));
        self.error_message = None;
        cx.notify();
    }

    fn remove_time_slot(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if self.time_slots.len() <= 1 {
            self.error_message = Some("至少需要1个时段".into());
            cx.notify();
            return;
        }

        if index < self.time_slots.len() {
            self.time_slots.remove(index);
            self.error_message = None;
            cx.notify();
        }
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        // 解析所有时段
        let mut slots = Vec::new();
        for slot in &self.time_slots {
            let hour = slot
                .hour_input
                .read(cx)
                .value()
                .parse::<u8>()
                .unwrap_or(0)
                .clamp(0, 23);
            let minute = slot
                .minute_input
                .read(cx)
                .value()
                .parse::<u8>()
                .unwrap_or(0)
                .clamp(0, 59);
            let rate = slot
                .rate_input
                .read(cx)
                .value()
                .parse::<u8>()
                .unwrap_or(1)
                .clamp(1, 63);

            slots.push((hour, minute, rate));
        }

        // 验证时段按时间升序
        for i in 1..slots.len() {
            let (h1, m1, _) = slots[i - 1];
            let (h2, m2, _) = slots[i];
            let time1 = h1 as u16 * 60 + m1 as u16;
            let time2 = h2 as u16 * 60 + m2 as u16;

            if time2 <= time1 {
                self.error_message = Some("时段必须按时间升序排列".into());
                cx.notify();
                return;
            }
        }

        if let Some(callback) = self.on_confirm.take() {
            callback(slots, window, cx);
            self.on_confirm = Some(callback);
        }
        window.close_dialog(cx);
    }
}

impl Render for TouConfigDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error_msg = self.error_message.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.colors.muted_foreground)
                    .child("配置日时段表（最多14个时段，时间必须升序）"),
            )
            .child(
                v_form().flex_shrink_0().gap_4().child(
                    field()
                        .label("费率数")
                        .child(Input::new(&self.num_rates_input).w(px(100.0))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .child(Label::new("时段列表").text_sm().font_semibold())
                            .child(
                                Button::new("add-slot")
                                    .label("+ 添加时段")
                                    .small()
                                    .on_click(cx.listener(Self::add_time_slot)),
                            ),
                    )
                    .child(
                        div()
                            .id("tou-time-slots-list")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .gap_2()
                            .overflow_y_scrollbar()
                            .children(self.time_slots.iter().enumerate().map(|(idx, slot)| {
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .p_2()
                                    .rounded_lg()
                                    .bg(theme.colors.muted.opacity(0.5))
                                    .child(
                                        Label::new(format!("#{}", idx + 1)).text_sm().w(px(40.0)),
                                    )
                                    .child(Input::new(&slot.hour_input).w(px(60.0)))
                                    .child(Label::new(":"))
                                    .child(Input::new(&slot.minute_input).w(px(60.0)))
                                    .child(Label::new("费率"))
                                    .child(Input::new(&slot.rate_input).w(px(60.0)))
                                    .child(
                                        Button::new(format!("remove-{}", idx))
                                            .icon(IconName::Delete)
                                            .small()
                                            .ghost()
                                            .on_click({
                                                let idx = idx;
                                                cx.listener(move |this, _, window, cx| {
                                                    this.remove_time_slot(idx, window, cx);
                                                })
                                            }),
                                    )
                            })),
                    ),
            )
            .children(error_msg.map(|msg| {
                div()
                    .flex_shrink_0()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.colors.danger.opacity(0.1))
                    .border_1()
                    .border_color(theme.colors.danger)
                    .child(div().text_sm().text_color(theme.colors.danger).child(msg))
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .mt_4()
                    .child(
                        Button::new("confirm-tou")
                            .label("确定")
                            .on_click(cx.listener(Self::handle_confirm)),
                    ),
            )
    }
}
// ============================================================================
// 新增 / 复制表对话框
// ============================================================================

/// "复制自"列表里"不复制，新建默认表"选项对应的 sentinel（用 `None` 表示，
/// 这个常量只用来在选项列表第 0 项上显示文案）。
const NO_COPY_SOURCE_LABEL: &str = "不复制（新建默认表）";

/// 电表列表面板顶部"添加表"入口的对话框：填新地址，可选"复制自"某块已有表。
/// 选了复制来源时，新表会复制该表的仿真/协议/冻结/负荷记录等全部配置与历史
/// 数据，只有地址不同；不选则新建一块默认配置的表。
///
/// "复制自"使用 `Select` 下拉；菜单限制最大高度，避免来源较多时超出对话框。
/// Select 的弹出菜单由组件 overlay 层渲染，不会被对话框后续内容覆盖。
pub struct AddMeterView {
    address_input: Entity<InputState>,
    count_input: Entity<InputState>,
    /// 已存在的地址集合：用来做新地址判重。
    existing_addresses: Vec<String>,
    /// "复制自"下拉选项的状态；首项 sentinel 表示不复制。
    source_select: Entity<SelectState<Vec<String>>>,
    error_message: Option<SharedString>,
    on_confirm: Option<
        Box<dyn Fn(Vec<[u8; 6]>, Option<String>, &mut Window, &mut Context<Self>) + 'static>,
    >,
}

impl AddMeterView {
    pub fn new(
        existing_addresses: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("新表地址（十进制，不足12位自动补0，如 39）")
        });
        let count_input = cx.new(|cx| InputState::new(window, cx).placeholder("创建数量，如 10"));
        count_input.update(cx, |state, cx| {
            state.set_value("1".to_string(), window, cx);
        });

        let source_items = std::iter::once(NO_COPY_SOURCE_LABEL.to_string())
            .chain(existing_addresses.iter().cloned())
            .collect();
        let source_select = cx.new(|cx| SelectState::new(source_items, None, window, cx));

        Self {
            address_input,
            count_input,
            existing_addresses,
            source_select,
            error_message: None,
            on_confirm: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(Vec<[u8; 6]>, Option<String>, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        let mut addr_str = self.address_input.read(cx).value().trim().to_uppercase();
        // 电表地址按 12 位十进制数字处理，右对齐并在左侧补 0。
        if addr_str.len() < 12 {
            addr_str = format!("{addr_str:0>12}");
        }
        let Ok(start_value) = addr_str.parse::<u64>() else {
            self.error_message = Some("地址格式错误，需要 1~12 位十进制数字".into());
            cx.notify();
            return;
        };
        if addr_str.len() != 12 || start_value > 999_999_999_999 {
            self.error_message = Some("地址格式错误，需要 1~12 位十进制数字".into());
            cx.notify();
            return;
        }
        let count = match self.count_input.read(cx).value().trim().parse::<u16>() {
            Ok(count) if (1..=1000).contains(&count) => count,
            _ => {
                window.push_notification(
                    Notification::error("创建数量必须是 1 到 1000 之间的整数"),
                    cx,
                );
                return;
            }
        };

        let end_value = match start_value.checked_add(u64::from(count - 1)) {
            Some(value) if value <= 999_999_999_999 => value,
            _ => {
                window
                    .push_notification(Notification::error("地址范围超出 12 位十进制地址上限"), cx);
                return;
            }
        };
        let addresses = (start_value..=end_value)
            .map(|value| {
                meter_core::protocol::format::parse_address(&format!("{value:012}"))
                    .expect("validated decimal address must be a valid BCD address")
            })
            .collect::<Vec<_>>();

        let mut seen = self.existing_addresses.clone();
        if let Some(duplicate) = addresses
            .iter()
            .map(meter_core::protocol::format::format_address)
            .find(|address| seen.iter().any(|item| item == address))
        {
            window.push_notification(
                Notification::error(format!("地址 {duplicate} 已存在，请调整起始地址或创建数量")),
                cx,
            );
            return;
        }
        seen.extend(
            addresses
                .iter()
                .map(meter_core::protocol::format::format_address),
        );
        if seen.len() != self.existing_addresses.len() + addresses.len() {
            let duplicate = addresses
                .iter()
                .map(meter_core::protocol::format::format_address)
                .find(|address| {
                    addresses
                        .iter()
                        .filter(|item| {
                            meter_core::protocol::format::format_address(item) == *address
                        })
                        .count()
                        > 1
                })
                .unwrap_or_default();
            window.push_notification(
                Notification::error(format!("地址 {duplicate} 重复，请调整创建数量")),
                cx,
            );
            return;
        }

        let source = self
            .source_select
            .read(cx)
            .selected_value()
            .filter(|value| *value != NO_COPY_SOURCE_LABEL)
            .cloned();

        if let Some(callback) = self.on_confirm.take() {
            callback(addresses, source, window, cx);
            self.on_confirm = Some(callback);
        }
        window.remove_window();
    }
}

impl Render for AddMeterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let error_msg = self.error_message.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(SettingsTitleBar::new("添加表"))
            .child(
                div().flex_1().p_6().child(
                    v_flex()
                        .gap_4()
                        .child(
                            v_form().gap_4().child(
                                field()
                                    .label("新表地址")
                                    .child(Input::new(&self.address_input)),
                            ),
                        )
                        .child(
                            field()
                                .label("创建数量")
                                .child(Input::new(&self.count_input).w(px(180.))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(Label::new("复制自").text_sm())
                                .child(
                                    Select::new(&self.source_select)
                                        .w_full()
                                        .placeholder(NO_COPY_SOURCE_LABEL)
                                        .menu_max_h(px(200.)),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.colors.muted_foreground)
                                .child(
                                    "选择\"复制自\"某块表时，新表会复制该表的仿真/协议/冻结/\
                                         负荷记录等全部配置及历史数据，仅地址不同；不选则新建\
                                         一块默认配置的表。",
                                ),
                        )
                        .children(error_msg.map(|msg| {
                            div()
                                .p_3()
                                .rounded_lg()
                                .bg(theme.colors.danger.opacity(0.1))
                                .border_1()
                                .border_color(theme.colors.danger)
                                .child(div().text_sm().text_color(theme.colors.danger).child(msg))
                        }))
                        .child(
                            div().flex().flex_row().gap_2().justify_end().mt_4().child(
                                Button::new("confirm-add-meter")
                                    .label("确定")
                                    .on_click(cx.listener(Self::handle_confirm)),
                            ),
                        ),
                ),
            )
            .children(Root::render_notification_layer(window, cx))
    }
}

// ============================================================================
// 修改表地址对话框
// ============================================================================

/// 电表列表"修改地址"入口的对话框：显示当前地址，填一个新地址。
///
/// 地址格式与"添加表"（`AddMeterView`）一致：按 12 位十进制数字处理（右
/// 对齐、左侧补 0、上限 999999999999），并校验不得与当前地址相同、不得与
/// 其他已存在地址重复。仅换地址，该表的配置与历史数据全部保留——内存与
/// 数据库的同步搬移由 `AppBackend::update_meter_address` 完成。
pub struct ModifyAddressView {
    current_address: String,
    new_address_input: Entity<InputState>,
    /// 除当前表外已存在的地址集合，用来做新地址判重。
    existing_addresses: Vec<String>,
    error_message: Option<SharedString>,
    /// 监听输入变化：出错后用户一旦改动输入就撤销错误提示，
    /// 避免过期的"地址已存在"一直挂着误导操作。
    subscriptions: Vec<Subscription>,
    on_confirm: Option<Box<dyn Fn(String, [u8; 6], &mut Window, &mut Context<Self>) + 'static>>,
}

impl ModifyAddressView {
    pub fn new(
        current_address: impl Into<String>,
        existing_addresses: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current_address: String = current_address.into();
        let new_address_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("新地址（十进制，不足12位自动补0，如 39）")
        });
        // 判重集合排除当前地址本身：改成别的表的地址由这份数据拦截，
        // "与当前地址相同"的情况单独给出更明确的提示。
        let existing_addresses = existing_addresses
            .into_iter()
            .filter(|address| *address != current_address)
            .collect();

        let mut view = Self {
            current_address,
            new_address_input: new_address_input.clone(),
            existing_addresses,
            error_message: None,
            subscriptions: Vec::new(),
            on_confirm: None,
        };
        view.subscriptions.push(cx.subscribe(
            &new_address_input,
            |this, _, event, cx| {
                if matches!(event, InputEvent::Change) && this.error_message.take().is_some() {
                    cx.notify();
                }
            },
        ));
        view
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(String, [u8; 6], &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_message = None;

        let mut addr_str = self
            .new_address_input
            .read(cx)
            .value()
            .trim()
            .to_uppercase();
        // 与"添加表"一致：电表地址按 12 位十进制数字处理，右对齐并在左侧补 0。
        if addr_str.len() < 12 {
            addr_str = format!("{addr_str:0>12}");
        }
        let Ok(value) = addr_str.parse::<u64>() else {
            self.error_message = Some("地址格式错误，需要 1~12 位十进制数字".into());
            cx.notify();
            return;
        };
        if addr_str.len() != 12 || value > 999_999_999_999 {
            self.error_message = Some("地址格式错误，需要 1~12 位十进制数字".into());
            cx.notify();
            return;
        }
        let new_address_str = format!("{value:012}");
        if new_address_str == self.current_address {
            self.error_message = Some("新地址与当前地址相同".into());
            cx.notify();
            return;
        }
        if self
            .existing_addresses
            .iter()
            .any(|address| *address == new_address_str)
        {
            self.error_message = Some(format!("地址 {new_address_str} 已存在").into());
            cx.notify();
            return;
        }
        let new_address = meter_core::protocol::format::parse_address(&new_address_str)
            .expect("validated decimal address must be a valid BCD address");

        if let Some(callback) = self.on_confirm.take() {
            callback(self.current_address.clone(), new_address, window, cx);
            self.on_confirm = Some(callback);
        }
        window.remove_window();
    }
}

impl Render for ModifyAddressView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let error_msg = self.error_message.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(SettingsTitleBar::new("修改表地址"))
            .child(
                div().flex_1().p_6().child(
                    v_flex()
                        .gap_4()
                        .child(
                            v_form().gap_4().child(
                                field().label("当前地址").child(
                                    div()
                                        .text_sm()
                                        .py_1()
                                        .text_color(theme.colors.muted_foreground)
                                        .child(self.current_address.clone()),
                                ),
                            ),
                        )
                        .child(
                            field()
                                .label("新地址")
                                .child(Input::new(&self.new_address_input)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.colors.muted_foreground)
                                .child(
                                    "仅修改地址，该表的仿真/协议/冻结/负荷记录等全部配置\
                                         及历史数据保持不变。",
                                ),
                        )
                        .children(error_msg.map(|msg| {
                            div()
                                .p_3()
                                .rounded_lg()
                                .bg(theme.colors.danger.opacity(0.1))
                                .border_1()
                                .border_color(theme.colors.danger)
                                .child(div().text_sm().text_color(theme.colors.danger).child(msg))
                        }))
                        .child(
                            div().flex().flex_row().gap_2().justify_end().mt_4().child(
                                Button::new("confirm-modify-address")
                                    .label("确定")
                                    .on_click(cx.listener(Self::handle_confirm)),
                            ),
                        ),
                ),
            )
            .children(Root::render_notification_layer(window, cx))
    }
}
