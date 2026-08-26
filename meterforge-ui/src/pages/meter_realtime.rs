//! 实时数据展示区域，仅消费快照，不包含命令或窗口状态。
use crate::state::RealtimeSample;
use crate::types::MeterSnapshot;
use gpui::*;
use gpui_component::chart::AreaChart;
use gpui_component::{label::Label, *};
use std::collections::VecDeque;

fn metric(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .min_h(px(92.))
        .p_4()
        .rounded_lg()
        .bg(theme.muted.opacity(0.35))
        .child(
            v_flex()
                .gap_2()
                .child(
                    Label::new(label)
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
                .child(Label::new(value).text_xl().font_semibold()),
        )
}

fn group(
    title: impl Into<SharedString>,
    items: Vec<(String, String, Hsla)>,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(Label::new(title).text_lg().font_semibold())
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .children(items.into_iter().map(|(label, value, color)| {
                    v_flex()
                        .gap_1()
                        .min_w(px(150.))
                        .child(
                            Label::new(label)
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            Label::new(value)
                                .text_xl()
                                .font_semibold()
                                .text_color(color),
                        )
                })),
        )
}

/// 时间戳 → "hh:mm:ss" 轴标签
fn time_label(time_ms: i64) -> String {
    let secs = time_ms.div_euclid(1000);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

/// 一条相位曲线定义
struct PhaseSeries {
    id: &'static str,
    name: &'static str,
    color: Hsla,
    pick: fn(&RealtimeSample) -> f64,
}

/// 单指标折线图卡片：用 AreaChart + 透明填充实现"折线"外观，
/// 多相曲线真正共享同一个 Y 轴 domain（AreaChart 内部按所有序列的合集算 Y scale）
fn chart_card(
    title: impl Into<SharedString>,
    samples: Vec<RealtimeSample>,
    series: Vec<PhaseSeries>,
    theme: &Theme,
) -> impl IntoElement {
    // 动态计算刻度间隔，使 x 轴上大约显示 6 个刻度（样本少时显示全部）
    let tick_margin = if samples.len() <= 6 {
        1
    } else {
        (samples.len() + 5) / 6
    };

    let mut chart = AreaChart::new(samples)
        .x(|d: &RealtimeSample| time_label(d.time_ms))
        .tick_margin(tick_margin);

    // 启用交互提示需要为图表设置唯一 id，否则图表保持非交互状态。
    // 使用第一条序列的 id 作为图表 id（调用方保证每个图表序列 id 在页面内唯一）。
    if !series.is_empty() {
        chart = chart.id(series[0].id);
    }

    for s in &series {
        chart = chart
            .y(s.pick)
            .stroke(s.color)
            .fill(s.color.opacity(0.)) // 完全透明，视觉上退化成折线
            .name(s.name);
    }

    v_flex()
        .h(px(240.))
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .p_4()
        .gap_2()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(Label::new(title).text_sm().font_semibold())
                .child(h_flex().gap_3().children(series.iter().map(|s| {
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().size(px(8.)).rounded_full().bg(s.color))
                        .child(
                            Label::new(s.name)
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                }))),
        )
        .child(div().flex_1().min_h_0().child(chart))
}

