use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    h_flex,
    label::Label,
    list::ListItem,
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme as _, Icon, IconName, Root, Sizable as _, StyledExt as _, Theme, TitleBar,
};
use meter_core::communication_log::{flatten_value_tree, CommunicationLogEntry, FlatNode};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// 面板内保留的条数上限，与 meter-core 的按地址分桶环形缓冲上限保持一致
/// （见 `communication_log::PER_METER_CAP`）。
const ENTRY_CAP: usize = 300;

/// 日志批量写入 UI 的间隔：报文先入队，到点合并成一次列表更新，
/// 避免逐条驱动列表重排把 UI 拖卡。
const FLUSH_INTERVAL_MS: u64 = 250;

/// 列表侧单行日志的固定高度：时间/方向/通道/解析摘要/原始报文（单行
/// 截断）/字节数都在这一行内，不随解析结果的深浅变化——这是相对旧版
/// "行内展开解析表"布局的关键改动：旧版每条记录的高度取决于其解析树
/// 是否展开，gpui 的虚拟列表每次刷新都要重新测量/重建可见范围内的
/// 元素，解析树一深（尤其数据块/负荷记录这类批量应答）行高开销就上去
/// 了，滚动即卡顿。固定行高之后，列表滚动只需要处理定长行，和日志
/// 内容复杂度完全解耦；完整解析在独立窗口中查看。
const ROW_HEIGHT: Pixels = px(28.);

/// 报文详情独立窗口尺寸
const DETAIL_WINDOW_SIZE: Size<Pixels> = size(px(960.), px(660.));
const DETAIL_WINDOW_MIN: Size<Pixels> = size(px(640.), px(420.));

/// 解析树列宽（参照 protocol-viewer）：三列宽度按窗口宽度的固定百分比
/// 给出默认值；字段列/数据列的分隔条可拖拽调宽（拖拽结果换算成比例存
/// 回 [`LogDetailView::col_ratios`]），说明列固定吃剩余空间，不参与
/// 拖拽，数组第三项仅作默认占比参考。所有列超出都显示省略号。
/// 列宽百分比：[字段, 数据, 说明]
const COL_WIDTH_PERCENT: [f32; 3] = [0.30, 0.25, 0.45]; // 30%, 25%, 45%
/// 列宽下限（像素）
const MIN_COL_W: f32 = 80.0;
/// 分隔条自身宽度（像素）
const RESIZE_HANDLE_W: f32 = 4.0;

// ── 树线常量（参照 protocol-viewer）──────────────────
/// 每级深度的槽宽
const TREE_SLOT_W: f32 = 24.0;
/// 线宽
const TREE_LINE_W: f32 = 1.0;
/// 行高
const TREE_ROW_H: f32 = 22.0;
/// 垂直连接线重叠像素
const LINE_OVERLAP: f32 = 2.0;

/// 一条日志的渲染模型：写入时一次性构建（时间/hex 字符串、解析摘要、
/// 解析展平行），render_item 每帧只消费不再重算。
struct LogRecord {
    /// 唯一序号：作为选中状态的标识，不受滑动窗口挤旧导致的索引移位影响
    seq: u64,
    time: String,
    hex: String,
    is_tx: bool,
    channel: String,
    byte_count: usize,
    /// 列表行摘要："读数据 · (当前)组合有功总电能" 这类一眼可读的信息
    summary: String,
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
        let nodes = entry
            .parsed
            .as_ref()
            .map(flatten_value_tree)
            .unwrap_or_default();
        Self {
            seq,
            time,
            hex,
            is_tx: entry.direction == "TX",
            channel: entry.channel.clone(),
            byte_count: entry.data.len(),
            // 摘要由 meter-core 在解析时从结构化 Message 提取（功能码 +
            // DI 名称），这里只透传
            summary: entry.summary.clone(),
            unparsed: entry.parsed.is_none() && !entry.data.is_empty(),
            nodes,
        }
    }
}

/// 从解析展平行中提取列表行摘要的逻辑已上移到 meter-core：
/// 摘要在解析时由结构化的 `Message`（`Message::summary`）生成并随
/// `CommunicationLogEntry.summary` 下发，UI 不再对展平树做字符串匹配。

