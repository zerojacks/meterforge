//! 事件记录与冻结数据展示区域。
use crate::types::MeterSnapshot;
use chrono::{Local, Utc};
use gpui::*;
use gpui_component::{h_flex, label::Label, v_flex, *};
use meter_core::snapshot::{FreezeSnapshotSummary, LoadRecordSummary};

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
                    v.with_timezone(&Utc)
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

/// 渲染负荷记录标签页。
///
/// 新方案：展示 load_profile_records 表的完整记录块，每条记录显示：
/// - 类别（第1~6类负荷记录）
/// - 采样时间
/// - 选通的数据块摘要（电压/电流/功率等）
/// - 关键数值（展开后显示）
pub fn load_profile(
    history: Option<&[LoadRecordSummary]>,
    loading: bool,
    theme: &Theme,
) -> impl IntoElement {
    let items = history.unwrap_or(&[]);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(Label::new("负荷记录").text_2xl().font_semibold())
        .child(
            Label::new(if loading && history.is_none() {
                "正在从数据库加载负荷记录…".to_string()
            } else if items.is_empty() {
                "暂无负荷记录采样".to_string()
            } else {
                format!("最近 {} 条记录，按采样时间倒序显示", items.len())
            })
            .text_sm()
            .text_color(theme.muted_foreground),
        )
        .children(items.iter().map(|record| {
            let time = chrono::DateTime::from_timestamp_millis(record.sample_time_ms)
                .map(|v| {
                    v.with_timezone(&Utc)
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
                    // 头部：类别 + 时间
                    h_flex()
                        .justify_between()
                        .child(
                            Label::new(format!("{} · {}", record.class_label, record.blocks_summary))
                                .font_semibold(),
                        )
                        .child(
                            Label::new(time)
                                .text_sm()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(
                    // 关键数值展示（横向排列）
                    h_flex()
                        .gap_6()
                        .pt_2()
                        .children(record.key_values.iter().take(4).map(|kv| {
                            Label::new(if kv.unit.is_empty() {
                                format!("{}: {}", kv.label, kv.value)
                            } else {
                                format!("{}: {} {}", kv.label, kv.value, kv.unit)
                            })
                            .text_sm()
                            .text_color(theme.muted_foreground)
                        })),
                )
        }))
}

/// 渲染冻结数据标签页。
///
/// `history` 是通过 `AppBackend::load_freeze_history` 异步加载、合并了数据库
/// 历史并去重后的完整列表；在它加载完成之前（或加载失败）先用 `snapshot.freezes`
/// （仅内存环形缓冲、重启后可能不完整）兜底展示，避免切到这个 tab 时先空一下。
///
/// `history` 只在切入 tab 时查一次库，不会自动刷新；而 `snapshot.freezes` 会随每次
/// 后端推送实时更新。若只在 `history.is_none()` 时才 fallback 到 `snapshot.freezes`，
/// 一旦 DB 历史加载完成，tab 停留期间后端新推送的冻结数据就会被彻底忽略、界面不再更新。
/// 因此这里始终把两者按 `(trigger, snapshot_time_ms)` 去重合并（与后端
/// `MeterActor::load_freeze_history` 的去重口径一致），保证新推送的冻结能实时出现。
pub fn freezes(
    snapshot: &MeterSnapshot,
    history: Option<&[FreezeSnapshotSummary]>,
    loading: bool,
    theme: &Theme,
) -> impl IntoElement {
    let merged_storage;
    let items: &[FreezeSnapshotSummary] = match history {
        Some(hist) => {
            let mut seen: std::collections::HashSet<(String, i64)> = hist
                .iter()
                .map(|f| (f.trigger.clone(), f.snapshot_time_ms))
                .collect();
            let mut combined = hist.to_vec();
            for freeze in &snapshot.freezes {
                if seen.insert((freeze.trigger.clone(), freeze.snapshot_time_ms)) {
                    combined.push(freeze.clone());
                }
            }
            combined.sort_by_key(|f| std::cmp::Reverse(f.snapshot_time_ms));
            merged_storage = combined;
            &merged_storage
        }
        None => &snapshot.freezes,
    };
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(Label::new("冻结数据").text_2xl().font_semibold())
        .child(
            Label::new(if loading && history.is_none() {
                "正在从数据库加载历史冻结数据…".to_string()
            } else {
                format!("共 {} 条冻结快照，按冻结时间倒序显示", items.len())
            })
            .text_sm()
            .text_color(theme.muted_foreground),
        )
        .children(items.iter().map(|freeze| {
            let time = chrono::DateTime::from_timestamp_millis(freeze.snapshot_time_ms)
                .map(|v| {
                    v.with_timezone(&Utc)
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
                            Label::new(format!("最大需量 {:4} kW", freeze.max_demand_kw))
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