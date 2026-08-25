use chrono::Local;
use dlt645_2007::{decode_message, FieldValue};
use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
};
use tokio::sync::broadcast;

/// 传给 spec-engine 的协议标识（DL/T 645-2007）
pub const PARSE_PROTOCOL_ID: &str = "dlt645-2007";
/// 数据项内容解析采用的区域规范
pub const PARSE_REGION: &str = "南网";

/// DL/T 645 广播地址：所有电表都会响应（如广播校时），
/// 因此广播帧要出现在每一台电表各自的通信日志里。
pub const BROADCAST_ADDRESS: [u8; 6] = [0x99; 6];

/// 单台电表的日志环形缓冲上限。
///
/// 总线场景下最多可能有 2000 台虚拟表共享同一个物理通道，如果只按
/// "全局最近 N 条" 缓存，其他表的高频报文会在几百毫秒内把某一台表
/// 仅有的几条历史记录全部挤出窗口——UI 打开这台表的日志时看到的其实
/// 是别的表的数据。因此这里按地址分桶，每台表独立计数、独立环形。
const PER_METER_CAP: usize = 300;
/// 广播帧独立分桶（不属于任何单一地址），同样做上限截断。
const BROADCAST_CAP: usize = 300;

#[derive(Debug, Clone)]
pub struct CommunicationLogEntry {
    pub timestamp_ms: i64,
    pub direction: &'static str,
    pub channel: String,
    pub data: Vec<u8>,
    /// 完整帧解析结果（链路层 + 应用层 + 数据项内容）。
    /// None 表示该段数据不是一条可完整解析的 645 帧（噪声/半截帧/校验失败）。
    pub parsed: Option<FieldValue>,
    /// 一句话摘要（"读数据 · (当前)组合有功总电能"），由结构化的
    /// `Message` 在解析时提取，供日志列表直接展示，无需再遍历解析树。
    /// 未解析帧为空字符串。
    pub summary: String,
    /// 帧地址域（原始 6 字节 BCD），直接从原始字节摘取，不要求整帧能
    /// 完整解析成功——总线上偶发的半截帧/校验错误也可能是"发给这台表"
    /// 的，按地址过滤时仍需要保留。取不到（数据太短或没有 68H 帧头）
    /// 时为 None。
    pub address: Option<[u8; 6]>,
}

/// 直接从原始字节摘取地址域（68H + 6 字节地址），不依赖 [`parse_frame`]
/// 的完整解码结果。
fn extract_address(data: &[u8]) -> Option<[u8; 6]> {
    if data.len() >= 7 && data[0] == 0x68 {
        data[1..7].try_into().ok()
    } else {
        None
    }
}

/// 把 `FieldValue` 树展平成带缩进深度的行，UI 详情表格与日志文件共用同一份结果。
#[derive(Debug, Clone, PartialEq)]
pub struct FlatNode {
    pub depth: usize,
    pub name: String,
    pub raw: Vec<u8>,
    pub value: String,
}

/// 完整解析一条 DL/T 645-2007 帧。
///
/// 仅当整段数据恰好是一条帧（允许前导 FE 字节）时才返回（解析树, 摘要），
/// 避免把带残留噪声的缓冲渲染成看似正确的字段。摘要由结构化的
/// `Message` 提取（功能码 + 数据项名称），与展示树解耦。
fn parse_frame(data: &[u8]) -> Option<(FieldValue, String)> {
    if data.is_empty() {
        return None;
    }
    let (msg, consumed) = decode_message(data, PARSE_PROTOCOL_ID, PARSE_REGION).ok()?;
    if consumed != data.len() {
        return None;
    }
    let summary = msg.summary(PARSE_PROTOCOL_ID, PARSE_REGION);
    let tree = msg.to_value_tree(PARSE_PROTOCOL_ID, PARSE_REGION).ok()?;
    Some((tree, summary))
}

/// 展平解析树：根节点是整条报文的汇总行，直接展开其子字段。
pub fn flatten_value_tree(tree: &FieldValue) -> Vec<FlatNode> {
    let mut out = Vec::new();
    if let FieldValue::Node { value, .. } = tree {
        if matches!(**value, FieldValue::List(_)) {
            flatten_into(value, 0, &mut out);
            return out;
        }
    }
    flatten_into(tree, 0, &mut out);
    out
}