#[derive(Clone, Default)]
pub struct CommunicationLogStore(Arc<Mutex<Vec<Arc<LogRecord>>>>);

impl CommunicationLogStore {
    fn records(&self) -> std::sync::MutexGuard<'_, Vec<Arc<LogRecord>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 通信日志面板：底部全宽的固定行高报文列表（虚拟滚动），每行带解析
/// 摘要（功能 + 数据项）；点击某行在**独立窗口**中打开该帧的完整解析
/// （完整 hex + 全量解析表），之后再点击其他条目时复用同一窗口切换内容。
pub struct CommunicationLogPanel {
    pub store: CommunicationLogStore,
    list_state: ListState,
    auto_scroll: bool,
    /// 待写入 store 的日志缓冲。由 [`Self::append`] 入队、[`Self::flush`]
    /// 定时合并写入——日志到达速率可能是每秒几十条，逐条驱动列表更新
    /// （哪怕走增量 splice）也会让 UI 忙于重排。
    pending: Vec<CommunicationLogEntry>,
    /// store 是否发生过"达到上限挤掉最旧条目"。此后条目索引持续整体
    /// 移位，splice 无法表达这种复用（会拿错缓存高度），只能走整体 reset。
    evicted: bool,
    /// 下一条日志的序号
    next_seq: u64,
    /// 最近一次点开详情的日志序号（列表行高亮标识）
    selected: Option<u64>,
    /// 报文详情独立窗口里的视图（窗口关闭后置 None，下次点击重开）
    detail_view: Option<Entity<LogDetailView>>,
    /// 报文详情独立窗口句柄（用于判断窗口是否仍存活）
    detail_window: Option<WindowHandle<Root>>,
}

impl CommunicationLogPanel {
    pub fn new(
        initial: Vec<CommunicationLogEntry>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let records: Vec<Arc<LogRecord>> = initial
            .iter()
            .enumerate()
            .map(|(ix, entry)| Arc::new(LogRecord::build(ix as u64, entry)))
            .collect();
        let next_seq = records.len() as u64;
        let store = CommunicationLogStore(Arc::new(Mutex::new(records)));
        let count = store.count();
        let list_state = ListState::new(count, ListAlignment::Top, ROW_HEIGHT);
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
            pending: Vec::new(),
            evicted: false,
            next_seq,
            selected: None,
            detail_view: None,
            detail_window: None,
        }
    }

