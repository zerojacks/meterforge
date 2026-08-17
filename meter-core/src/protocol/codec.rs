// DL/T 645-2007 帧编解码器

use super::frame::Frame;
use crate::error::{MeterError, Result};

/// 错误信息字（ERR）位定义
///
/// 异常应答（控制码 D6=1）的 DATA = 1 字节 ERR
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorInfoWord(u8);

impl ErrorInfoWord {
    pub const OTHER: u8 = 0b0000_0001; // bit0 其他错误
    pub const NO_DATA: u8 = 0b0000_0010; // bit1 无请求数据（DI不存在）
    pub const PASSWORD_ERR: u8 = 0b0000_0100; // bit2 密码错/未授权
    pub const BAUD_LOCKED: u8 = 0b0000_1000; // bit3 通讯速率不能更改
    pub const TOU_ZONE_EXCEED: u8 = 0b0001_0000; // bit4 年时区数超
    pub const TIME_SLOT_EXCEED: u8 = 0b0010_0000; // bit5 日时段数超
    pub const RATE_EXCEED: u8 = 0b0100_0000; // bit6 费率数超
                                             // bit7 保留

    pub fn new(bits: u8) -> Self {
        Self(bits)
    }

    pub fn bits(&self) -> u8 {
        self.0
    }
}

/// 解码 DL/T 645-2007 帧
///
/// 帧格式: 68H A0~A5 68H C L DATA CS 16H
/// - 68H: 起始符
/// - A0~A5: 地址域（6字节，BCD码，低位在前）
/// - 68H: 起始符（第二次）
/// - C: 控制码
/// - L: 数据域长度
/// - DATA: 数据域（每字节需 -33H）
/// - CS: 校验和（地址+控制+长度+数据）
/// - 16H: 结束符
pub fn decode_frame(raw: &[u8]) -> Result<Frame> {
    // 最小帧长度：68H(1) + ADDR(6) + 68H(1) + C(1) + L(1) + CS(1) + 16H(1) = 12
    if raw.len() < 12 {
        return Err(MeterError::InvalidFrame(format!(
            "Frame too short: {} bytes",
            raw.len()
        )));
    }

    // 检查起始符
    if raw[0] != 0x68 {
        return Err(MeterError::InvalidFrame(format!(
            "Invalid start byte: 0x{:02X}",
            raw[0]
        )));
    }

    if raw[7] != 0x68 {
        return Err(MeterError::InvalidFrame(format!(
            "Invalid second start byte at pos 7: 0x{:02X}",
            raw[7]
        )));
    }

    // 提取地址域（6 字节）
    let address: [u8; 6] = raw[1..7].try_into().unwrap();

    // 提取控制码
    let control = raw[8];

    // 提取数据域长度
    let data_len = raw[9] as usize;

    // 检查帧长度
    let expected_len = 10 + data_len + 2; // 68H(1) + ADDR(6) + 68H(1) + C(1) + L(1) + DATA + CS(1) + 16H(1)
    if raw.len() < expected_len {
        return Err(MeterError::InvalidFrame(format!(
            "Frame incomplete: expected {} bytes, got {}",
            expected_len,
            raw.len()
        )));
    }

    // 提取数据域（需要 -33H）
    let data_offset_start = 10;
    let data_offset_end = data_offset_start + data_len;
    let data_with_offset = &raw[data_offset_start..data_offset_end];
    let data: Vec<u8> = data_with_offset
        .iter()
        .map(|&b| b.wrapping_sub(0x33))
        .collect();

    // 提取校验和
    let cs_pos = data_offset_end;
    let cs_received = raw[cs_pos];

    // 计算校验和（地址 + 控制 + 长度 + 数据域原始字节）
    let mut cs_calculated: u8 = 0;
    for &b in &raw[0..cs_pos] {
        // 地址域
        cs_calculated = cs_calculated.wrapping_add(b);
    }

    if cs_calculated != cs_received {
        return Err(MeterError::ChecksumMismatch {
            expected: cs_calculated,
            actual: cs_received,
        });
    }

    // 检查结束符
    let end_pos = cs_pos + 1;
    if raw[end_pos] != 0x16 {
        return Err(MeterError::InvalidFrame(format!(
            "Invalid end byte: 0x{:02X}",
            raw[end_pos]
        )));
    }

    Ok(Frame {
        address,
        control,
        data,
    })
}