pub fn render(
    snapshot: &MeterSnapshot,
    history: &VecDeque<RealtimeSample>,
    theme: &Theme,
) -> impl IntoElement {
    let samples: Vec<RealtimeSample> = history.iter().copied().collect();
    div()
        .flex()
        .flex_col()
        .w_full()
        .items_stretch()
        .gap_5()
        .child(Label::new("实时数据").text_2xl().font_semibold())
        .child(
            Label::new("显示当前三相电压、电流、功率与电能数据，曲线保留最近 120 个采样点")
                .text_sm()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .grid()
                .grid_cols(4)
                .gap_3()
                .child(metric(
                    "A相电压",
                    format!("{:.1} V", snapshot.voltage_a),
                    theme,
                ))
                .child(metric(
                    "A相电流",
                    format!("{:.3} A", snapshot.current_a),
                    theme,
                ))
                .child(metric(
                    "有功功率",
                    format!("{:.4} kW", snapshot.active_power_kw),
                    theme,
                ))
                .child(metric(
                    "总电能",
                    format!("{:.2} kWh", snapshot.energy_total_kwh),
                    theme,
                )),
        )
        // ── 实时曲线 ──
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap_3()
                .child(chart_card(
                    "电压曲线 (V)",
                    samples.clone(),
                    vec![
                        PhaseSeries {
                            id: "chart-v-a",
                            name: "A相",
                            color: theme.colors.blue,
                            pick: |s| s.voltage_a as f64,
                        },
                        PhaseSeries {
                            id: "chart-v-b",
                            name: "B相",
                            color: theme.colors.green,
                            pick: |s| s.voltage_b as f64,
                        },
                        PhaseSeries {
                            id: "chart-v-c",
                            name: "C相",
                            color: theme.foreground,
                            pick: |s| s.voltage_c as f64,
                        },
                    ],
                    theme,
                ))
                .child(chart_card(
                    "电流曲线 (A)",
                    samples.clone(),
                    vec![
                        PhaseSeries {
                            id: "chart-i-a",
                            name: "A相",
                            color: theme.colors.blue,
                            pick: |s| s.current_a as f64,
                        },
                        PhaseSeries {
                            id: "chart-i-b",
                            name: "B相",
                            color: theme.colors.green,
                            pick: |s| s.current_b as f64,
                        },
                        PhaseSeries {
                            id: "chart-i-c",
                            name: "C相",
                            color: theme.foreground,
                            pick: |s| s.current_c as f64,
                        },
                    ],
                    theme,
                ))
                .child(chart_card(
                    "有功功率曲线 (kW)",
                    samples.clone(),
                    vec![PhaseSeries {
                        id: "chart-p-active",
                        name: "有功功率",
                        color: theme.colors.red,
                        pick: |s| s.active_power_kw as f64,
                    }],
                    theme,
                ))
                .child(chart_card(
                    "无功功率曲线 (kvar)",
                    samples.clone(),
                    vec![PhaseSeries {
                        id: "chart-p-reactive",
                        name: "无功功率",
                        color: theme.colors.blue,
                        pick: |s| s.reactive_power_kvar as f64,
                    }],
                    theme,
                )),
        )
        .child(group(
            "三相电压",
            vec![
                (
                    "A相".into(),
                    format!("{:.1} V", snapshot.voltage_a),
                    theme.colors.blue,
                ),
                (
                    "B相".into(),
                    format!("{:.1} V", snapshot.voltage_b),
                    theme.colors.green,
                ),
                (
                    "C相".into(),
                    format!("{:.1} V", snapshot.voltage_c),
                    theme.foreground,
                ),
            ],
            theme,
        ))
        .child(group(
            "三相电流",
            vec![
                (
                    "A相".into(),
                    format!("{:.3} A", snapshot.current_a),
                    theme.colors.blue,
                ),
                (
                    "B相".into(),
                    format!("{:.3} A", snapshot.current_b),
                    theme.colors.green,
                ),
                (
                    "C相".into(),
                    format!("{:.3} A", snapshot.current_c),
                    theme.foreground,
                ),
            ],
            theme,
        ))
        .child(group(
            "功率",
            vec![
                (
                    "有功功率".into(),
                    format!("{:.4} kW", snapshot.active_power_kw),
                    theme.colors.red,
                ),
                (
                    "无功功率".into(),
                    format!("{:.2} kvar", snapshot.reactive_power_kvar),
                    theme.colors.blue,
                ),
                (
                    "功率因数".into(),
                    format!("{:.3}", snapshot.power_factor),
                    theme.foreground,
                ),
            ],
            theme,
        ))
        .child(group(
            "电能",
            vec![
                (
                    "总电能".into(),
                    format!("{:.2} kWh", snapshot.energy_total_kwh),
                    theme.foreground,
                ),
                (
                    "最大需量".into(),
                    format!("{:.4} kW", snapshot.max_demand_kw),
                    theme.foreground,
                ),
            ],
            theme,
        ))
}