    /// 点击一条日志：行高亮 + 在独立窗口中展示该帧的完整解析。
    ///
    /// 窗口只在第一次点击（或上次关闭后）创建；之后点击其他条目复用
    /// 同一窗口、仅更新内容。详情视图持有 `Arc<LogRecord>` 快照，
    /// 条目之后被环形缓冲挤掉也不影响查看。
    fn select(&mut self, seq: u64, cx: &mut Context<Self>) {
        self.selected = Some(seq);
        cx.notify();
        let Some(record) = self
            .store
            .records()
            .iter()
            .find(|record| record.seq == seq)
            .cloned()
        else {
            return;
        };
        // entity() 在窗口已关闭/未加载时返回 Err，即句柄仍持有但视图已销毁
        let window_alive = self
            .detail_window
            .as_ref()
            .is_some_and(|handle| handle.entity(cx).is_ok());
        if window_alive {
            if let Some(view) = self.detail_view.clone() {
                view.update(cx, |view, cx| {
                    view.record = Some(record);
                    cx.notify();
                });
                return;
            }
        }
        let view = cx.new(|cx| LogDetailView::new(Some(record), cx));
        let root_view = view.clone();
        // 自定义标题栏（视图内渲染 DetailTitleBar：拖拽 + 最大化 + 关闭）
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                DETAIL_WINDOW_SIZE,
                cx,
            ))),
            titlebar: Some(TitleBar::title_bar_options()),
            app_owns_titlebar_drag: true,
            window_min_size: Some(DETAIL_WINDOW_MIN),
            kind: WindowKind::Normal,
            app_id: Some("meter-engine-comm-detail".to_string()),
            ..Default::default()
        };
        let handle = cx
            .open_window(options, |window, cx| {
                window.set_window_title("MeterForgeParser");
                cx.new(|cx| Root::new(root_view, window, cx))
            })
            .expect("failed to open communication log detail window");
        self.detail_view = Some(view);
        self.detail_window = Some(handle);
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
                records.push(Arc::new(LogRecord::build(seq, &entry)));
            }
            records.len()
        };
        if self.evicted {
            self.rebuild_list(count);
        } else {
            // 尾部 splice：已有条目的位置全部保留，只追加新增条目
            // （固定行高，无需重新测量）
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
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending.clear();
        self.evicted = false;
        self.selected = None;
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

    /// 列表侧单行：定长高度，只展示摘要信息，点击选中以在右侧详情
    /// 面板查看完整解析结果。
    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
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
        let seq = record.seq;
        let is_selected = self.selected == Some(seq);
        let has_parse = !record.nodes.is_empty();

        h_flex()
            .id(("comm-log-item", ix))
            .w_full()
            .h(ROW_HEIGHT)
            .px_3()
            .gap_2()
            .items_center()
            .min_w_0()
            .border_b_1()
            .border_color(theme.border)
            .when(is_selected, |s| s.bg(theme.muted.opacity(0.6)))
            .when(!is_selected, |s| {
                s.hover(|s| s.bg(theme.muted.opacity(0.3)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select(seq, cx);
                }),
            )
            .child(
                div()
                    .flex_shrink(0.)
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.muted_foreground)
                    .child(record.time.clone()),
            )
            .child(
                div()
                    .flex_shrink(0.)
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
                    .w(px(90.))
                    .flex_shrink(0.)
                    .min_w_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(record.channel.clone()),
            )
            // 解析摘要：功能 + 数据项，一眼看出这条报文在干什么
            .child(
                div()
                    .w(px(240.))
                    .flex_shrink(0.)
                    .min_w_0()
                    .text_xs()
                    .text_color(theme.foreground)
                    .truncate()
                    .child(if record.summary.is_empty() {
                        if record.unparsed {
                            "未解析帧".to_string()
                        } else {
                            String::new()
                        }
                    } else {
                        record.summary.clone()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(record.hex.clone()),
            )
            .child(
                div()
                    .flex_shrink(0.)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} B", record.byte_count)),
            )
            .child(
                div()
                    .flex_shrink(0.)
                    .w(px(14.))
                    .text_xs()
                    .when(has_parse, |s| s.text_color(theme.success))
                    .when(!has_parse && record.unparsed, |s| {
                        s.text_color(theme.muted_foreground)
                    })
                    .child(if has_parse {
                        "●"
                    } else if record.unparsed {
                        "○"
                    } else {
                        ""
                    }),
            )
            .into_any_element()
    }
}

/// 列宽拖拽用的“假拖拽”载荷：只携带被拖动的列序号（0=字段列，
/// 1=数据列），本身不渲染任何内容（拖拽预览为空）。
///
/// 之所以借用 GPUI 的拖放系统（`on_drag` + `on_drag_move`）而不是普通的
/// `on_mouse_down` + 全局 `on_mouse_move` 来实现拖拽调宽，是因为普通的
/// `on_mouse_move` 只在鼠标悬停在“当前最上层的可命中元素”时才会触发：
/// 一旦拖拽过程中鼠标划过下方的树形列表（它自身的行也会 occlude 命中测
/// 试），挂在外层容器上的 `on_mouse_move` 就会停止收到事件，表现为拖动
/// 卡顿/跳变。而 `on_drag_move` 在拖拽开始后，不论鼠标当前处于哪个元素
/// 之上都会持续触发，因此可以流畅跟手。
#[derive(Clone)]
struct ColResizeDrag(usize);

