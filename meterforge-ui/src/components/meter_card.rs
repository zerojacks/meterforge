// 左侧表列表中的紧凑表项。

use crate::types::MeterSnapshot;
use gpui::*;
use gpui_component::label::Label;
use gpui_component::*;

#[derive(IntoElement)]
pub struct MeterCard {
    snapshot: MeterSnapshot,
    selected: bool,
    /// 卡片左侧（地址前）的附加内容，比如批量删除用的勾选框。
    leading: Option<AnyElement>,
    /// 卡片右侧（状态点旁）的附加内容，比如列表项上的删除按钮。
    trailing: Option<AnyElement>,
}

impl MeterCard {
    pub fn new(snapshot: MeterSnapshot) -> Self {
        Self {
            snapshot,
            selected: false,
            leading: None,
            trailing: None,
        }
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }
    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_any_element());
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
                        h_flex().gap_2().items_start().children(self.leading).child(
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
                        ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().mt_1().size(px(7.0)).rounded_full().bg(accent))
                            .children(self.trailing),
                    ),
            )
    }
}
