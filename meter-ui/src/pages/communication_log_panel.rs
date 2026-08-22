use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    h_flex,
    label::Label,
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _, Theme,
};
use meter_core::communication_log::{flatten_value_tree, CommunicationLogEntry, FlatNode};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// 面板内保留的条数上限，与 meter-core 的 CommunicationLogService 保持一致。
const ENTRY_CAP: usize = 1_000;

/// 日志批量写入 UI 的间隔：高频报文先入队，到点合并成一次列表更新，
/// 避免逐条驱动列表重排把 UI 拖卡。
const FLUSH_INTERVAL_MS: u64 = 250;

/// 解析表 raw 列完整展示的字节数，超出部分折叠为 "… +N B"
const RAW_INLINE_BYTES: usize = 8;
/// 解析表三列宽度：字段名（含缩进）/ 原始字节 / 解析值（flex 弹性）
const COL_NAME: Pixels = px(250.);
const COL_RAW: Pixels = px(180.);

/// 折叠态下单条日志的解析表最多渲染的行数（点 ▶ 展开全量）。
///
/// gpui 的 list 没有元素缓存：每次刷新都会重建可见条目的整棵元素树，
/// 解析表（动辄几十行 × 3 列文本）是成本大头。默认折叠让流式日志下的
/// 稳态渲染量降一个数量级，需要看完整解析时再展开单条。
const MAX_COLLAPSED_PARSE_ROWS: usize = 12;

/// 一条日志的渲染模型：写入时一次性构建（时间/hex 字符串、解析展平行），
/// render_item 每帧只消费不再重算——flatten/join/格式化只发生一次。
struct LogRecord {
    /// 唯一序号：作为展开状态的标识，不受滑动窗口挤旧导致的索引移位影响
    seq: u64,
    time: String,
    hex: String,
    is_tx: bool,
    channel: String,
    byte_count: usize,
    /// 非完整 645 帧（无法解析）
    unparsed: bool,
    /// 解析展平行（未解析时为空）
    nodes: Vec<FlatNode>,
}

impl LogRecord {
    fn build(seq: u64, entry: &CommunicationLogEntry) -> Self {
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
        Self {
            seq,
            time,
            hex,
            is_tx: entry.direction == "TX",
            channel: entry.channel.clone(),
            byte_count: entry.data.len(),
            unparsed: entry.parsed.is_none() && !entry.data.is_empty(),
            nodes: entry.parsed.as_ref().map(flatten_value_tree).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Default)]
pub struct CommunicationLogStore(pub Arc<Mutex<Vec<LogRecord>>>);

/// 通信日志面板：每条记录 = 头部行（时间/方向/通道）+ 原始报文 + 内联的
/// 帧解析结果（默认折叠，可展开）。可变高度虚拟列表，默认自动滚动到底部。
pub struct CommunicationLogPanel {
    pub store: CommunicationLogStore,
    list_state: ListState,
    auto_scroll: bool,
    /// 是否展示内联的帧解析结果。关闭时每条记录只渲染原始报文，条目
    /// 最矮也最省 CPU——报文量大或者只想看原始收发数据时可以关掉。
    show_parsed: bool,
    /// 待写入 store 的日志缓冲。由 [`Self::append`] 入队、[`Self::flush`]
    /// 定时合并写入——日志到达速率可能是每秒几十条，逐条驱动列表更新
    /// （哪怕走增量 splice）也会让 UI 忙于重排。
    pending: Vec<CommunicationLogEntry>,
    /// store 是否发生过"达到上限挤掉最旧条目"。此后条目索引持续整体
    /// 移位，splice 无法表达这种复用（会拿错缓存高度），只能走整体 reset。
    evicted: bool,
    /// 下一条日志的序号
    next_seq: u64,
    /// 展开全量解析的条目（按 seq 标识）
    expanded: HashSet<u64>,
}

impl CommunicationLogPanel {
    pub fn new(
        initial: Vec<CommunicationLogEntry>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let records: Vec<LogRecord> = initial
            .iter()
            .enumerate()
            .map(|(ix, entry)| LogRecord::build(ix as u64, entry))
            .collect();
        let next_seq = records.len() as u64;
        let store = CommunicationLogStore(Arc::new(Mutex::new(records)));
        let count = store.count();
        let list_state = ListState::new(count, ListAlignment::Top, px(64.));
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
            pending: Vec::new(),
            evicted: false,
            next_seq,
            expanded: HashSet::new(),
        }
    }