fn flatten_into(value: &FieldValue, depth: usize, out: &mut Vec<FlatNode>) {
    match value {
        // List 本身不产生行，项的深度由父 Node 递归时 +1
        FieldValue::List(items) => {
            for item in items {
                flatten_into(item, depth, out);
            }
        }
        FieldValue::Map(pairs) => {
            if pairs.len() == 1 {
                // 单条目 Map 是包装层，跳过后不增加深度（与 protocol-viewer 的展示语义一致）
                flatten_into(&pairs[0].1, depth, out);
            } else {
                for (key, val) in pairs {
                    match val {
                        FieldValue::List(_) | FieldValue::Map(_) | FieldValue::Node { .. } => {
                            flatten_into(val, depth, out);
                        }
                        FieldValue::Skip => {}
                        _ => out.push(FlatNode {
                            depth,
                            name: key.clone(),
                            raw: Vec::new(),
                            value: scalar_text(val),
                        }),
                    }
                }
            }
        }
        FieldValue::Node { name, raw, value } => {
            let is_container =
                matches!(**value, FieldValue::List(_) | FieldValue::Map(_) | FieldValue::Node { .. });
            // 位域节点约定 raw 留空、原始字节放在 Bit.bit_byte 里
            let row_raw = match &**value {
                FieldValue::Bit { bit_byte, .. } if raw.is_empty() && !bit_byte.is_empty() => {
                    bit_byte.clone()
                }
                _ => raw.clone(),
            };
            out.push(FlatNode {
                depth,
                name: name.clone(),
                raw: row_raw,
                value: if is_container {
                    String::new()
                } else {
                    scalar_text(value)
                },
            });
            if is_container {
                flatten_into(value, depth + 1, out);
            }
        }
        FieldValue::Skip => {}
        _ => out.push(FlatNode {
            depth,
            name: String::new(),
            raw: Vec::new(),
            value: scalar_text(value),
        }),
    }
}

fn scalar_text(value: &FieldValue) -> String {
    match value {
        FieldValue::Int(i) => i.to_string(),
        FieldValue::Float(f) => f.to_string(),
        FieldValue::Str(s) => s.clone(),
        FieldValue::Bytes(b) => to_hex(b),
        FieldValue::WithUnit { value, unit } => format!("{} {}", scalar_text(value), unit),
        FieldValue::Pn(i) => i.to_string(),
        FieldValue::Invalid { reason } => format!("无效: {reason}"),
        FieldValue::Bit {
            bit_start,
            bit_end,
            bit_value,
            value,
            ..
        } => match value {
            Some(desc) => scalar_text(desc),
            None => format!("D{bit_start}~D{bit_end}={bit_value}"),
        },
        _ => String::new(),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone)]
pub struct CommunicationLogService {
    /// 按电表地址分桶的日志环形缓冲，key 为 6 字节 BCD 地址。
    per_address: Arc<Mutex<HashMap<[u8; 6], VecDeque<CommunicationLogEntry>>>>,
    /// 广播帧单独分桶（不对应任何单一地址）。
    broadcast_entries: Arc<Mutex<VecDeque<CommunicationLogEntry>>>,
    events: broadcast::Sender<CommunicationLogEntry>,
    file_tx: mpsc::Sender<CommunicationLogEntry>,
}

