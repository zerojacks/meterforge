// 虚拟电表演示程序
// 展示如何使用基于物理模型的虚拟电表

use meter_core::{LoadProfile, PhysicsConfig, VirtualMeter, VirtualMeterConfig};

fn main() {
    println!("========================================");
    println!("虚拟 DL/T645-2007 电表演示");
    println!("========================================\n");

    // 创建默认虚拟电表
    let mut meter = VirtualMeter::default();
    println!("✓ 虚拟电表已创建");
    println!("  地址: 123456789012\n");

    // 测试各类数据读取
    test_voltage(&mut meter);
    test_current(&mut meter);
    test_power(&mut meter);
    test_power_factor(&mut meter);
    test_energy(&mut meter);
    test_demand(&mut meter);

    println!("\n========================================");
    println!("物理关系验证");
    println!("========================================");
    verify_physics(&mut meter);

    println!("\n========================================");
    println!("时间演进测试");
    println!("========================================");
    test_time_evolution(&mut meter);

    println!("\n✓ 所有测试完成！");
}

/// 测试电压读取
fn test_voltage(meter: &mut VirtualMeter) {
    println!("【电压测试】");

    // A相电压 DI: 02-01-01-00 → [0x00, 0x01, 0x01, 0x02]
    let di = [0x00, 0x01, 0x01, 0x02];
    match meter.handle_read_command(di) {
        Ok(data) => {
            let voltage = decode_bcd_voltage(&data);
            println!("  A相电压: {:.1} V", voltage);
        }
        Err(e) => println!("  ✗ 错误: {}", e),
    }

    // B相电压
    let di = [0x00, 0x02, 0x01, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let voltage = decode_bcd_voltage(&data);
        println!("  B相电压: {:.1} V", voltage);
    }

    // C相电压
    let di = [0x00, 0x03, 0x01, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let voltage = decode_bcd_voltage(&data);
        println!("  C相电压: {:.1} V", voltage);
    }

    println!();
}

/// 测试电流读取
fn test_current(meter: &mut VirtualMeter) {
    println!("【电流测试】");

    let di = [0x00, 0x01, 0x02, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let current = decode_bcd_current(&data);
        println!("  A相电流: {:.3} A", current);
    }

    let di = [0x00, 0x02, 0x02, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let current = decode_bcd_current(&data);
        println!("  B相电流: {:.3} A", current);
    }

    let di = [0x00, 0x03, 0x02, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let current = decode_bcd_current(&data);
        println!("  C相电流: {:.3} A", current);
    }

    println!();
}

/// 测试功率读取
fn test_power(meter: &mut VirtualMeter) {
    println!("【功率测试】");

    // 瞬时总有功功率
    let di = [0x00, 0x00, 0x03, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let power = decode_bcd_power(&data);
        println!("  瞬时总有功功率: {:.4} kW", power);
    }

    // A相有功功率
    let di = [0x00, 0x01, 0x03, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let power = decode_bcd_power(&data);
        println!("  A相有功功率: {:.4} kW", power);
    }

    // 瞬时总无功功率
    let di = [0x00, 0x00, 0x04, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let power = decode_bcd_power(&data);
        println!("  瞬时总无功功率: {:.4} kvar", power);
    }

    // 瞬时总视在功率
    let di = [0x00, 0x00, 0x05, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let power = decode_bcd_power(&data);
        println!("  瞬时总视在功率: {:.4} kVA", power);
    }

    println!();
}

/// 测试功率因数
fn test_power_factor(meter: &mut VirtualMeter) {
    println!("【功率因数测试】");

    let di = [0x00, 0x00, 0x06, 0x02];
    if let Ok(data) = meter.handle_read_command(di) {
        let pf = decode_bcd_power_factor(&data);
        println!("  总功率因数: {:.3}", pf);
    }

    println!();
}

/// 测试电能读取
fn test_energy(meter: &mut VirtualMeter) {
    println!("【电能测试】");

    // 正向有功总电能
    let di = [0x00, 0x00, 0x01, 0x00];
    if let Ok(data) = meter.handle_read_command(di) {
        let energy = decode_bcd_energy(&data);
        println!("  正向有功总电能: {:.2} kWh", energy);
    }

    // 读取费率数参数
    let di = [0x04, 0x02, 0x00, 0x04];
    let num_rates = if let Ok(data) = meter.handle_read_command(di) {
        let bcd = data[0];
        ((bcd >> 4) * 10 + (bcd & 0x0F)) as u8
    } else {
        4 // 默认4费率
    };

    println!("  配置的费率数: {}", num_rates);

    // 读取各费率电能
    for rate in 1..=num_rates {
        // 根据费率号构造 DI (费率1 → DI1=0x01, 费率2 → DI1=0x02, ...)
        let di = [0x00, rate, 0x01, 0x00];
        if let Ok(data) = meter.handle_read_command(di) {
            let energy = decode_bcd_energy(&data);
            println!("  费率{}电能: {:.2} kWh", rate, energy);
        }
    }

    println!();
}

