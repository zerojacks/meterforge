// BCD 编码辅助函数

use chrono::{Datelike, Timelike};

// ============================================
// BCD 编码函数
// ============================================

/// 编码电压 (XXX.X V, 2字节)
pub(super) fn encode_bcd_voltage(voltage: f64) -> Vec<u8> {
    encode_bcd(voltage, 2, 1)
}

/// 编码电流 (XXX.XXX A, 3字节)
pub(super) fn encode_bcd_current(current: f64) -> Vec<u8> {
    encode_bcd(current, 3, 3)
}

/// 编码功率 (XX.XXXX kW, 3字节)
pub(super) fn encode_bcd_power(power: f64) -> Vec<u8> {
    encode_bcd(power, 3, 4)
}

/// 编码功率因数 (X.XXX, 2字节)
pub(super) fn encode_bcd_power_factor(pf: f64) -> Vec<u8> {
    encode_bcd(pf, 2, 3)
}

/// 编码电能 (XXXXXX.XX kWh, 4字节)
pub(super) fn encode_bcd_energy(energy: f64) -> Vec<u8> {
    encode_bcd(energy, 4, 2)
}

/// 通用 BCD 编码
pub(super) fn encode_bcd(value: f64, bytes: usize, decimals: usize) -> Vec<u8> {
    // 转换为整数 (乘以 10^decimals)
    let multiplier = 10_f64.powi(decimals as i32);
    let int_value = (value * multiplier).round() as u64;

    let mut result = vec![0u8; bytes];
    let mut temp = int_value;

    for i in 0..bytes {
        let low = (temp % 10) as u8;
        temp /= 10;
        let high = (temp % 10) as u8;
        temp /= 10;

        result[i] = (high << 4) | low;
    }

    result
}

/// 编码日期时间 (ss mm hh DD MM YY, 6字节 BCD)
pub(super) fn encode_datetime(dt: &chrono::DateTime<chrono::Local>) -> Vec<u8> {
    vec![
        to_bcd(dt.second() as u8),
        to_bcd(dt.minute() as u8),
        to_bcd(dt.hour() as u8),
        to_bcd(dt.day() as u8),
        to_bcd(dt.month() as u8),
        to_bcd((dt.year() % 100) as u8),
    ]
}

/// 单字节 BCD 编码
pub(super) fn to_bcd(value: u8) -> u8 {
    let high = value / 10;
    let low = value % 10;
    (high << 4) | low
}

/// 单字节 BCD 转十进制
pub(super) fn bcd_to_decimal(bcd: u8) -> u8 {
    let high = (bcd >> 4) & 0x0F;
    let low = bcd & 0x0F;
    high * 10 + low
}
