// 参数设置对话框集合
// 包含时间设置、密码修改、通信速率、费率时段表等所有配置对话框

use chrono::{Datelike, Local, TimeZone, Timelike};
use gpui::*;
use gpui_component::button::Button;
use gpui_component::form::{field, v_form};
use gpui_component::input::{Input, InputState};
use gpui_component::label::Label;
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
// ============================================================================
// 时间设置对话框
// ============================================================================

#[allow(dead_code)]
pub struct TimeSettingDialog {
    year_input: Entity<InputState>,
    month_input: Entity<InputState>,
    day_input: Entity<InputState>,
    hour_input: Entity<InputState>,
    minute_input: Entity<InputState>,
    second_input: Entity<InputState>,
    on_confirm:
        Option<Box<dyn Fn(chrono::DateTime<Local>, &mut Window, &mut Context<Self>) + 'static>>,
}

impl TimeSettingDialog {
    pub fn new(
        current_time: chrono::DateTime<Local>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let year_input = cx.new(|cx| InputState::new(window, cx).placeholder("年"));
        let month_input = cx.new(|cx| InputState::new(window, cx).placeholder("月"));
        let day_input = cx.new(|cx| InputState::new(window, cx).placeholder("日"));
        let hour_input = cx.new(|cx| InputState::new(window, cx).placeholder("时"));
        let minute_input = cx.new(|cx| InputState::new(window, cx).placeholder("分"));
        let second_input = cx.new(|cx| InputState::new(window, cx).placeholder("秒"));

        // 设置初始值
        year_input.update(cx, |state, cx| {
            state.set_value(current_time.year().to_string(), window, cx);
        });
        month_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", current_time.month()), window, cx);
        });
        day_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", current_time.day()), window, cx);
        });
        hour_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", current_time.hour()), window, cx);
        });
        minute_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", current_time.minute()), window, cx);
        });
        second_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", current_time.second()), window, cx);
        });

        Self {
            year_input,
            month_input,
            day_input,
            hour_input,
            minute_input,
            second_input,
            on_confirm: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(chrono::DateTime<Local>, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    fn set_current_time(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Local::now();
        self.year_input.update(cx, |state, cx| {
            state.set_value(now.year().to_string(), window, cx);
        });
        self.month_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", now.month()), window, cx);
        });
        self.day_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", now.day()), window, cx);
        });
        self.hour_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", now.hour()), window, cx);
        });
        self.minute_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", now.minute()), window, cx);
        });
        self.second_input.update(cx, |state, cx| {
            state.set_value(format!("{:02}", now.second()), window, cx);
        });
        cx.notify();
    }

    fn handle_confirm(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 解析输入的时间
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

        // 构造时间
        if let Some(datetime) = Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
        {
            if let Some(callback) = self.on_confirm.take() {
                callback(datetime, window, cx);
                self.on_confirm = Some(callback);
            }
        }
    }
}

impl Render for TimeSettingDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .flex()
            .flex_col()
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
                div().flex().flex_row().gap_2().child(
                    Button::new("set-now")
                        .label("设置为当前时间")
                        .small()
                        .on_click(cx.listener(Self::set_current_time)),
                ),
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
    }
}

impl Render for TouConfigDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error_msg = self.error_message.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Label::new("费率时段表配置").text_lg().font_semibold())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.colors.muted_foreground)
                            .child("配置日时段表（最多14个时段，时间必须升序）"),
                    ),
            )
            .child(
                v_form().gap_4().child(
                    field()
                        .label("费率数")
                        .child(Input::new(&self.num_rates_input).w(px(100.0))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
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
                            .flex()
                            .flex_col()
                            .gap_2()
                            .max_h(px(300.0))
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
                                            .label("🗑️")
                                            .small()
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
                    .p_3()
                    .rounded_lg()
                    .bg(theme.colors.danger.opacity(0.1))
                    .border_1()
                    .border_color(theme.colors.danger)
                    .child(div().text_sm().text_color(theme.colors.danger).child(msg))
            }))
            .child(
                div().flex().flex_row().gap_2().justify_end().mt_4().child(
                    Button::new("confirm-tou")
                        .label("确定")
                        .on_click(cx.listener(Self::handle_confirm)),
                ),
            )
    }
}
