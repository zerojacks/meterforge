use gpui::*;
use gpui_component::{
    clipboard::Clipboard,
    h_flex,
    label::Label,
    list::{List, ListDelegate, ListItem, ListState},
    ActiveTheme as _, StyledExt,
};
use meter_core::communication_log::CommunicationLogEntry;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct CommunicationLogStore(pub Arc<Mutex<Vec<CommunicationLogEntry>>>);

pub struct CommunicationLogDelegate {
    store: CommunicationLogStore,
    selected: Option<gpui_component::IndexPath>,
}
impl CommunicationLogDelegate {
    pub fn new(store: CommunicationLogStore) -> Self {
        Self {
            store,
            selected: None,
        }
    }
}

impl ListDelegate for CommunicationLogDelegate {
    type Item = ListItem;
    fn items_count(&self, _: usize, _: &App) -> usize {
        self.store.0.lock().map(|items| items.len()).unwrap_or(0)
    }
    fn render_item(
        &mut self,
        ix: gpui_component::IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let entry = self.store.0.lock().ok()?.get(ix.row).cloned()?;
        let time = chrono::DateTime::from_timestamp_millis(entry.timestamp_ms)
            .map(|v| {
                v.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_default();
        let bytes = entry
            .data
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let theme = cx.theme();
        Some(
            ListItem::new(ix).selected(self.selected == Some(ix)).child(
                // `ListItem` is not itself a flex row. Keep all fields in one
                // explicit row so virtualized list layout cannot stack them.
                h_flex()
                    .h(px(30.))
                    .w_full()
                    .px_3()
                    .gap_3()
                    .items_center()
                    .child(Label::new(time).w(px(178.)).text_xs())
                    .child(
                        Label::new(entry.direction)
                            .w(px(28.))
                            .text_xs()
                            .font_semibold()
                            .text_color(if entry.direction == "TX" {
                                theme.success
                            } else {
                                theme.primary
                            }),
                    )
                    .child(
                        Label::new(entry.channel)
                            .w(px(150.))
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Label::new(bytes)
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family("monospace")
                            .truncate(),
                    )
                    .child(
                        Clipboard::new(format!("copy-log-payload-{}", ix.row))
                            .value(
                                entry
                                    .data
                                    .iter()
                                    .map(|byte| format!("{byte:02X}"))
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            )
                            .tooltip("复制报文"),
                    ),
            ),
        )
    }
    fn set_selected_index(
        &mut self,
        ix: Option<gpui_component::IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
        cx.notify();
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .items_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("暂无通信日志")
    }
}

pub struct CommunicationLogPanel {
    pub store: CommunicationLogStore,
    list: Entity<ListState<CommunicationLogDelegate>>,
}
impl CommunicationLogPanel {
    pub fn new(
        initial: Vec<CommunicationLogEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = CommunicationLogStore(Arc::new(Mutex::new(initial)));
        let delegate = CommunicationLogDelegate::new(store.clone());
        let list = cx.new(|cx| ListState::new(delegate, window, cx));
        Self { store, list }
    }
    pub fn append(&mut self, entry: CommunicationLogEntry, cx: &mut Context<Self>) {
        if let Ok(mut entries) = self.store.0.lock() {
            if entries.len() == 1_000 {
                entries.remove(0);
            }
            entries.push(entry);
        }
        self.list.update(cx, |list, cx| {
            list.scroll_handle().scroll_to_bottom();
            cx.notify();
        });
        cx.notify();
    }
}
impl Render for CommunicationLogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(36.))
                    .px_3()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Label::new("通信日志").text_sm().font_semibold())
                    .child(
                        Label::new("最新记录显示在底部")
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .gap_3()
                    .items_center()
                    .bg(theme.muted.opacity(0.35))
                    .child(
                        Label::new("时间")
                            .w(px(178.))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Label::new("方向")
                            .w(px(28.))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Label::new("通道")
                            .w(px(150.))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Label::new("报文")
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(List::new(&self.list).flex_1().min_h_0())
    }
}