/// 编码帧（内部实现，可控制 DATA 域是否做 +33H 偏移）
fn encode_frame_impl(address: &[u8; 6], control: u8, data: &[u8], apply_offset: bool) -> Vec<u8> {
    let data_len = data.len();

    // 计算总长度
    let total_len = 12 + data_len; // 68H(1) + ADDR(6) + 68H(1) + C(1) + L(1) + DATA + CS(1) + 16H(1)

    let mut buf = Vec::with_capacity(total_len);

    // 起始符
    buf.push(0x68);
    // 地址域
    buf.extend_from_slice(address);
    // 起始符（第二次）
    buf.push(0x68);
    // 控制码
    buf.push(control);
    // 数据域长度
    buf.push(data_len as u8);

    // 数据域
    let transformed: Vec<u8> = data
        .iter()
        .map(|&b| {
            if apply_offset {
                b.wrapping_add(0x33)
            } else {
                b
            }
        })
        .collect();
    buf.extend_from_slice(&transformed);

    // 计算校验和（DL/T645-2007 5.2.6：从第一个帧起始符起到校验码之前所有字节的模256和）
    let mut cs: u8 = 0x68; // 第一个起始符
    cs = cs.wrapping_add(0x68); // 第二个起始符（地址域之后）
    for &b in address {
        cs = cs.wrapping_add(b);
    }
    cs = cs.wrapping_add(control);
    cs = cs.wrapping_add(data_len as u8);
    for &b in &transformed {
        cs = cs.wrapping_add(b);
    }
    buf.push(cs);

    // 结束符
    buf.push(0x16);

    buf
}

/// 编码 DL/T 645-2007 帧（DATA 域 +33H）
///
/// 将 Frame 结构编码为字节流
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    encode_frame_impl(&frame.address, frame.control, &frame.data, true)
}

/// 编码帧（DATA 域不做 +33H 偏移）
///
/// 用于读通信地址 13H 响应等协议例外（地址字节不属于 +33H 偏移范围）。
pub fn encode_frame_raw(address: [u8; 6], control: u8, data: &[u8]) -> Vec<u8> {
    encode_frame_impl(&address, control, data, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let frame = Frame::read(
            [0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
            [0x00, 0x01, 0x00, 0x00],
        );

        let encoded = encode_frame(&frame);
        let decoded = decode_frame(&encoded).unwrap();

        assert_eq!(decoded.address, frame.address);
        assert_eq!(decoded.control, frame.control);
        assert_eq!(decoded.data, frame.data);
    }

    #[test]
    fn test_checksum_error() {
        let raw = vec![
            0x68, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x68, 0x11, 0x04, 0x33, 0x34, 0x33, 0x33,
            0x00, // 错误的校验和
            0x16,
        ];

        let result = decode_frame(&raw);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            MeterError::ChecksumMismatch { .. }
        ));
    }
}

/// 从数据域中解析 DI（数据标识）
///
/// DI 格式：DI0 DI1 DI2 DI3（4字节，低字节先传）
///
/// # 返回
/// - (DI3, DI2, DI1, DI0, rest_data)
pub fn parse_di(data: &[u8]) -> Result<([u8; 4], &[u8])> {
    if data.len() < 4 {
        return Err(MeterError::InvalidFrame(format!(
            "Data too short for DI: {} bytes",
            data.len()
        )));
    }

    let di = [data[0], data[1], data[2], data[3]];
    let rest = &data[4..];

    Ok((di, rest))
}