impl Render for ColResizeDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// 报文详情视图（挂在独立窗口中）：选中条目的完整帧信息 + 树形解析
/// 结果（参照 protocol-viewer 的实现：三栏可拖列宽、树线连接、节点
/// 可展开/折叠）。
///
/// 窗口存活期间通过 [`CommunicationLogPanel::select`] 更新 `record`
/// 切换显示的条目（树在渲染时检测序号变化重建）；持有 `Arc<LogRecord>`
/// 快照，条目之后被环形缓冲挤掉也不影响查看。主窗口里流式日志驱动
/// 的列表刷新完全不触碰这里。
struct LogDetailView {
    record: Option<Arc<LogRecord>>,
    tree_state: Entity<TreeState>,
    /// 当前树对应的记录序号（检测切换、避免重复重建）
    tree_seq: Option<u64>,
    tree_lines: Rc<Vec<TreeLineInfo>>,
    /// 两个可拖拽列（字段/数据）占容器宽度的比例：默认取自
    /// `COL_WIDTH_PERCENT[0..2]`，拖拽会覆盖成用户设定的比例。
    /// 存比例而不是绝对像素，是为了让窗口缩放时列宽能跟着按比例重新
    /// 计算（而不是拖拽一次后就固定成死的像素值，窗口变了也不跟着变）。
    col_ratios: [f32; 2],
    /// 容器宽度（按比例计算实际列宽用，随窗口大小变化）
    container_width: f32,
}

impl LogDetailView {
    fn new(record: Option<Arc<LogRecord>>, cx: &mut Context<Self>) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        Self {
            record,
            tree_state,
            tree_seq: None,
            tree_lines: Rc::new(Vec::new()),
            col_ratios: [COL_WIDTH_PERCENT[0], COL_WIDTH_PERCENT[1]],
            container_width: 800.0,
        }
    }

    /// 实际列宽 = 容器宽度 * 当前比例，每帧按最新窗口宽度重新计算，
    /// 因此窗口缩放会让列宽同步跟着缩放。
    fn col_width(&self, col: usize) -> f32 {
        (self.container_width * self.col_ratios[col]).max(MIN_COL_W)
    }

    /// 拖拽产生的新列宽：每条分隔条只调它**两侧相邻**的两列，不牵动第
    /// 三方列——
    ///
    /// - 分隔条 0（字段|数据）：字段列变宽多少，数据列就压窄多少；向
    ///   左拖同理（数据列放宽）。说明列宽度保持不变。不能让说明列（flex
    ///   吃剩余空间）来吸收分隔条 0 的增减：那样数据列会保持原宽整体
    ///   平移、分隔条 1 跟着走，看起来像"把第三列也一起拖走了"，而且
    ///   数据列远没到最小宽度时说明列就先被挤到看不见。字段列的拖拽上
    ///   限 = 自身宽度 + 数据列当前可让出的宽度，数据列压到 MIN_COL_W
    ///   即停——这也修复了旧实现「两列上限互以对方当前宽度为基准」造
    ///   成的互锁（一列拖到头，另一列向右被彻底锁死）。
    /// - 分隔条 1（数据|说明）：分隔条 1 的位置 = 字段+数据宽度之和，
    ///   向右拖在几何上只能靠压缩说明列腾位置，说明列到最小宽度即停；
    ///   向左拖放宽说明列。
    ///
    /// 预留量 = 说明列最小宽度 + 两条分隔条自身的宽度 + 安全余量：确保
    /// 说明列不会被挤没、分隔条始终落在窗口内点得到。
    fn set_col_width(&mut self, col: usize, width: f32, cx: &mut Context<Self>) {
        if self.container_width <= 0.0 {
            return;
        }
        if col == 0 {
            // 分隔条 0：字段列与数据列此消彼长，说明列不动
            let w0 = self.col_width(0);
            let w1 = self.col_width(1);
            let max_w0 = w0 + (w1 - MIN_COL_W);
            let clamped = width.clamp(MIN_COL_W, max_w0);
            let new_w1 = (w1 - (clamped - w0)).max(MIN_COL_W);
            self.col_ratios[1] = new_w1 / self.container_width;
            self.col_ratios[0] = clamped / self.container_width;
        } else {
            // 分隔条 1：数据列的增减由说明列吸收
            let reserved = MIN_COL_W + RESIZE_HANDLE_W * 2.0 + 8.0;
            let max_w1 = (self.container_width - self.col_width(0) - reserved).max(MIN_COL_W);
            let clamped = width.clamp(MIN_COL_W, max_w1);
            self.col_ratios[1] = clamped / self.container_width;
        }
        cx.notify();
    }
}

