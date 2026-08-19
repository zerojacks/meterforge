//! 事件记录与冻结数据展示区域。
use crate::types::MeterSnapshot;
use chrono::Utc;
use gpui::*;
use gpui_component::{h_flex, label::Label, *};
use meter_core::snapshot::FreezeSnapshotSummary;

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
/// 统一展示模式：
/// - 优先显示快照中的实时数据（snapshot.load_records）
/// - 合并数据库历史数据（按需加载）
/// - 按 (class_id, sample_time_ms) 去重
pub fn load_profile(
    snapshot: &MeterSnapshot,
    history: Option<&[meter_core::snapshot::LoadRecordSnapshot]>,
    loading: bool,
    theme: &Theme,
) -> impl IntoElement {
    // 合并快照和历史数据并去重
    let all_records = if let Some(hist) = history {
        let mut seen: std::collections::HashSet<(u8, i64)> = snapshot.load_records
            .iter()
            .map(|r| (r.class_id, r.sample_time_ms))
            .collect();
        
        // 先收集快照数据
        let mut combined: Vec<&meter_core::snapshot::LoadRecordSnapshot> = 
            snapshot.load_records.iter().collect();
        
        // 添加历史数据中不重复的记录
        for record in hist {
            if seen.insert((record.class_id, record.sample_time_ms)) {
                combined.push(record);
            }
        }
        
        combined
    } else {
        // 只有快照数据
        snapshot.load_records.iter().collect()
    };
    
    let status_text = if loading && history.is_none() {
        "正在从数据库加载负荷记录…".to_string()
    } else if all_records.is_empty() {
        "暂无负荷记录采样".to_string()
    } else if history.is_some() {
        format!("共 {} 条记录，按采样时间倒序显示", all_records.len())
    } else {
        format!("最近 {} 条记录（实时快照），按采样时间倒序显示", all_records.len())
    };
    
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(Label::new("负荷记录").text_2xl().font_semibold())
        .child(
            Label::new(status_text)
                .text_sm()
                .text_color(theme.muted_foreground),
        )
        .children(all_records.iter().map(|record| {
            render_load_record(record, theme)
        }))
}

/// 渲染单条负荷记录
fn render_load_record(
    record: &meter_core::snapshot::LoadRecordSnapshot,
    theme: &Theme,
) -> impl IntoElement {
    let time = chrono::DateTime::from_timestamp_millis(record.sample_time_ms)
        .map(|v| {
            v.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    
    // 构建关键数值列表
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
                    Label::new(format!("{} · {}", record.class_label(), record.blocks_summary()))
                        .font_semibold()
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
                    Label::new(kv)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                })),
        )
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