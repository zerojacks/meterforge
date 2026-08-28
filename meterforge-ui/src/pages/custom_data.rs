use super::meter_detail::MeterDetailView;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::{
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::DialogButtonProps,
    label::Label,
    *,
    WindowExt,
};

impl MeterDetailView {
    fn show_clear_custom_data_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("清空自定义数据项？")
                .description("当前电表的全部自定义数据项将被删除，此操作不可撤销。")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("清空全部")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("取消")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| {
                            view.clear_custom_data_items(cx);
                        });
                    }
                    true
                })
        });
    }

    pub(super) fn render_custom_data_tab_extracted(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let mode = self
            .custom_data_info
            .as_ref()
            .map(|info| info.mode)
            .unwrap_or(2);
        let count = self.custom_data_items.len();

        let mode_button = |label: &'static str, value: u8, current: u8| {
            Button::new(format!("custom-data-mode-{value}"))
                .label(label)
                .small()
                .selected(current == value)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.apply_custom_data_mode(value, cx);
                }))
        };

        let mode_hint = match mode {
            0 => "命中自定义数据项直接回复；未命中时回退到模拟数据。",
            1 => "命中自定义数据项直接回复；未命中时应答\"无数据\"错误，不回退模拟数据。",
            _ => "不查自定义数据项，全部使用模拟数据（默认，兼容原有行为）。",
        };

        let add_row = h_flex()
            .gap_2()
            .items_end()
            .child(
                v_flex()
                    .gap_1()
                    .w(px(220.))
                    .child(Label::new("DI（8位HEX）").text_xs().text_color(theme.muted_foreground))
                    .child(Input::new(&self.custom_data_di_input)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .flex_1()
                    .child(
                        Label::new("应答内容（HEX，可为空）")
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(Input::new(&self.custom_data_value_input)),
            )
            .child(
                Button::new("add-custom-data-item")
                    .label("新增/覆盖")
                    .small()
                    .primary()
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.add_custom_data_item(window, cx);
                    })),
            );

        let list = if count == 0 {
            div()
                .p_4()
                .rounded_lg()
                .bg(theme.muted)
                .child(
                    Label::new(if self.custom_data_loading {
                        "加载中…"
                    } else {
                        "暂无自定义数据项"
                    })
                    .text_sm()
                    .text_color(theme.muted_foreground),
                )
                .into_any_element()
        } else {
            list(
                self.custom_data_list_state.clone(),
                cx.processor(Self::render_custom_data_item),
            )
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        v_flex()
            .w_full()
            .size_full()
            .gap_5()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(Label::new("自定义数据项").text_2xl().font_semibold())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("sync-custom-data-dialog")
                                    .label("一键同步到所有表")
                                    .small()
                                    .on_click(cx.listener(Self::show_sync_custom_data_dialog)),
                            )
                            .child(
                                Button::new("refresh-custom-data")
                                    .label("刷新")
                                    .small()
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.load_custom_data_items(cx);
                                    })),
                            )
                            .child(
                                Button::new("clear-custom-data")
                                    .label("清空全部")
                                    .small()
                                    .danger()
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.show_clear_custom_data_dialog(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                Label::new(
                    "收到读命令时按 DI 精确匹配（不做协议转换）：命中则将下面配置的内容整体逆序后回复（与645协议低字节在前的传输顺序一致，按人类正常顺序输入即可）。",
                )
                .text_sm()
                .text_color(theme.muted_foreground),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(mode_button("优先使用自定义数据项", 0, mode))
                    .child(mode_button("完全使用自定义数据项", 1, mode))
                    .child(mode_button("使用模拟数据", 2, mode)),
            )
            .child(Label::new(mode_hint).text_xs().text_color(theme.muted_foreground))
            .child(add_row)
            .child(list)
            .into_any_element()
    }

    fn render_custom_data_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(entry) = self.custom_data_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let di_display = entry.di.clone();
        let di_bytes = super::meter_detail::parse_di_hex(&entry.di).unwrap_or([0; 4]);

        h_flex()
            .id(("custom-data-item", ix))
            .w_full()
            .justify_between()
            .items_center()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(format!("DI {di_display}")).font_semibold())
                    .child(
                        Label::new(if entry.data.is_empty() {
                            "（空应答内容）".to_string()
                        } else {
                            entry.data
                        })
                        .text_sm()
                        .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                Button::new(format!("remove-custom-data-{di_display}"))
                    .label("删除")
                    .small()
                    .danger()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.remove_custom_data_item(di_bytes, cx);
                    })),
            )
            .into_any_element()
    }
}
