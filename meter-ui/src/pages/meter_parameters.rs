//! 参数设置卡片：展示与触发动作分离，具体命令仍由详情视图统一处理。
use super::meter_detail::MeterDetailView;
use crate::types::MeterSnapshot;
use chrono::{Datelike, FixedOffset, Local, Timelike, Utc};
use gpui::*;
use gpui_component::{
    badge::Badge,
    button::{Button, ButtonVariants},
    label::Label,
    *,
};

fn card(
    title: &str,
    description: &str,
    action: Button,
    content: impl IntoElement,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_5()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    v_flex()
                        .gap_1()
                        .child(Label::new(title).text_lg().font_semibold())
                        .child(
                            Label::new(description)
                                .text_sm()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(action),
        )
        .child(content)
}

pub fn render(
    view: &MeterDetailView,
    snapshot: &MeterSnapshot,
    cx: &mut Context<MeterDetailView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let time = chrono::DateTime::from_timestamp_millis(snapshot.virtual_time_ms)
        .unwrap_or_else(|| Utc::now().into())
        .with_timezone(&chrono::Utc);
    div()
        .flex()
        .flex_col()
        .w_full()
        .items_stretch()
        .gap_5()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(Label::new("参数设置").text_2xl().font_semibold())
                .child(Badge::new().child("需要密码验证")),
        )
        .child(
            Label::new("参数写入通过 meter-core 的管理员命令执行。 ")
                .text_sm()
                .text_color(theme.muted_foreground),
        )
        .child(card(
            "电表时间设置",
            "DI: 04-00-01-01 / 04-00-01-02",
            Button::new("set-time-dialog")
                .label("设置时间")
                .small()
                .on_click(cx.listener({
                    let snapshot = snapshot.clone();
                    move |view, _, window, cx| view.show_time_setting_dialog(&snapshot, window, cx)
                })),
            h_flex()
                .gap_8()
                .p_4()
                .rounded_lg()
                .bg(theme.muted)
                .child(
                    v_flex()
                        .child(
                            Label::new("当前日期")
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            Label::new(format!(
                                "{:04}-{:02}-{:02}",
                                time.year(),
                                time.month(),
                                time.day()
                            ))
                            .text_xl()
                            .font_semibold(),
                        ),
                )
                .child(
                    v_flex()
                        .child(
                            Label::new("当前时间")
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            Label::new(format!(
                                "{:02}:{:02}:{:02}",
                                time.hour(),
                                time.minute(),
                                time.second()
                            ))
                            .text_xl()
                            .font_semibold(),
                        ),
                ),
            theme,
        ))
        .child(card(
            "最大需量清零",
            "控制码: 19H",
            Button::new("clear-demand-dialog")
                .label("执行清零")
                .small()
                .danger()
                .on_click(cx.listener(MeterDetailView::show_clear_demand_dialog)),
            Label::new(format!("当前最大需量：{:.4} kW", snapshot.max_demand_kw)).text_sm(),
            theme,
        ))
        .child(card(
            "密码管理",
            "DI: 04-00-0C-xx",
            Button::new("password-dialog")
                .label("修改密码")
                .small()
                .on_click(cx.listener(MeterDetailView::show_password_dialog)),
            Label::new("支持普通、抄表与管理员权限密码。 ").text_sm(),
            theme,
        ))
        .child(card(
            "通信速率",
            "DI: 04-00-07-03 / 控制码: 17H",
            Button::new("baudrate-dialog")
                .label("修改速率")
                .small()
                .on_click(cx.listener({
                    let snapshot = snapshot.clone();
                    move |view, _, window, cx| view.show_baudrate_dialog(&snapshot, window, cx)
                })),
            Label::new("通信速率修改将由电表核心状态保存。 ").text_sm(),
            theme,
        ))
        .child(card(
            "费率时段表",
            "DI: 04-00-02-xx / 04-00-03-xx",
            Button::new("tou-dialog")
                .label("配置费率")
                .small()
                .on_click(cx.listener(MeterDetailView::show_tou_dialog)),
            Label::new("配置日时段与费率编号。 ").text_sm(),
            theme,
        ))
        .child(card(
            "电表清零",
            "控制码: 1AH",
            Button::new("clear-meter-dialog")
                .label("电表清零")
                .small()
                .danger()
                .on_click(cx.listener(MeterDetailView::show_clear_meter_dialog)),
            Label::new("清零电能与最大需量；事件和冻结记录会保留。 ")
                .text_sm()
                .text_color(theme.danger),
            theme,
        ))
}