    /// 入队一条日志。真正的列表更新由 [`Self::flush`] 延时批量执行：
    /// 队列从空到非空时安排一个定时器，期间到达的日志一起合并。
    pub fn append(&mut self, entry: CommunicationLogEntry, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(FLUSH_INTERVAL_MS))
                    .await;
                let _ = this.update(cx, |panel, cx| panel.flush(cx));
            })
            .detach();
        }
        self.pending.push(entry);
    }

    /// 把缓冲中的日志一次性写入 store 并增量更新列表。
    fn flush(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            return;
        }
        let old_count = self.list_state.item_count();
        let count = {
            let Ok(mut records) = self.store.0.lock() else {
                self.pending.clear();
                return;
            };
            for entry in self.pending.drain(..) {
                if records.len() == ENTRY_CAP {
                    records.remove(0);
                    self.evicted = true;
                }
                let seq = self.next_seq;
                self.next_seq += 1;
                records.push(LogRecord::build(seq, &entry));
            }
            records.len()
        };
        if self.evicted {
            self.rebuild_list(count);
        } else {
            // 尾部 splice：已有条目的高度缓存全部保留，只测量新增条目
            let added = count - old_count;
            if added > 0 {
                self.list_state.splice(old_count..old_count, added);
            }
            if self.auto_scroll {
                self.list_state.scroll_to(ListOffset {
                    item_ix: count,
                    offset_in_item: px(0.),
                });
            }
        }
        // 自动滚动关闭时列表视口是静止的：新日志已 splice 进列表，
        // 等用户滚动/交互时自然出现，这里不触发重绘——否则 gpui 的
        // list 每次刷新都会重建可见条目的整棵元素树（展开态一条就
        // 有几百个元素），流式日志下会持续卡顿。
        if self.auto_scroll || old_count == 0 {
            cx.notify();
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending.clear();
        self.evicted = false;
        self.expanded.clear();
        if let Ok(mut records) = self.store.0.lock() {
            records.clear();
        }
        self.list_state.reset(0);
        cx.notify();
    }

    /// 整体重建列表（滑动窗口挤旧/兜底路径）：自动滚动开启时贴底，
    /// 否则尽量保持原滚动位置。
    fn rebuild_list(&mut self, count: usize) {
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

    /// 条目数与列表状态不一致时整体重建（外部修改 store 的兜底）。
    fn refresh(&mut self, count: usize) {
        if self.list_state.item_count() == count {
            return;
        }
        self.rebuild_list(count);
    }

    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        // 持锁期间完成全部元素构建（元素均为 owned）。LogRecord 里的
        // 字符串/展平行在写入时已构建好，这里只做轻量 clone 与 div 组装。
        let Ok(records) = self.store.0.lock() else {
            return div().into_any_element();
        };
        let Some(record) = records.get(ix) else {
            return div().into_any_element();
        };
        let (dir_bg, dir_label) = if record.is_tx {
            (theme.success, "TX")
        } else {
            (theme.primary, "RX")
        };
        let has_parse = self.show_parsed && !record.nodes.is_empty();
        let expanded = has_parse && self.expanded.contains(&record.seq);

        let mut item = v_flex()
            .gap_1()
            .w_full()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .hover(|s| s.bg(theme.muted.opacity(0.4)))
            // 头部行：时间 + 方向 + 通道 + 字节数 + 展开解析 + 复制
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(theme.muted_foreground)
                            .child(record.time.clone()),
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
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(record.channel.clone()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} B", record.byte_count)),
                    )
                    .children(has_parse.then(|| {
                        let seq = record.seq;
                        let item_ix = ix;
                        Button::new(("comm-log-expand", ix))
                            .label(if expanded { "▼" } else { "▶" })
                            .small()
                            .ghost()
                            .tooltip(if expanded {
                                "收起解析（自动滚动保持关闭）"
                            } else {
                                "展开全部解析行（将暂停自动滚动）"
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let now_expanded = this.expanded.insert(seq);
                                if !now_expanded {
                                    this.expanded.remove(&seq);
                                    // 收起只影响本条高度：精确重测这一条，
                                    // 其余条目的高度缓存全部保留
                                    this.list_state.remeasure_items(item_ix..item_ix + 1);
                                } else {
                                    // 展开一条通常意味着要细看：暂停自动滚动
                                    // （否则新日志把它推离视口、且流式刷新会
                                    // 反复重建这几百个元素），并把该条滚入视口
                                    this.auto_scroll = false;
                                    this.list_state.remeasure_items(item_ix..item_ix + 1);
                                    this.list_state.scroll_to_reveal_item(item_ix);
                                }
                                cx.notify();
                            }))
                    }))
                    .child(
                        Clipboard::new(("copy-comm-log", ix))
                            .value(record.hex.clone())
                            .tooltip("复制报文"),
                    ),
            )
            // 原始报文：空格分隔的等宽文本，按面板宽度自然断行且断点
            // 落在字节之间——短帧一行放下，长帧也不产生多余行
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.foreground)
                    .child(record.hex.clone()),
            );
        if self.show_parsed {
            if !record.nodes.is_empty() {
                let limit = if expanded {
                    record.nodes.len()
                } else {
                    MAX_COLLAPSED_PARSE_ROWS.min(record.nodes.len())
                };
                let omitted = record.nodes.len() - limit;
                let mut card = v_flex()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.muted.opacity(0.35))
                    .border_1()
                    .border_color(theme.border)
                    .child(render_parse_header(&theme))
                    .children(
                        record.nodes
                            .iter()
                            .take(limit)
                            .map(|node| render_flat_node(node, &theme)),
                    );
                if omitted > 0 {
                    card = card.child(
                        div()
                            .py_0p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "… 已折叠 {omitted} 行，点击右上 ▶ 展开全部"
                            )),
                    );
                }
                item = item.child(card);
            } else if record.unparsed {
                item = item.child(
                    div()
                        .mt_1()
                        .text_xs()
                        .italic()
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

/// 解析结果表头：三列与 [`render_flat_node`] 一一对应
fn render_parse_header(theme: &Theme) -> Div {
    h_flex()
        .gap_2()
        .pb_1()
        .mb_0p5()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .w(COL_NAME)
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("字段"),
        )
        .child(
            div()
                .w(COL_RAW)
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("原始字节"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("解析值"),
        )
}

/// 单个解析行：三列（字段名 / 原始字节 / 解析值）。
///
/// 名称与 raw 列固定宽度 + 单行省略（truncate），杜绝长字段名/长数据块
/// 折行导致的整行错位；完整字节始终在上方 hex 文本里可查。解析值列
/// 弹性伸缩、允许列内换行（长费率表/数据块值不丢信息）。顶层容器行
/// （无值、展开子字段）加粗并加大上边距，形成"链路层/应用层"分组感。
fn render_flat_node(node: &FlatNode, theme: &Theme) -> Div {
    let is_group = node.value.is_empty();
    h_flex()
        .items_start()
        .gap_2()
        .py_0p5()
        .min_w_0()
        .when(is_group, |s| s.mt_1())
        .child(
            div()
                .w(COL_NAME)
                .min_w_0()
                .pl(px(node.depth as f32 * 12.0))
                .text_xs()
                .truncate()
                .when(is_group, |s| s.font_semibold())
                .when(node.depth > 0, |s| s.text_color(theme.muted_foreground))
                .child(node.name.clone()),
        )
        .child(
            div()
                .w(COL_RAW)
                .min_w_0()
                .text_xs()
                .font_family("monospace")
                .text_color(theme.muted_foreground)
                .truncate()
                .child(raw_column_text(&node.raw)),
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

/// raw 列文本：短字节串完整显示（≤8 字节约 165px，列宽 180px 内放得下）；
/// 超出显示前 5 个字节并标注折叠掉的字节数（"AA BB … +12 B"），
/// 折叠后总宽仍在列宽内，避免省略号本身再被截断。
fn raw_column_text(raw: &[u8]) -> String {
    const COLLAPSE_FROM: usize = RAW_INLINE_BYTES + 1;
    if raw.len() < COLLAPSE_FROM {
        raw.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let shown = RAW_INLINE_BYTES - 3;
        let head: Vec<_> = raw
            .iter()
            .take(shown)
            .map(|b| format!("{b:02X}"))
            .collect();
        format!("{} … +{} B", head.join(" "), raw.len() - shown)
    }
}

impl CommunicationLogStore {
    fn count(&self) -> usize {
        self.0.lock().map(|records| records.len()).unwrap_or(0)
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
                            .tooltip("切换是否展示帧解析结果（默认折叠前12行，点条目 ▶ 展开）")
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
                                    this.list_state.scroll_to(ListOffset {
                                        item_ix: count,
                                        offset_in_item: px(0.),
                                    });
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
