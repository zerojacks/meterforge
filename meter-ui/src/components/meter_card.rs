// 左侧表列表中的紧凑表项。

use crate::types::MeterSnapshot;
use gpui::*;
use gpui_component::label::Label;
use gpui_component::*;

#[derive(IntoElement)]
pub struct MeterCard {
    snapshot: MeterSnapshot,
    selected: bool,
}

impl MeterCard {
    pub fn new(snapshot: MeterSnapshot) -> Self {
        Self {
            snapshot,
            selected: false,
        }
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for MeterCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let online = self.snapshot.is_online;
        let accent = if online {
            theme.success
        } else {
            theme.muted_foreground
        };
        let background = if self.selected {
            theme.primary.opacity(0.16)
        } else {
            theme.background
        };
        let border = if self.selected {
            theme.primary
        } else {
            theme.border.opacity(0.25)
        };

        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(background)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_start()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Label::new(self.snapshot.address.clone())
                                    .font_semibold()
                                    .text_sm(),
                            )
                            .child(
                                Label::new(if online {
                                    format!(
                                        "{:.1}V · {:.2}kW",
                                        self.snapshot.voltage_a, self.snapshot.active_power_kw
                                    )
                                } else {
                                    "离线".to_string()
                                })
                                .text_xs()
                                .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(div().mt_1().size(px(7.0)).rounded_full().bg(accent)),
            )
    }
}
