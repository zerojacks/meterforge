// DL/T 645-2007 数据格式转换工具
// 支持 BCD、HEX、时间戳等格式

use crate::error::{MeterError, Result};
use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};

/// 将 BCD 编码的字节数组转换为 u64
///
/// 例如: [0x34, 0x12] -> 1234
pub fn bcd_to_u64(data: &[u8]) -> Result<u64> {
    let mut result = 0u64;

    for &byte in data.iter().rev() {
        let high = (byte >> 4) & 0x0F;
        let low = byte & 0x0F;

        // 检查是否为有效 BCD（0-9）
        if high > 9 || low > 9 {
            return Err(MeterError::ParseError(format!(
                "Invalid BCD byte: 0x{:02X}",
                byte
            )));
        }

        result = result * 100 + (high as u64) * 10 + (low as u64);
    }

    Ok(result)
}

/// 将 u64 转换为 BCD 编码的字节数组
///
/// 例如: 1234, 2 -> [0x34, 0x12]
pub fn u64_to_bcd(value: u64, len: usize) -> Vec<u8> {
    let mut result = vec![0u8; len];
    let mut val = value;

    for i in 0..len {
        let low = (val % 10) as u8;
        val /= 10;
        let high = (val % 10) as u8;
        val /= 10;

        result[i] = (high << 4) | low;
    }

    result
}

/// 将 BCD 编码的字节数组转换为浮点数（带小数位）
///
/// 例如: [0x34, 0x12, 0x00, 0x00], 2 -> 12.34
pub fn bcd_to_f64(data: &[u8], decimals: u8) -> Result<f64> {
    let int_value = bcd_to_u64(data)?;
    let divisor = 10f64.powi(decimals as i32);
    Ok(int_value as f64 / divisor)
}

/// 将浮点数转换为 BCD 编码（带小数位）
///
/// 例如: 12.34, 2, 4 -> [0x34, 0x12, 0x00, 0x00]
pub fn f64_to_bcd(value: f64, decimals: u8, len: usize) -> Vec<u8> {
    let multiplier = 10f64.powi(decimals as i32);
    let int_value = (value * multiplier).round() as u64;
    u64_to_bcd(int_value, len)
}

/// 将二进制整数（小端）转换为 u64
///
/// 例如: [0x34, 0x12, 0x00, 0x00] -> 0x1234
pub fn bin_to_u64(data: &[u8]) -> u64 {
    let mut result = 0u64;
    for (i, &byte) in data.iter().enumerate() {
        result |= (byte as u64) << (i * 8);
    }
    result
}

/// 将 u64 转换为二进制整数（小端）
///
/// 例如: 0x1234, 4 -> [0x34, 0x12, 0x00, 0x00]
pub fn u64_to_bin(value: u64, len: usize) -> Vec<u8> {
    let mut result = vec![0u8; len];
    for i in 0..len {
        result[i] = ((value >> (i * 8)) & 0xFF) as u8;
    }
    result
}

/// 解析 DL/T 645-2007 时间格式（7字节 BCD）
///
/// 格式: 秒 分 时 日 月 年(低) 年(高)
/// 例如: [0x30, 0x45, 0x12, 0x05, 0x08, 0x26, 0x20] -> 2026-08-05 12:45:30
pub fn parse_datetime(data: &[u8]) -> Result<DateTime<Utc>> {
    if data.len() != 7 {
        return Err(MeterError::InvalidDataLength {
            expected: 7,
            actual: data.len(),
        });
    }

    let second = bcd_to_u64(&data[0..1])? as u32;
    let minute = bcd_to_u64(&data[1..2])? as u32;
    let hour = bcd_to_u64(&data[2..3])? as u32;
    let day = bcd_to_u64(&data[3..4])? as u32;
    let month = bcd_to_u64(&data[4..5])? as u32;
    let year_low = bcd_to_u64(&data[5..6])? as i32;
    let year_high = bcd_to_u64(&data[6..7])? as i32;
    let year = year_high * 100 + year_low;

    // 验证范围
    if second > 59 || minute > 59 || hour > 23 || day < 1 || day > 31 || month < 1 || month > 12 {
        return Err(MeterError::ParseError(format!(
            "Invalid datetime: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            year, month, day, hour, minute, second
        )));
    }

    let naive = NaiveDateTime::parse_from_str(
        &format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            year, month, day, hour, minute, second
        ),
        "%Y-%m-%d %H:%M:%S",
    )
    .map_err(|e| MeterError::ParseError(format!("DateTime parse error: {}", e)))?;

    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}

/// 格式化 DateTime 为 DL/T 645-2007 时间格式（7字节 BCD）
pub fn format_datetime(dt: &DateTime<Utc>) -> Vec<u8> {
    let time = dt.time();
    let date = dt.date_naive();

    vec![
        u64_to_bcd(time.second() as u64, 1)[0],
        u64_to_bcd(time.minute() as u64, 1)[0],
        u64_to_bcd(time.hour() as u64, 1)[0],
        u64_to_bcd(date.day() as u64, 1)[0],
        u64_to_bcd(date.month() as u64, 1)[0],
        u64_to_bcd((date.year() % 100) as u64, 1)[0],
        u64_to_bcd((date.year() / 100) as u64, 1)[0],
    ]
}

