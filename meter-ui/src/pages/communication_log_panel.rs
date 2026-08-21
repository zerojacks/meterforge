use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    h_flex,
    label::Label,
    v_flex,
    ActiveTheme as _, Sizable as _, StyledExt as _, Theme,
};
use meter_core::communication_log::{flatten_value_tree, CommunicationLogEntry};
use std::sync::{Arc, Mutex};

/// 面板内保留的条数上限，与 meter-core 的 CommunicationLogService 保持一致。
const ENTRY_CAP: usize = 1_000;

#[derive(Clone, Default)]
pub struct CommunicationLogStore(pub Arc<Mutex<Vec<CommunicationLogEntry>>>);

/// 通信日志面板：每条记录 = 头部行（时间/方向/通道）+ 原始报文 + 内联的完整帧解析结果。
/// 参照 gpui 原生 list 的可变高度条目实现，长内容自动换行；默认自动滚动到底部。
pub struct CommunicationLogPanel {
    pub store: CommunicationLogStore,
    list_state: ListState,
    auto_scroll: bool,
    /// 是否展示内联的帧解析结果。关闭时每条记录只渲染原始报文，跳过
    /// `flatten_value_tree` 的构建与整棵解析树的渲染，条目变矮很多、也更省
    /// CPU——报文量大或者只想看原始收发数据时可以关掉，减少卡顿。
    show_parsed: bool,
}

impl CommunicationLogPanel {
    pub fn new(
        initial: Vec<CommunicationLogEntry>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = CommunicationLogStore(Arc::new(Mutex::new(initial)));
        let count = store.count();
        let list_state = ListState::new(count, ListAlignment::Top, px(120.));
        if count > 0 {
            list_state.scroll_to(ListOffset {
                item_ix: count,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
        Self {
            store,
            list_state,
            auto_scroll: true,
            show_parsed: true,
        }
    }

    pub fn append(&mut self, entry: CommunicationLogEntry, cx: &mut Context<Self>) {
        let count = {
            let Ok(mut entries) = self.store.0.lock() else {
                return;
            };
            if entries.len() == ENTRY_CAP {
                entries.remove(0);
            }
            entries.push(entry);
            entries.len()
        };
        self.refresh(count);
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        if let Ok(mut entries) = self.store.0.lock() {
            entries.clear();
        }
        self.list_state.reset(0);
        cx.notify();
    }

    /// 条目数变化后重建列表：自动滚动开启时贴底，否则保持原滚动位置。
    fn refresh(&mut self, count: usize) {
        if self.list_state.item_count() == count {
            return;
        }
        let saved_top = self.list_state.logical_scroll_top();
        self.list_state.reset(count);
        if self.auto_scroll {
            self.list_state.scroll_to(ListOffset {
                item_ix: count,
                offset_in_item: px(0.),
            });
        } else {
            self.list_state.scroll_to(ListOffset {
                item_ix: saved_top.item_ix.min(count.saturating_sub(1)),
                offset_in_item: saved_top.offset_in_item,
            });
        }
    }

    fn render_item(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let entry = match self.store.0.lock() {
            Ok(entries) => match entries.get(ix) {
                Some(entry) => entry.clone(),
                None => return div().into_any_element(),
            },
            Err(_) => return div().into_any_element(),
        };
        let time = chrono::DateTime::from_timestamp_millis(entry.timestamp_ms)
            .map(|v| {
                v.with_timezone(&chrono::Local)
                    .format("%H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_default();
        let hex = entry
            .data
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let is_tx = entry.direction == "TX";
        let (dir_bg, dir_label) = if is_tx {
            (theme.success, "TX")
        } else {
            (theme.primary, "RX")
        };

        let mut item = v_flex()
            .gap_1()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .hover(|s| s.bg(theme.muted.opacity(0.4)))
            // 头部行：时间 + 方向 + 通道 + 复制
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(theme.muted_foreground)
                            .child(time),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .text_xs()
                            .font_semibold()
                            .bg(dir_bg)
                            .text_color(theme.primary_foreground)
                            .child(dir_label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(entry.channel.clone()),
                    )
                    .child(div().flex_1())
                    .child(
                        Clipboard::new(("copy-comm-log", ix))
                            .value(hex.clone())
                            .tooltip("复制报文"),
                    ),
            )
            // 原始报文（自动换行）
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.foreground)
                    .child(hex),
            );
        if self.show_parsed {
            if let Some(tree) = &entry.parsed {
                item = item.child(v_flex().children(
                    flatten_value_tree(tree)
                        .into_iter()
                        .map(|node| render_flat_node(&node, &theme)),
                ));
            } else if !entry.data.is_empty() {
                item = item.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("（非完整 DL/T 645-2007 帧，未解析）"),
                );
            }
        }
        div()
            .id(("comm-log-item", ix))
            .w_full()
            .child(item)
            .into_any_element()
    }
}

/// 单个解析行：三列（字段名 / 原始字节 / 解析值），列内文本自动换行，整体行高自适应。
fn render_flat_node(node: &meter_core::communication_log::FlatNode, theme: &Theme) -> Div {
    let raw = node
        .raw
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    h_flex()
        .items_start()
        .gap_2()
        .py_0p5()
        .pl(px(node.depth as f32 * 14.0))
        .child(
            div()
                .w(px(170.))
                .min_w_0()
                .text_xs()
                .child(node.name.clone()),
        )
        .child(
            div()
                .w(px(120.))
                .min_w_0()
                .text_xs()
                .font_family("monospace")
                .text_color(theme.muted_foreground)
                .child(raw),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.primary)
                .child(node.value.clone()),
        )
}

impl CommunicationLogStore {
    fn count(&self) -> usize {
        self.0.lock().map(|entries| entries.len()).unwrap_or(0)
    }
}

impl Render for CommunicationLogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.store.count();
        self.refresh(count);

        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .flex_shrink(0.)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Label::new("通信日志").text_sm().font_semibold())
                    .child(
                        Label::new(format!("{count} 条"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("comm-log-toggle-parse")
                            .label(if self.show_parsed {
                                "解析:开"
                            } else {
                                "解析:关"
                            })
                            .small()
                            .ghost()
                            .tooltip("切换是否内联展示帧解析结果，关闭可减少卡顿")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_parsed = !this.show_parsed;
                                // 每条item高度都会因为解析树的有无而变化，
                                // remeasure 保留滚动位置的同时强制重新测量所有行高。
                                this.list_state.remeasure();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("comm-log-auto-scroll")
                            .label(if self.auto_scroll {
                                "自动滚动:开"
                            } else {
                                "自动滚动:关"
                            })
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.auto_scroll = !this.auto_scroll;
                                if this.auto_scroll {
                                    let count = this.store.count();
                                    this.refresh(count);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("comm-log-clear")
                            .label("清空")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.clear(cx);
                            })),
                    ),
            );
        if count == 0 {
            panel = panel.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("暂无通信日志"),
            );
        } else {
            panel = panel.child(
                div()
                    .id("comm-log-list")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        list(self.list_state.clone(), cx.processor(Self::render_item))
                            .with_sizing_behavior(ListSizingBehavior::Auto)
                            .size_full()
                            .into_any_element(),
                    ),
            );
        }
        panel
    }
}