impl CommunicationLogService {
    pub fn new(path: PathBuf) -> Self {
        let per_address = Arc::new(Mutex::new(HashMap::new()));
        let broadcast_entries = Arc::new(Mutex::new(VecDeque::with_capacity(BROADCAST_CAP)));
        // 总线上可能同时有大量虚拟表在收发，缓冲适当放大，避免 UI 侧
        // 订阅者一时处理不过来（比如页面刚打开还在建 LogRecord）时被
        // broadcast channel 直接丢弃事件。
        let (events, _) = broadcast::channel(2_000);
        let (file_tx, file_rx) = mpsc::channel::<CommunicationLogEntry>();
        thread::spawn(move || {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            while let Ok(entry) = file_rx.recv() {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
                    let _ = writeln!(
                        file,
                        "{} {} {} {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                        entry.direction,
                        entry.channel,
                        to_hex(&entry.data)
                    );
                    if let Some(tree) = &entry.parsed {
                        for node in flatten_value_tree(tree) {
                            let _ = writeln!(
                                file,
                                "    {}{} | {} | {}",
                                "  ".repeat(node.depth),
                                node.name,
                                to_hex(&node.raw),
                                node.value
                            );
                        }
                    }
                }
            }
        });
        Self {
            per_address,
            broadcast_entries,
            events,
            file_tx,
        }
    }

    /// 记录一条报文。磁盘文件仍然记录总线上的全部原始流量（不分地址），
    /// 内存里的环形缓冲则按地址分桶，供 UI 按"这台表"过滤查询/订阅。
    /// 摘不到地址（噪声/半截帧）的报文只落盘，不进内存缓冲——它不属于
    /// 任何一台表，留在内存里也无法按地址检索。
    pub fn record(&self, direction: &'static str, channel: impl Into<String>, data: &[u8]) {
        let (parsed, summary) = match parse_frame(data) {
            Some((tree, summary)) => (Some(tree), summary),
            None => (None, String::new()),
        };
        let entry = CommunicationLogEntry {
            timestamp_ms: Local::now().timestamp_millis(),
            direction,
            channel: channel.into(),
            data: data.to_vec(),
            parsed,
            summary,
            address: extract_address(data),
        };
        match entry.address {
            Some(BROADCAST_ADDRESS) => {
                if let Ok(mut buf) = self.broadcast_entries.lock() {
                    if buf.len() == BROADCAST_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(entry.clone());
                }
            }
            Some(addr) => {
                if let Ok(mut map) = self.per_address.lock() {
                    let buf = map
                        .entry(addr)
                        .or_insert_with(|| VecDeque::with_capacity(PER_METER_CAP));
                    if buf.len() == PER_METER_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(entry.clone());
                }
            }
            None => {}
        }
        let _ = self.file_tx.send(entry.clone());
        let _ = self.events.send(entry);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CommunicationLogEntry> {
        self.events.subscribe()
    }

    /// 某台电表的历史报文（含发给它的广播帧），按时间升序合并、
    /// 截断到最近 [`PER_METER_CAP`] 条。
    pub fn entries_for(&self, address: [u8; 6]) -> Vec<CommunicationLogEntry> {
        let mut merged: Vec<CommunicationLogEntry> = Vec::new();
        if let Ok(map) = self.per_address.lock() {
            if let Some(buf) = map.get(&address) {
                merged.extend(buf.iter().cloned());
            }
        }
        if let Ok(buf) = self.broadcast_entries.lock() {
            merged.extend(buf.iter().cloned());
        }
        merged.sort_by_key(|e| e.timestamp_ms);
        if merged.len() > PER_METER_CAP {
            let drop_count = merged.len() - PER_METER_CAP;
            merged.drain(0..drop_count);
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读数据请求帧：地址 123456789012，DI=00010000（组合有功总电能）
    fn read_request_frame() -> Vec<u8> {
        let mut bytes = vec![
            0x68, 0x12, 0x90, 0x78, 0x56, 0x34, 0x12, 0x68, 0x11, 0x04, 0x33, 0x33, 0x34, 0x33,
            0x00, 0x16,
        ];
        let len = bytes.len();
        let cs: u8 = bytes[0..len - 2]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        bytes[len - 2] = cs;
        bytes
    }

    #[test]
    fn parses_read_request_into_flat_rows() {
        let (tree, summary) = parse_frame(&read_request_frame()).expect("合法帧应解析成功");
        let rows = flatten_value_tree(&tree);
        assert!(!rows.is_empty());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        for expected in ["起始符", "地址域", "控制码", "数据长度", "校验码", "结束符"] {
            assert!(names.contains(&expected), "缺少字段 {expected}: {names:?}");
        }
        // 控制码是分组节点，展开后应包含方向位与功能码位域行
        let control = rows
            .iter()
            .find(|r| r.name == "控制码")
            .expect("应包含控制码节点");
        assert_eq!(control.value, "");
        assert!(rows
            .iter()
            .any(|r| r.depth > 0 && r.name.contains("功能码") && r.value.contains("读数据")));
        // 摘要从结构化 Message 提取：功能码 + 数据项
        assert!(summary.contains("读数据"), "摘要应包含功能码: {summary}");
    }

    /// 读数据应答帧：地址 123456789012，DI=00010000，数据 123.45（BCD 45 23 01 00，低位在前）
    fn read_response_frame() -> Vec<u8> {
        let mut bytes = vec![
            0x68, 0x12, 0x90, 0x78, 0x56, 0x34, 0x12, 0x68, 0x91, 0x08, // 控制码 91H = 从站正常应答·读数据
            0x33, 0x33, 0x34, 0x33, // DI 00 00 01 00 + 33H
            0x78, 0x56, 0x34, 0x33, // 45 23 01 00 + 33H
            0x00, 0x16,
        ];
        let len = bytes.len();
        let cs: u8 = bytes[0..len - 2]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        bytes[len - 2] = cs;
        bytes
    }

    #[test]
    fn parses_read_response_with_data_items() {
        let (tree, summary) = parse_frame(&read_response_frame()).expect("应答帧应解析成功");
        let rows = flatten_value_tree(&tree);
        assert!(summary.contains("读数据"), "应答摘要应包含功能码: {summary}");
        // 控制码 91H：从站→主站、正常应答、读数据
        assert!(rows
            .iter()
            .any(|r| r.depth == 1 && r.value == "方向=从站→主站"));
        assert!(rows.iter().any(|r| r.depth == 1 && r.value == "读数据"));
        // DI 00010000 的内容经 spec-engine 按南网规范解析为带单位数值
        let item = rows
            .iter()
            .find(|r| r.depth == 1 && r.name.contains("00010000"))
            .expect("应包含 DI 00010000 数据项");
        assert_eq!(item.value, "123.45 kWh");
        assert_eq!(item.raw, vec![0x45, 0x23, 0x01, 0x00]);
        assert!(rows.iter().any(|r| r.name == "校验码" && r.value == "校验正确"));
    }

    /// 数据块读应答帧（现场实录）：地址 000000000001，DI=0000FF00 组合有功电能数据块，
    /// 数据 20 字节 = 总/费率1~4 五个电能值各 4 字节 BCD
    fn block_response_frame() -> Vec<u8> {
        vec![
            0x68, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x68, 0x91, 0x18, 0x33, 0x32, 0x33, 0x33,
            0x44, 0x7A, 0x3C, 0x33, 0x84, 0x6A, 0x35, 0x33, 0x98, 0x46, 0x35, 0x33, 0x87, 0x3C,
            0x35, 0x33, 0x69, 0xB9, 0x35, 0x33, 0xC3, 0x16,
        ]
    }

    #[test]
    fn parses_block_response_with_sub_item_values() {
        let (tree, summary) = parse_frame(&block_response_frame()).expect("数据块帧应解析成功");
        let rows = flatten_value_tree(&tree);
        assert!(summary.contains("读数据"), "数据块摘要应包含功能码: {summary}");
        // 数据块节点本身是分组行，raw 为 20 字节数据块
        let block = rows
            .iter()
            .find(|r| r.name.contains("0000FF00"))
            .expect("应包含数据块节点");
        assert_eq!(block.raw.len(), 20);
        assert_eq!(block.value, "");
        // 五个子项都应带原始字节与解析值（总 + 费率1~4）
        let expected = [
            ("00000000", "947.11 kWh"),
            ("00000100", "237.51 kWh"),
            ("00000200", "213.65 kWh"),
            ("00000300", "209.54 kWh"),
            ("00000400", "286.36 kWh"),
        ];
        for (di, value) in expected {
            let item = rows
                .iter()
                .find(|r| r.name.contains(di))
                .unwrap_or_else(|| panic!("缺少子项 {di}: {rows:?}"));
            assert_eq!(item.value, value, "子项 {di} 解析值不符");
            assert_eq!(item.raw.len(), 4);
            assert!(item.depth > block.depth);
        }
    }

    #[test]
    fn rejects_garbage_and_trailing_bytes() {
        assert!(parse_frame(&[0x00, 0x01, 0x02]).is_none());
        let mut frame = read_request_frame();
        frame.push(0x00);
        assert!(parse_frame(&frame).is_none(), "带尾部噪声不应解析");
    }
}