/// 编码数据块（通配查询）响应
///
/// 当 DI2/DI1/DI0 包含 `FFH` 通配符时，响应格式为：
/// ```text
/// DATA = 原始请求DI(4字节) + m(1字节) + 数据项序列
/// 数据项序列 = (DI0 DI1 DI2 DI3)₁ + 值₁ + (DI0 DI1 DI2 DI3)₂ + 值₂ + ...
/// ```
///
/// # 参数
/// - `request_di`: 原始请求DI（含FFH通配符）
/// - `items`: (实际DI, 值字节) 列表
///
/// # 返回
/// - 编码后的数据域字节
pub fn encode_data_block_response(request_di: [u8; 4], items: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut data = request_di.to_vec();
    data.push(items.len().min(255) as u8); // m，协议保证不超255

    for (actual_di, value_bytes) in items {
        // DI0 DI1 DI2 DI3（低字节先传）
        data.extend_from_slice(&actual_di);
        data.extend(value_bytes);
    }

    data
}

/// 编码错误响应帧
///
/// # 参数
/// - `address`: 电表地址
/// - `request_control_code`: 请求的控制码
/// - `err`: 错误信息字
///
/// # 返回
/// - 完整的错误响应帧字节
pub fn encode_error_response(
    address: [u8; 6],
    request_control_code: u8,
    err: ErrorInfoWord,
) -> Vec<u8> {
    // 错误控制码：D7=1（从站应答），D6=1（异常）
    let error_control = (request_control_code & 0x1F) | 0xC0;

    let frame = Frame {
        address,
        control: error_control,
        data: vec![err.bits()],
    };

    encode_frame(&frame)
}

/// 判断地址是否为广播地址
///
/// 广播地址：6 字节全为 `99H`
pub fn is_broadcast_address(addr_bytes: &[u8; 6]) -> bool {
    addr_bytes.iter().all(|&b| b == 0x99)
}

/// 判断地址是否为通配地址
///
/// 通配地址：任意字节为 `AAH`
pub fn is_wildcard_address(addr_bytes: &[u8; 6]) -> bool {
    addr_bytes.iter().any(|&b| b == 0xAA)
}

/// 验证广播命令是否合法
///
/// 只允许广播校时（08H）和广播冻结（16H）
pub fn validate_broadcast_command(control_code: u8) -> bool {
    matches!(control_code & 0x1F, 0x08 | 0x16)
}

/// 匹配通配地址
///
/// # 参数
/// - `pattern`: 地址模式（可包含 `AAH` 通配符）
/// - `actual`: 实际地址
///
/// # 返回
/// - 是否匹配
pub fn match_address(pattern: &[u8; 6], actual: &[u8; 6]) -> bool {
    pattern
        .iter()
        .zip(actual.iter())
        .all(|(p, a)| *p == 0xAA || *p == *a)
}

#[cfg(test)]
mod codec_extended_tests {
    use super::*;

    #[test]
    fn test_parse_di() {
        let data = vec![0x00, 0x01, 0x00, 0x00, 0xAA, 0xBB];
        let (di, rest) = parse_di(&data).unwrap();

        assert_eq!(di, [0x00, 0x01, 0x00, 0x00]);
        assert_eq!(rest, &[0xAA, 0xBB]);
    }

    #[test]
    fn test_parse_di_too_short() {
        let data = vec![0x00, 0x01, 0x00];
        let result = parse_di(&data);

        assert!(result.is_err());
    }