impl Render for LogDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // 响应窗口大小变化（含最大化/还原）：col_width() 每次都按当前
        // container_width * 比例现算，所以这里只需更新宽度，列宽会在
        // render 时自动跟着窗口缩放重新计算。
        let bounds = window.bounds();
        self.container_width = bounds.size.width.into();

        let Some(record) = self.record.clone() else {
            return v_flex()
                .size_full()
                .child(DetailTitleBar::new("报文详情"))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("未选择日志"),
                )
                .into_any_element();
        };
        let (dir_bg, dir_label) = if record.is_tx {
            (theme.success, "TX")
        } else {
            (theme.primary, "RX")
        };

        // 记录切换时重建树（展平行 → 层级结构 + 树线信息）。列宽走固定
        // 百分比 + 拖拽偏移，不随切换记录重置。
        if self.tree_seq != Some(record.seq) {
            let lines = compute_tree_lines(&record.nodes);
            let (items, _) = build_tree_items(&record.nodes, 0, 0);
            self.tree_state
                .update(cx, |state, cx| state.set_items(items, cx));
            self.tree_lines = Rc::new(lines);
            self.tree_seq = Some(record.seq);
        }

        let cw = [self.col_width(0), self.col_width(1)];
        let line_color = theme.border;
        let tree_lines = self.tree_lines.clone();

        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme.background)
            .child(DetailTitleBar::new("报文详情"))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    // 头部信息行：方向 + 时间 + 通道 + 摘要 + 字节数 + 复制
                    .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .flex_shrink(0.)
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
                            .font_family("monospace")
                            .text_color(theme.muted_foreground)
                            .child(record.time.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(0.)
                            .max_w(px(240.))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(record.channel.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(if record.summary.is_empty() {
                                if record.unparsed {
                                    "未解析帧".to_string()
                                } else {
                                    String::new()
                                }
                            } else {
                                record.summary.clone()
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} B", record.byte_count)),
                    )
                    .child(
                        Clipboard::new("copy-comm-log-detail")
                            .value(record.hex.clone())
                            .tooltip("复制报文"),
                    ),
            )
            // 原始报文：空格分隔的等宽文本，按窗口宽度自然断行
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_shrink(0.)
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.foreground)
                    .child(record.hex.clone()),
            )
            .child(if record.nodes.is_empty() {
                // 非完整帧：无解析树可展示
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .italic()
                    .text_color(theme.muted_foreground)
                    .child("（非完整 DL/T 645-2007 帧，未解析）")
                    .into_any_element()
            } else {
                // 解析区：表头（可拖列宽）+ 树形三栏列表
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .w_full()
                            .items_stretch()
                            .flex_shrink(0.)
                            .h(px(32.))
                            .bg(theme.title_bar)
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .w(px(cw[0]))
                                    .flex_shrink(0.)
                                    .min_w_0()
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(div().truncate().child("字段")),
                            )
                            // 分隔条 0：拖拽调整字段列宽度
                            .child(
                                div()
                                    .id(("comm-detail-resize", 0usize))
                                    .w(px(RESIZE_HANDLE_W))
                                    .h_full()
                                    .flex_shrink(0.)
                                    .bg(theme.border)
                                    .cursor(CursorStyle::ResizeLeftRight)
                                    .on_drag(ColResizeDrag(0), |drag, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.new(|_| drag.clone())
                                    })
                                    // 兜底：万一拖拽途中鼠标移出了窗口边界导致
                                    // 系统没能把后续事件正常送达，松开左键时
                                    // 至少强制刷新一次，避免状态卡在中途。
                                    .on_mouse_up_out(
                                        MouseButton::Left,
                                        cx.listener(|_, _, _, cx| cx.notify()),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(cw[1]))
                                    .flex_shrink(0.)
                                    .min_w_0()
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(div().truncate().child("数据")),
                            )
                            // 分隔条 1：拖拽调整数据列宽度
                            .child(
                                div()
                                    .id(("comm-detail-resize", 1usize))
                                    .w(px(RESIZE_HANDLE_W))
                                    .h_full()
                                    .flex_shrink(0.)
                                    .bg(theme.border)
                                    .cursor(CursorStyle::ResizeLeftRight)
                                    .on_drag(ColResizeDrag(1), |drag, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.new(|_| drag.clone())
                                    })
                                    .on_mouse_up_out(
                                        MouseButton::Left,
                                        cx.listener(|_, _, _, cx| cx.notify()),
                                    ),
                            )
                            // 唯一一个 on_drag_move 监听器：GPUI 的 on_drag_move 只按
                            // “拖拽载荷类型是否匹配”触发，不区分挂在哪个元素上——如果
                            // 两个分隔条各自注册一个 on_drag_move，拖动任意一条都会让
                            // 两边的回调同时触发、用各自不同的公式互相覆盖对方写入的
                            // 列宽，导致拖动效果错乱、宽度失控变大。因此只保留这一个
                            // 集中处理的监听器，内部按 `col` 分流，整个拖拽过程只有一处
                            // 会写 `col_ratios`。
                            .child(
                                div()
                                    .id("comm-detail-resize-move-sink")
                                    .w(px(0.))
                                    .h(px(0.))
                                    .on_drag_move(cx.listener(
                                        |this, e: &DragMoveEvent<ColResizeDrag>, _, cx| {
                                            let &ColResizeDrag(col) = e.drag(cx);
                                            let x: f32 = e.event.position.x.into();
                                            let new_width = if col == 0 {
                                                x
                                            } else {
                                                // 分隔条 1 的位置 = 字段宽 +
                                                // 分隔条 0 宽 + 数据宽
                                                x - this.col_width(0) - RESIZE_HANDLE_W
                                            };
                                            this.set_col_width(col, new_width, cx);
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(div().truncate().child("说明")),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child(tree(&self.tree_state, move |ix, entry, _selected, _window, _cx| {
                                let idx: usize = entry
                                    .item()
                                    .id
                                    .trim_start_matches("row-")
                                    .parse()
                                    .unwrap_or(usize::MAX);
                                let (data, desc) = record
                                    .nodes
                                    .get(idx)
                                    .map(|node| (raw_column_text(&node.raw), node.value.clone()))
                                    .unwrap_or_default();
                                let tl = tree_lines.get(idx);
                                // 分组行 = 有子节点的行（控制码/数据域等容器）
                                let is_group = tl.is_some_and(|tl| tl.has_children);
                                let depth = tl.map(|tl| tl.depth).unwrap_or(0);

                                let prefix = tl
                                    .map(|tl| build_tree_prefix(tl, line_color))
                                    .unwrap_or_else(|| h_flex().flex_shrink(0.));

                                ListItem::new(ix).child(
                                    h_flex()
                                        .h(px(TREE_ROW_H))
                                        .min_w_0()
                                        .overflow_hidden()
                                        .items_center()
                                        // 第一列：树线前缀 + 字段名（分组行加粗，
                                        // 子字段灰显，形成链路层/应用层分组感）
                                        .child(
                                            div()
                                                .w(px(cw[0]))
                                                .flex_shrink(0.)
                                                .min_w_0()
                                                .overflow_hidden()
                                                .child(
                                                    h_flex()
                                                        .items_center()
                                                        .min_w_0()
                                                        .child(prefix)
                                                        .child(
                                                            div()
                                                                .truncate()
                                                                .text_xs()
                                                                .when(is_group, |s| {
                                                                    s.font_semibold()
                                                                })
                                                                .text_color(
                                                                    if is_group || depth == 0 {
                                                                        theme.foreground
                                                                    } else {
                                                                        theme.muted_foreground
                                                                    },
                                                                )
                                                                .child(entry.item().label.clone()),
                                                        ),
                                                ),
                                        )
                                        // 第二列：原始字节（十六进制）
                                        .child(
                                            div()
                                                .w(px(cw[1]))
                                                .flex_shrink(0.)
                                                .min_w_0()
                                                .px_2()
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_xs()
                                                        .font_family("monospace")
                                                        .text_color(theme.muted_foreground)
                                                        .child(data),
                                                ),
                                        )
                                        // 第三列：说明（解析值）
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .px_2()
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_xs()
                                                        .text_color(theme.primary)
                                                        .child(desc),
                                                ),
                                        ),
                                )
                            })),
                    )
                    .into_any_element()
            })
            )
            .into_any_element()
    }
}

