// 监控工作台：左侧连接/表列表，右侧当前电表详情。

use super::MeterDetailView;
use crate::components::MeterCard;
use crate::state::{GlobalMeterRegistry, MeterState};
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;

pub struct MeterListView {
    all_addresses: Vec<String>,
    selected_address: Option<String>,
    subscriptions: Vec<Subscription>,
    detail_view: Option<Entity<MeterDetailView>>,
    address_search: Entity<InputState>,
}

impl MeterListView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let all_addresses = cx.global::<GlobalMeterRegistry>().0.read().all_addresses();
        let selected_address = all_addresses.first().cloned();
        let address_search = cx.new(|cx| InputState::new(_window, cx).placeholder("搜索电表地址"));
        let mut view = Self {
            all_addresses,
            selected_address,
            subscriptions: Vec::new(),
            detail_view: None,
            address_search: address_search.clone(),
        };
        view.subscriptions
            .push(cx.subscribe(&address_search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
        view.subscribe_to_meters(cx);
        view
    }

    fn subscribe_to_meters(&mut self, cx: &mut Context<Self>) {
        let entities: Vec<Entity<MeterState>> = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            self.all_addresses
                .iter()
                .filter_map(|address| registry.get(address).cloned())
                .collect()
        };
        for entity in entities {
            self.subscriptions
                .push(cx.observe(&entity, |_this, _entity, cx| cx.notify()));
        }
    }

    fn filtered_addresses(&self, cx: &App) -> Vec<String> {
        let query = self.address_search.read(cx).value().trim().to_owned();
        self.all_addresses
            .iter()
            .filter(|address| query.is_empty() || address.contains(query.as_str()))
            .cloned()
            .collect()
    }

    fn select_meter(&mut self, address: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_address = Some(address);
        cx.notify();
    }

    fn selected_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MeterDetailView>> {
        let address = self.selected_address.clone()?;
        let needs_new = self
            .detail_view
            .as_ref()
            .map(|view| view.read(cx).address() != address)
            .unwrap_or(true);
        if needs_new {
            self.detail_view = Some(cx.new(|cx| MeterDetailView::new(address, window, cx)));
        }
        self.detail_view.clone()
    }
}

impl Render for MeterListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let filtered_addresses = self.filtered_addresses(cx);
        let cards: Vec<_> = {
            let registry = cx.global::<GlobalMeterRegistry>().0.read();
            filtered_addresses
                .iter()
                .filter_map(|address| {
                    let snapshot = registry.get(address)?.read(cx).snapshot.clone();
                    let selected = self.selected_address.as_ref() == Some(address);
                    Some(
                        div()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let address = address.clone();
                                    move |view, _, window, cx| {
                                        view.select_meter(address.clone(), window, cx)
                                    }
                                }),
                            )
                            .child(MeterCard::new(snapshot).selected(selected)),
                    )
                })
                .collect()
        };
        let detail = self.selected_detail(window, cx);

        div().size_full().flex().flex_col().child(
            div().flex_1().min_h_0().child(
                h_resizable("meter-workbench")
                    .child(
                        resizable_panel()
                            .size(px(304.0))
                            .size_range(px(240.0)..px(460.0))
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .border_r_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .px_3()
                                            .py_2()
                                            .border_b_1()
                                            .border_color(theme.border)
                                            .child(Input::new(&self.address_search).small()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scrollbar()
                                            .px_2()
                                            .py_3()
                                            .child(if cards.is_empty() {
                                                v_flex()
                                                    .items_center()
                                                    .py_8()
                                                    .child(
                                                        Label::new("未找到匹配的电表地址")
                                                            .text_sm()
                                                            .text_color(theme.muted_foreground),
                                                    )
                                                    .into_any_element()
                                            } else {
                                                v_flex().gap_2().children(cards).into_any_element()
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        resizable_panel().size_range(px(520.0)..Pixels::MAX).child(
                            div().size_full().min_w_0().child(
                                detail
                                    .map(|view| view.into_any_element())
                                    .unwrap_or_else(|| {
                                        div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                Label::new("选择一块电表以查看详情")
                                                    .text_color(theme.muted_foreground),
                                            )
                                            .into_any_element()
                                    }),
                            ),
                        ),
                    ),
            ),
        )
    }
}