    #[test]
    fn test_encode_data_block_response() {
        let request_di = [0xFF, 0x01, 0x00, 0x00];
        let items = vec![
            ([0x01, 0x01, 0x00, 0x00], vec![0x12, 0x34]),
            ([0x02, 0x01, 0x00, 0x00], vec![0x56, 0x78]),
        ];

        let data = encode_data_block_response(request_di, items);

        // 原始DI(4) + m(1) + DI1(4) + 值1(2) + DI2(4) + 值2(2) = 17 bytes
        assert_eq!(data.len(), 17);
        assert_eq!(&data[0..4], &[0xFF, 0x01, 0x00, 0x00]); // 原始DI
        assert_eq!(data[4], 2); // m=2
        assert_eq!(&data[5..9], &[0x01, 0x01, 0x00, 0x00]); // DI1
        assert_eq!(&data[9..11], &[0x12, 0x34]); // 值1
        assert_eq!(&data[11..15], &[0x02, 0x01, 0x00, 0x00]); // DI2
        assert_eq!(&data[15..17], &[0x56, 0x78]); // 值2
    }

    #[test]
    fn test_encode_error_response() {
        let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let request_control = 0x11; // 读数据
        let err = ErrorInfoWord::new(ErrorInfoWord::NO_DATA);

        let response_bytes = encode_error_response(address, request_control, err);

        // 解码验证
        let frame = decode_frame(&response_bytes).unwrap();
        assert_eq!(frame.address, address);
        assert_eq!(frame.control, 0xD1); // 0x11 | 0xC0 = 0xD1
        assert_eq!(frame.data, vec![ErrorInfoWord::NO_DATA]);
    }

    #[test]
    fn test_encode_frame_raw_no_offset() {
        // 13H 读通信地址响应：DATA 域不加 +33H 偏移
        let addr = [0x12, 0x90, 0x78, 0x56, 0x34, 0x12];
        let raw = encode_frame_raw(addr, 0x93, &addr);

        assert_eq!(raw[0], 0x68);
        assert_eq!(raw[7], 0x68);
        assert_eq!(raw[8], 0x93);
        assert_eq!(raw[9], 6); // L = 6
        assert_eq!(&raw[10..16], &addr); // 地址字节无偏移
        assert_eq!(raw[raw.len() - 1], 0x16);
    }

    #[test]
    fn test_is_broadcast_address() {
        assert!(is_broadcast_address(&[0x99, 0x99, 0x99, 0x99, 0x99, 0x99]));
        assert!(!is_broadcast_address(&[0x01, 0x99, 0x99, 0x99, 0x99, 0x99]));
        assert!(!is_broadcast_address(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn test_is_wildcard_address() {
        assert!(is_wildcard_address(&[0xAA, 0x00, 0x00, 0x00, 0x00, 0x00]));
        assert!(is_wildcard_address(&[0x01, 0x02, 0xAA, 0x04, 0x05, 0x06]));
        assert!(!is_wildcard_address(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]));
    }

    #[test]
    fn test_validate_broadcast_command() {
        assert!(validate_broadcast_command(0x08)); // 广播校时
        assert!(validate_broadcast_command(0x16)); // 广播冻结
        assert!(!validate_broadcast_command(0x11)); // 读数据不允许广播
        assert!(!validate_broadcast_command(0x14)); // 写数据不允许广播
    }

    #[test]
    fn test_match_address() {
        let pattern = [0xAA, 0x02, 0x03, 0xAA, 0x05, 0x06];

        assert!(match_address(
            &pattern,
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]
        ));
        assert!(match_address(
            &pattern,
            &[0xFF, 0x02, 0x03, 0x00, 0x05, 0x06]
        ));
        assert!(!match_address(
            &pattern,
            &[0x01, 0xFF, 0x03, 0x04, 0x05, 0x06]
        )); // 不匹配位置1
        assert!(!match_address(
            &pattern,
            &[0x01, 0x02, 0xFF, 0x04, 0x05, 0x06]
        )); // 不匹配位置2
    }

    #[test]
    fn test_error_info_word() {
        let err = ErrorInfoWord::new(ErrorInfoWord::NO_DATA | ErrorInfoWord::PASSWORD_ERR);
        assert_eq!(err.bits(), 0b0000_0110);

        let err2 = ErrorInfoWord::new(ErrorInfoWord::OTHER);
        assert_eq!(err2.bits(), 0b0000_0001);
    }
}