// ── 解析树（参照 protocol-viewer 的树形三栏实现）──────────────

/// 树线信息：每行绘制 ├── └── 连接线所需的父子关系
#[derive(Clone, Debug)]
struct TreeLineInfo {
    depth: usize,
    /// 祖先每一级是否还有后续兄弟
    ancestor_continues: Vec<bool>,
    /// 当前行是否为最后一个子节点
    is_last_child: bool,
    /// 当前行是否有子节点
    has_children: bool,
}

fn compute_tree_lines(rows: &[FlatNode]) -> Vec<TreeLineInfo> {
    let n = rows.len();
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let depth = rows[i].depth;
        let has_children = i + 1 < n && rows[i + 1].depth > depth;

        let mut ancestor_continues = Vec::with_capacity(depth);
        for d in 0..depth {
            let mut has_later = false;
            for j in (i + 1)..n {
                if rows[j].depth < d {
                    break;
                }
                if rows[j].depth == d {
                    has_later = true;
                    break;
                }
            }
            ancestor_continues.push(has_later);
        }

        let is_last_child = {
            let mut found_sibling = false;
            for j in (i + 1)..n {
                if rows[j].depth < depth {
                    break;
                }
                if rows[j].depth == depth {
                    found_sibling = true;
                    break;
                }
            }
            !found_sibling
        };

        out.push(TreeLineInfo {
            depth,
            ancestor_continues,
            is_last_child,
            has_children,
        });
    }
    out
}

