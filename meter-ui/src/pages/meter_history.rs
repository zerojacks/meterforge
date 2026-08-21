//! 事件记录 / 冻结数据 / 负荷记录：单条记录的渲染 helper。
//!
//! 三个 tab 各自的列表虚拟滚动（`gpui::list` + `ListState`）状态持有在
//! `MeterDetailView` 里（与 `CommunicationLogPanel` 同一套模式），本模块只
//! 负责“一条记录长什么样”，不关心分页/虚拟化。

use chrono::Utc;
use gpui::*;
use gpui_component::StyledExt;
use gpui_component::{h_flex, label::Label, Theme};
use meter_core::snapshot::{EventSnapshot, FreezeSnapshotSummary, LoadRecordSnapshot};
/// 冻结触发类型筛选（对应协议 DI2 的几大类），用于“冻结数据”tab 的筛选条。
///
/// `Settlement` 对应 DI2=00（`FreezeTrigger::Timed`），即协议里的“定时冻结”，
/// 按结算日/月周期触发，也就是通常说的“月冻结（结算日）数据”。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum FreezeFilter {
    #[default]
    All,
    /// 月冻结（结算日），DI2=00
    Settlement,
    /// 日冻结，DI2=06
    Daily,
    /// 瞬时冻结，DI2=01
    Instant,
    /// 其余触发类型（时区表切换/日时段表切换/整点/阶梯切换）
    Other,
}

impl FreezeFilter {
    pub const ALL: [FreezeFilter; 5] = [
        FreezeFilter::All,
        FreezeFilter::Settlement,
        FreezeFilter::Daily,
        FreezeFilter::Instant,
        FreezeFilter::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FreezeFilter::All => "全部",
            FreezeFilter::Settlement => "月冻结（结算日）",
            FreezeFilter::Daily => "日冻结",
            FreezeFilter::Instant => "瞬时冻结",
            FreezeFilter::Other => "其他触发",
        }
    }

    pub fn matches(self, trigger: &str) -> bool {
        match self {
            FreezeFilter::All => true,
            FreezeFilter::Settlement => trigger == "Timed",
            FreezeFilter::Daily => trigger == "Daily",
            FreezeFilter::Instant => trigger == "Instant",
            FreezeFilter::Other => matches!(
                trigger,
                "TimeZoneSwitch" | "DayTableSwitch" | "Hourly" | "LadderSwitch"
            ),
        }
    }
}

/// 冻结触发类型的中文展示名（原始值是 `FreezeTrigger` 的 Debug 字符串，如 "Timed"）。
pub fn freeze_trigger_label(trigger: &str) -> &str {
    match trigger {
        "Timed" => "月冻结（结算日）",
        "Instant" => "瞬时冻结",
        "TimeZoneSwitch" => "时区表切换冻结",
        "DayTableSwitch" => "日时段表切换冻结",
        "Hourly" => "整点冻结",
        "Daily" => "日冻结",
        "LadderSwitch" => "阶梯切换冻结",
        other => other,
    }
}

fn format_time_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|v| {
            v.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// 渲染单条事件记录。
pub fn render_event_item(event: &EventSnapshot, theme: &Theme) -> impl IntoElement {
    let name = match event.event_type {
        0x01 => "失压事件",
        0x02 => "失流事件",
        0x30 => "编程记录",
        0x32 => "清零记录",
        _ => "其他事件",
    };
    let time = format_time_ms(event.start_time_ms);
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
}

/// 渲染单条负荷记录。
pub fn render_load_record_item(record: &LoadRecordSnapshot, theme: &Theme) -> impl IntoElement {
    let time = format_time_ms(record.sample_time_ms);

    let mut key_values = Vec::new();
    if let Some(v) = record.voltage_a {
        key_values.push(format!("电压A: {:.1} V", v));
    }
    if let Some(v) = record.current_a {
        key_values.push(format!("电流A: {:.2} A", v));
    }
    if let Some(v) = record.active_power_kw {
        key_values.push(format!("有功功率: {:.2} kW", v));
    }
    if let Some(v) = record.energy_forward_active_kwh {
        key_values.push(format!("正向有功电能: {:.2} kWh", v));
    }

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
                        "{} · {}",
                        record.class_label(),
                        record.blocks_summary()
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
                .pt_2()
                .children(key_values.iter().take(4).map(|kv| {
                    Label::new(kv.clone())
                        .text_sm()
                        .text_color(theme.muted_foreground)
                })),
        )
}

/// 渲染单条冻结快照。
pub fn render_freeze_item(freeze: &FreezeSnapshotSummary, theme: &Theme) -> impl IntoElement {
    let time = format_time_ms(freeze.snapshot_time_ms);
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
                        "{} #{}",
                        freeze_trigger_label(&freeze.trigger),
                        freeze.occurrence_index
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
                .flex_wrap()
                .child(
                    Label::new(format!("正向有功 {:.2} kWh", freeze.forward_active_kwh)).text_sm(),
                )
                .child(Label::new(format!("最大需量 {:.4} kW", freeze.max_demand_kw)).text_sm())
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
}