/// 解析 BCD 地址（6字节）为字符串
///
/// 例如: [0x01, 0x00, 0x00, 0x00, 0x00, 0x00] -> "000000000001"
pub fn format_address(addr: &[u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        addr[5], addr[4], addr[3], addr[2], addr[1], addr[0]
    )
}

/// 解析字符串地址为 BCD（6字节）
///
/// 例如: "000000000001" -> [0x01, 0x00, 0x00, 0x00, 0x00, 0x00]
pub fn parse_address(addr_str: &str) -> Result<[u8; 6]> {
    if addr_str.len() != 12 {
        return Err(MeterError::ParseError(format!(
            "Invalid address length: expected 12, got {}",
            addr_str.len()
        )));
    }

    let mut result = [0u8; 6];
    for i in 0..6 {
        let hex_str = &addr_str[(10 - i * 2)..(12 - i * 2)];
        result[i] = u8::from_str_radix(hex_str, 16)
            .map_err(|e| MeterError::ParseError(format!("Invalid hex digit: {}", e)))?;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn test_bcd_to_u64() {
        assert_eq!(bcd_to_u64(&[0x34, 0x12]).unwrap(), 1234);
        assert_eq!(bcd_to_u64(&[0x67, 0x45, 0x23, 0x01]).unwrap(), 1234567);
        assert_eq!(bcd_to_u64(&[0x00]).unwrap(), 0);
    }

    #[test]
    fn test_u64_to_bcd() {
        assert_eq!(u64_to_bcd(1234, 2), vec![0x34, 0x12]);
        assert_eq!(u64_to_bcd(1234567, 4), vec![0x67, 0x45, 0x23, 0x01]);
        assert_eq!(u64_to_bcd(0, 1), vec![0x00]);
    }

    #[test]
    fn test_bcd_roundtrip() {
        let values = [0, 1, 12, 123, 1234, 12345, 123456, 1234567];
        for &val in &values {
            let len = if val == 0 {
                1
            } else {
                ((val as f64).log10() as usize / 2) + 1
            };
            let bcd = u64_to_bcd(val, len);
            assert_eq!(bcd_to_u64(&bcd).unwrap(), val);
        }
    }

    #[test]
    fn test_bcd_to_f64() {
        assert_eq!(bcd_to_f64(&[0x34, 0x12, 0x00, 0x00], 2).unwrap(), 12.34);
        assert_eq!(bcd_to_f64(&[0x50, 0x12], 2).unwrap(), 12.50);
    }

    #[test]
    fn test_f64_to_bcd() {
        assert_eq!(f64_to_bcd(12.34, 2, 4), vec![0x34, 0x12, 0x00, 0x00]);
        assert_eq!(f64_to_bcd(12.50, 2, 2), vec![0x50, 0x12]);
    }

    #[test]
    fn test_bin_to_u64() {
        assert_eq!(bin_to_u64(&[0x34, 0x12, 0x00, 0x00]), 0x1234);
        assert_eq!(bin_to_u64(&[0xFF, 0x00]), 0xFF);
    }

    #[test]
    fn test_u64_to_bin() {
        assert_eq!(u64_to_bin(0x1234, 4), vec![0x34, 0x12, 0x00, 0x00]);
        assert_eq!(u64_to_bin(0xFF, 2), vec![0xFF, 0x00]);
    }

    #[test]
    fn test_parse_datetime() {
        // 2026-08-05 12:45:30
        let data = [0x30, 0x45, 0x12, 0x05, 0x08, 0x26, 0x20];
        let dt = parse_datetime(&data).unwrap();

        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 8);
        assert_eq!(dt.day(), 5);
        assert_eq!(dt.hour(), 12);
        assert_eq!(dt.minute(), 45);
        assert_eq!(dt.second(), 30);
    }

    #[test]
    fn test_format_datetime() {
        let dt_str = "2026-08-05 12:45:30";
        let naive = NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%d %H:%M:%S").unwrap();
        let dt = DateTime::from_naive_utc_and_offset(naive, Utc);

        let data = format_datetime(&dt);
        assert_eq!(data, vec![0x30, 0x45, 0x12, 0x05, 0x08, 0x26, 0x20]);
    }

    #[test]
    fn test_format_address() {
        let addr = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(format_address(&addr), "000000000001");

        let addr2 = [0x34, 0x12, 0x56, 0x78, 0x9A, 0xBC];
        assert_eq!(format_address(&addr2), "BC9A78561234");
    }

    #[test]
    fn test_parse_address() {
        let result = parse_address("000000000001").unwrap();
        assert_eq!(result, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let result2 = parse_address("BC9A78561234").unwrap();
        assert_eq!(result2, [0x34, 0x12, 0x56, 0x78, 0x9A, 0xBC]);
    }

    #[test]
    fn test_address_roundtrip() {
        let addresses = ["000000000001", "123456789012", "AABBCCDDEEFF"];

        for addr_str in &addresses {
            let addr = parse_address(addr_str).unwrap();
            let formatted = format_address(&addr);
            assert_eq!(formatted, *addr_str);
        }
    }
}
