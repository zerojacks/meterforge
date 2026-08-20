use chrono::Local;
use dlt645_2007::{decode_message, FieldValue};
use std::{
    collections::VecDeque,
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

#[derive(Debug, Clone)]
pub struct CommunicationLogEntry {
    pub timestamp_ms: i64,
    pub direction: &'static str,
    pub channel: String,
    pub data: Vec<u8>,
    /// 完整帧解析结果（链路层 + 应用层 + 数据项内容）。
    /// None 表示该段数据不是一条可完整解析的 645 帧（噪声/半截帧/校验失败）。
    pub parsed: Option<FieldValue>,
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
/// 仅当整段数据恰好是一条帧（允许前导 FE 字节）时才返回解析树，
/// 避免把带残留噪声的缓冲渲染成看似正确的字段。
fn parse_frame(data: &[u8]) -> Option<FieldValue> {
    if data.is_empty() {
        return None;
    }
    let (msg, consumed) = decode_message(data, PARSE_PROTOCOL_ID, PARSE_REGION).ok()?;
    if consumed != data.len() {
        return None;
    }
    msg.to_value_tree(PARSE_PROTOCOL_ID, PARSE_REGION).ok()
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
    entries: Arc<Mutex<VecDeque<CommunicationLogEntry>>>,
    events: broadcast::Sender<CommunicationLogEntry>,
    file_tx: mpsc::Sender<CommunicationLogEntry>,
}

impl CommunicationLogService {
    pub fn new(path: PathBuf) -> Self {
        let entries = Arc::new(Mutex::new(VecDeque::with_capacity(1_000)));
        let (events, _) = broadcast::channel(1_000);
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
            entries,
            events,
            file_tx,
        }
    }

    pub fn record(&self, direction: &'static str, channel: impl Into<String>, data: &[u8]) {
        let entry = CommunicationLogEntry {
            timestamp_ms: Local::now().timestamp_millis(),
            direction,
            channel: channel.into(),
            data: data.to_vec(),
            parsed: parse_frame(data),
        };
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == 1_000 {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }
        let _ = self.file_tx.send(entry.clone());
        let _ = self.events.send(entry);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<CommunicationLogEntry> {
        self.events.subscribe()
    }
    pub fn entries(&self) -> Vec<CommunicationLogEntry> {
        self.entries
            .lock()
            .map(|items| items.iter().cloned().collect())
            .unwrap_or_default()
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
        let tree = parse_frame(&read_request_frame()).expect("合法帧应解析成功");
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
        let tree = parse_frame(&read_response_frame()).expect("应答帧应解析成功");
        let rows = flatten_value_tree(&tree);
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
        let tree = parse_frame(&block_response_frame()).expect("数据块帧应解析成功");
        let rows = flatten_value_tree(&tree);
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