/// 把展平行按 depth 前缀重建为 Tree 层级结构（默认全部展开）
fn build_tree_items(rows: &[FlatNode], start: usize, depth: usize) -> (Vec<TreeItem>, usize) {
    let mut items = Vec::new();
    let mut i = start;

    while i < rows.len() && rows[i].depth == depth {
        let row_idx = i;
        i += 1;

        let (children, next_i) = if i < rows.len() && rows[i].depth == depth + 1 {
            build_tree_items(rows, i, depth + 1)
        } else {
            (Vec::new(), i)
        };
        i = next_i;

        let mut item = TreeItem::new(format!("row-{row_idx}"), rows[row_idx].name.clone());
        if !children.is_empty() {
            item = item.children(children).expanded(true);
        }
        items.push(item);
    }
    (items, i)
}

/// 树线绘制的一个槽位（祖先延续线 / 连接器）
fn tree_line_slot(vertical: bool, connector: bool, line_color: Hsla) -> Div {
    let half_h = TREE_ROW_H / 2.0;
    let half_w = TREE_SLOT_W / 2.0;
    let lw = TREE_LINE_W;
    let hw = lw / 2.0;

    let mut slot = div()
        .w(px(TREE_SLOT_W))
        .h(px(TREE_ROW_H))
        .flex_shrink(0.)
        .relative();
    if vertical {
        let height = if connector {
            // 叶子节点的连接竖线只画上半段（└── / ├── 的竖线部分）
            half_h + LINE_OVERLAP / 2.0
        } else {
            TREE_ROW_H + LINE_OVERLAP
        };
        slot = slot.child(
            div()
                .absolute()
                .left(px(half_w - hw))
                .top(px(0.0))
                .w(px(lw))
                .h(px(height))
                .bg(line_color),
        );
    }
    if connector {
        slot = slot.child(
            div()
                .absolute()
                .left(px(half_w))
                .top(px(half_h - hw))
                .w(px(half_w))
                .h(px(lw))
                .bg(line_color),
        );
    }
    slot
}

/// 行前缀：祖先延续线 + 连接器（├── / └──）
fn build_tree_prefix(tl: &TreeLineInfo, line_color: Hsla) -> Div {
    let mut prefix = h_flex().flex_shrink(0.);
    for k in 0..tl.depth {
        let continues = tl.ancestor_continues.get(k).copied().unwrap_or(false);
        prefix = prefix.child(tree_line_slot(continues, false, line_color));
    }
    // 连接器：非末子节点竖线贯穿整行，末子节点只画上半段
    prefix = prefix.child(tree_line_slot(true, !tl.is_last_child, line_color));
    prefix
}