/// 测试最大需量
fn test_demand(meter: &mut VirtualMeter) {
    println!("【最大需量测试】");

    let di = [0x00, 0x00, 0x01, 0x01];
    if let Ok(data) = meter.handle_read_command(di) {
        if data.len() >= 3 {
            let demand = decode_bcd_power(&data[0..3]);
            println!("  正向有功总最大需量: {:.4} kW", demand);

            if data.len() >= 8 {
                let time_bytes = &data[3..9];
                println!(
                    "  发生时间: {:02X}-{:02X}-{:02X} {:02X}:{:02X}:{:02X}",
                    time_bytes[5],
                    time_bytes[4],
                    time_bytes[3],
                    time_bytes[2],
                    time_bytes[1],
                    time_bytes[0]
                );
            }
        }
    }

    println!();
}

/// 验证物理关系
fn verify_physics(meter: &mut VirtualMeter) {
    // 读取 U, I, P, cosφ
    let va = meter
        .handle_read_command([0x00, 0x01, 0x01, 0x02])
        .map(|d| decode_bcd_voltage(&d))
        .unwrap_or(0.0);

    let ia = meter
        .handle_read_command([0x00, 0x01, 0x02, 0x02])
        .map(|d| decode_bcd_current(&d))
        .unwrap_or(0.0);

    let pa = meter
        .handle_read_command([0x00, 0x01, 0x03, 0x02])
        .map(|d| decode_bcd_power(&d))
        .unwrap_or(0.0);

    let pf = meter
        .handle_read_command([0x00, 0x00, 0x06, 0x02])
        .map(|d| decode_bcd_power_factor(&d))
        .unwrap_or(0.0);

    println!("  实测值:");
    println!("    U = {:.1} V", va);
    println!("    I = {:.3} A", ia);
    println!("    P = {:.4} kW", pa);
    println!("    cosφ = {:.3}", pf);

    // 计算理论值 P = U × I × cosφ / 1000
    let p_calculated = va * ia * pf / 1000.0;
    println!("\n  理论计算:");
    println!("    P = U × I × cosφ / 1000");
    println!("    P = {:.1} × {:.3} × {:.3} / 1000", va, ia, pf);
    println!("    P = {:.4} kW", p_calculated);

    let error = ((pa - p_calculated).abs() / p_calculated * 100.0);
    println!("\n  误差: {:.2}%", error);

    if error < 1.0 {
        println!("  ✓ 物理关系验证通过！");
    } else {
        println!("  ⚠ 误差较大");
    }
}

/// 测试时间演进
fn test_time_evolution(meter: &mut VirtualMeter) {
    println!("  监测5秒内电能变化...\n");

    let di = [0x00, 0x00, 0x01, 0x00];

    let e0 = meter
        .handle_read_command(di)
        .map(|d| decode_bcd_energy(&d))
        .unwrap_or(0.0);
    println!("  t=0s  电能: {:.2} kWh", e0);

    std::thread::sleep(std::time::Duration::from_secs(1));
    let e1 = meter
        .handle_read_command(di)
        .map(|d| decode_bcd_energy(&d))
        .unwrap_or(0.0);
    println!("  t=1s  电能: {:.2} kWh  (+{:.6} kWh)", e1, e1 - e0);

    std::thread::sleep(std::time::Duration::from_secs(2));
    let e2 = meter
        .handle_read_command(di)
        .map(|d| decode_bcd_energy(&d))
        .unwrap_or(0.0);
    println!("  t=3s  电能: {:.2} kWh  (+{:.6} kWh)", e2, e2 - e1);

    std::thread::sleep(std::time::Duration::from_secs(2));
    let e3 = meter
        .handle_read_command(di)
        .map(|d| decode_bcd_energy(&d))
        .unwrap_or(0.0);
    println!("  t=5s  电能: {:.2} kWh  (+{:.6} kWh)", e3, e3 - e2);

    let total_delta = e3 - e0;
    println!("\n  总增量: {:.6} kWh", total_delta);

    if total_delta > 0.0 {
        println!("  ✓ 电能正常累积！");
    }
}

// ============================================
// BCD 解码函数
// ============================================

fn decode_bcd_voltage(data: &[u8]) -> f64 {
    decode_bcd(data, 1)
}

fn decode_bcd_current(data: &[u8]) -> f64 {
    decode_bcd(data, 3)
}

fn decode_bcd_power(data: &[u8]) -> f64 {
    decode_bcd(data, 4)
}

fn decode_bcd_power_factor(data: &[u8]) -> f64 {
    decode_bcd(data, 3)
}

fn decode_bcd_energy(data: &[u8]) -> f64 {
    decode_bcd(data, 2)
}

fn decode_bcd(data: &[u8], decimals: usize) -> f64 {
    let mut value = 0u64;

    for &byte in data.iter().rev() {
        let high = (byte >> 4) & 0x0F;
        let low = byte & 0x0F;
        value = value * 100 + (high * 10 + low) as u64;
    }

    value as f64 / 10_f64.powi(decimals as i32)
}
