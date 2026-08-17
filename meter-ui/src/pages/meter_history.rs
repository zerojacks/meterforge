//! 事件记录与冻结数据展示区域。
use crate::types::MeterSnapshot;
use chrono::Local;
use gpui::*;
use gpui_component::{h_flex, label::Label, *};

pub fn events(snapshot: &MeterSnapshot, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(Label::new("事件记录").text_2xl().font_semibold())
        .child(
            Label::new(format!(
                "共 {} 条记录，按发生时间倒序显示",
                snapshot.events.len()
            ))
            .text_sm()
            .text_color(theme.muted_foreground),
        )
        .children(snapshot.events.iter().map(|event| {
            let name = match event.event_type {
                0x01 => "失压事件",
                0x02 => "失流事件",
                0x30 => "编程记录",
                0x32 => "清零记录",
                _ => "其他事件",
            };
            let time = chrono::DateTime::from_timestamp_millis(event.start_time_ms)
                .map(|v| {
                    v.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            div()
                .w_full()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .justify_between()
                        .child(Label::new(name).font_semibold())
                        .child(
                            Label::new(time)
                                .text_sm()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(
                    Label::new(format!(
                        "子类 {:02X} · 数据 {}",
                        event.sub_type,
                        if event.data_hex.is_empty() {
                            "—"
                        } else {
                            &event.data_hex
                        }
                    ))
                    .text_sm()
                    .text_color(theme.muted_foreground),
                )
        }))
}

pub fn freezes(snapshot: &MeterSnapshot, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(Label::new("冻结数据").text_2xl().font_semibold())
        .child(
            Label::new(format!(
                "共 {} 条冻结快照，按冻结时间倒序显示",
                snapshot.freezes.len()
            ))
            .text_sm()
            .text_color(theme.muted_foreground),
        )
        .children(snapshot.freezes.iter().map(|freeze| {
            let time = chrono::DateTime::from_timestamp_millis(freeze.snapshot_time_ms)
                .map(|v| {
                    v.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            div()
                .w_full()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            Label::new(format!(
                                "{} 冻结 #{}",
                                freeze.trigger, freeze.occurrence_index
                            ))
                            .font_semibold(),
                        )
                        .child(
                            Label::new(time)
                                .text_sm()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(
                    h_flex()
                        .gap_6()
                        .child(
                            Label::new(format!("正向有功 {:.2} kWh", freeze.forward_active_kwh))
                                .text_sm(),
                        )
                        .child(
                            Label::new(format!("最大需量 {:.2} kW", freeze.max_demand_kw))
                                .text_sm(),
                        )
                        .child(
                            Label::new(format!(
                                "A相电压 {} V",
                                freeze
                                    .voltage_a
                                    .map(|v| format!("{v:.1}"))
                                    .unwrap_or_else(|| "—".into())
                            ))
                            .text_sm(),
                        ),
                )
        }))
}
