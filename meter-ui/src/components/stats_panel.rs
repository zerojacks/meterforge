// 统计面板

use crate::state::{GlobalMeterRegistry, MeterRegistry};
use gpui::*;
use gpui_component::label::Label;
use gpui_component::*;
use parking_lot::RwLock;
use std::sync::Arc;

#[derive(IntoElement)]
pub struct StatsPanel {}

impl StatsPanel {
    pub fn new() -> Self {
        Self {}
    }
}

impl RenderOnce for StatsPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let registry = &cx.global::<GlobalMeterRegistry>().0;
        let reg = registry.read();

        let total_count = reg.count();
        let online_count = total_count; // 简化：全部在线

        // 计算统计数据
        let mut total_energy = 0.0;
        let mut total_power = 0.0;
        let mut avg_voltage = 0.0;

        for addr in reg.all_addresses() {
            if let Some(entity) = reg.get(&addr) {
                let snapshot = entity.read(cx).snapshot.clone();
                total_energy += snapshot.energy_total_kwh;
                total_power += snapshot.active_power_kw as f64;
                avg_voltage += snapshot.voltage_a as f64;
            }
        }

        if total_count > 0 {
            avg_voltage /= total_count as f64;
        }

        div()
            .w_full()
            .p_4()
            .bg(theme.muted.opacity(0.3))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .grid()
                    .grid_cols(4)
                    .gap_4()
                    .child(self.stat_card(
                        "在线表数",
                        format!("{} / {}", online_count, total_count),
                        IconName::User,
                        theme.success,
                        theme,
                    ))
                    .child(self.stat_card(
                        "总电能",
                        format!("{:.1} MWh", total_energy / 1000.0),
                        IconName::Sun,
                        theme.primary,
                        theme,
                    ))
                    .child(self.stat_card(
                        "总功率",
                        format!("{:.1} kW", total_power),
                        IconName::Sun,
                        theme.info,
                        theme,
                    ))
                    .child(self.stat_card(
                        "平均电压",
                        format!("{:.1} V", avg_voltage),
                        IconName::User,
                        theme.warning,
                        theme,
                    )),
            )
    }
}

impl StatsPanel {
    fn stat_card(
        &self,
        label: &str,
        value: String,
        icon: IconName,
        color: Hsla,
        theme: &Theme,
    ) -> impl IntoElement {
        div()
            .p_3()
            .rounded_lg()
            .bg(theme.background) // 修复：使用theme.background代替theme.card
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .p_2()
                            .rounded_lg()
                            .bg(color.opacity(0.1))
                            .child(Icon::new(icon).size_5().text_color(color)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Label::new(label)
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(Label::new(value).text_xl().font_semibold()),
                    ),
            )
    }
}