// ── 详情窗口标题栏：可拖拽 + 最大化 + 关闭 ──────────────────

#[derive(IntoElement)]
struct DetailTitleBar {
    title: SharedString,
}

struct DetailTitleBarState {
    should_move: bool,
}

impl Render for DetailTitleBarState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl DetailTitleBar {
    fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

/// 标题栏单个窗口控件（最大化 / 关闭）。
///
/// `window_control_area` 走系统非客户区路径（Windows 原生处理最大化/
/// 还原/关闭）。注意**不能**在外层或这里 stop_propagation：NC 按下事件
/// 会先派发给 gpui，截断传播会让平台层认为事件已处理、跳过
/// `nc_button_pressed` 的记录，原生 toggle 随之失效（gpui Windows 端的
/// `zoom()` 只会最大化、没有还原实现，还原必须靠原生路径）。
/// `on_click` 仅作为客户区路径的兜底，且只在未最大化时触发。
fn titlebar_control(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    hover_bg: Hsla,
    hover_fg: Hsla,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .w(px(34.))
        .h_full()
        .flex_shrink(0.)
        .justify_center()
        .content_center()
        .items_center()
        .text_color(theme.foreground)
        .hover(|style| style.bg(hover_bg).text_color(hover_fg))
        .window_control_area(area)
        .on_click(move |event, window, cx| {
            on_click(event, window, cx);
        })
        .child(Icon::new(icon).small())
}

impl RenderOnce for DetailTitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_state(cx, |_, _| DetailTitleBarState { should_move: false });
        let theme = cx.theme();

        h_flex()
            .flex_shrink(0.)
            .h(px(34.))
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.title_bar_border)
            .bg(theme.tokens.title_bar)
            .child(
                h_flex()
                    .id("comm-detail-title-drag")
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .px_3()
                    .when(!cfg!(target_family = "wasm"), |this| {
                        this.window_control_area(WindowControlArea::Drag)
                    })
                    .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
                        state.should_move = false;
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        window.listener_for(&state, |state, _, _, _| {
                            state.should_move = true;
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        window.listener_for(&state, |state, _, _, _| {
                            state.should_move = false;
                        }),
                    )
                    .on_mouse_move(window.listener_for(&state, |state, _, window, _| {
                        if state.should_move {
                            state.should_move = false;
                            window.start_window_move();
                        }
                    }))
                    .child(Label::new(self.title).font_semibold()),
            )
            .child(
                h_flex()
                    .h_full()
                    .flex_shrink(0.)
                    .child(titlebar_control(
                        "comm-detail-maximize",
                        if window.is_maximized() {
                            IconName::WindowRestore
                        } else {
                            IconName::WindowMaximize
                        },
                        WindowControlArea::Max,
                        // 兜底只负责最大化；还原必须交给原生 NC 路径
                        // （gpui 的 zoom() 没有还原实现）
                        |_, window, _| {
                            if !window.is_maximized() {
                                window.zoom_window();
                            }
                        },
                        theme.muted,
                        theme.foreground,
                        theme,
                    ))
                    .child(titlebar_control(
                        "comm-detail-close",
                        IconName::WindowClose,
                        WindowControlArea::Close,
                        |_, window, _| window.remove_window(),
                        theme.danger,
                        theme.danger_foreground,
                        theme,
                    )),
            )
    }
}

/// raw 列文本：完整显示字节序列（十六进制，空格分隔），列宽不够时由
/// `.truncate()` 省略号截断——不再按固定字节数预裁剪，列宽拖宽后能看到
/// 更多字节。完整字节始终在窗口顶部的 hex 文本里可查。
fn raw_column_text(raw: &[u8]) -> String {
    raw.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
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
                // 纯报文列表占满面板宽度；详情点击行弹出对话框，
                // 不再与列表在矮横条里分抢空间。
                div()
                    .id("comm-log-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
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