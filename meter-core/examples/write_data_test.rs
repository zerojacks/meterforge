// 写数据命令测试
// 测试虚拟电表的写数据功能

use meter_core::VirtualMeter;

fn main() {
    println!("========================================");
    println!("DL/T645 写数据命令测试");
    println!("========================================\n");

    let mut meter = VirtualMeter::default();
    println!("✓ 虚拟电表已创建");
    println!("  初始地址: 123456789012\n");

    // 测试 1: 修改电表地址
    test_write_address(&mut meter);

    // 测试 2: 修改通信波特率
    test_write_baudrate(&mut meter);

    // 测试 3: 修改电表常数
    test_write_meter_constant(&mut meter);

    // 测试 4: 密码保护
    test_password_protection(&mut meter);

    println!("\n✓ 所有写数据测试完成！");
}

/// 测试修改电表地址
fn test_write_address(meter: &mut VirtualMeter) {
    println!("【测试 1: 修改电表地址】");

    // 读取当前地址
    let di = [0x02, 0x01, 0x00, 0x04];
    match meter.handle_read_command(di) {
        Ok(data) => {
            println!(
                "  当前地址: {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                data[5], data[4], data[3], data[2], data[1], data[0]
            );
        }
        Err(e) => println!("  读取失败: {}", e),
    }

    // 构造写数据: DI(4) + 密码(4) + 操作者代码(4) + 新地址(6)
    let mut write_data = Vec::new();

    // DI: 04-00-01-02
    write_data.extend(&[0x02, 0x01, 0x00, 0x04]);

    // 密码: 00000000
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]);

    // 操作者代码: 00000000
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]);

    // 新地址: 999888777666 → [0x66, 0x67, 0x77, 0x88, 0x98, 0x99]
    write_data.extend(&[0x66, 0x67, 0x77, 0x88, 0x98, 0x99]);

    // 执行写入
    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("  ✓ 写入成功"),
        Err(e) => println!("  ✗ 写入失败: {}", e),
    }

    // 验证修改
    match meter.handle_read_command(di) {
        Ok(data) => {
            println!(
                "  新地址: {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                data[5], data[4], data[3], data[2], data[1], data[0]
            );
        }
        Err(e) => println!("  验证失败: {}", e),
    }

    println!();
}

/// 测试修改通信波特率
fn test_write_baudrate(meter: &mut VirtualMeter) {
    println!("【测试 2: 修改通信波特率】");

    // 读取当前波特率
    let di = [0x01, 0x04, 0x00, 0x04];
    match meter.handle_read_command(di) {
        Ok(data) => {
            println!("  当前波特率编码: 0x{:02X}", data[0]);
        }
        Err(e) => println!("  读取失败: {}", e),
    }

    // 构造写数据
    let mut write_data = Vec::new();
    write_data.extend(&[0x01, 0x04, 0x00, 0x04]); // DI
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 密码
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 操作者代码
    write_data.push(0x0C); // 新波特率: 9600 bps (编码为 0x0C)

    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("  ✓ 写入成功"),
        Err(e) => println!("  ✗ 写入失败: {}", e),
    }

    // 验证
    match meter.handle_read_command(di) {
        Ok(data) => {
            println!("  新波特率编码: 0x{:02X}", data[0]);
        }
        Err(e) => println!("  验证失败: {}", e),
    }

    println!();
}

/// 测试修改电表常数
fn test_write_meter_constant(meter: &mut VirtualMeter) {
    println!("【测试 3: 修改电表常数】");

    // 读取当前电表常数
    let di = [0x02, 0x04, 0x00, 0x04];
    match meter.handle_read_command(di) {
        Ok(data) => {
            let constant = decode_bcd(&data);
            println!("  当前电表常数: {} imp/kWh", constant);
        }
        Err(e) => println!("  读取失败: {}", e),
    }

    // 构造写数据
    let mut write_data = Vec::new();
    write_data.extend(&[0x02, 0x04, 0x00, 0x04]); // DI
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 密码
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 操作者代码

    // 新常数: 3200 → BCD: [0x00, 0x32, 0x00]
    write_data.extend(&[0x00, 0x32, 0x00]);

    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("  ✓ 写入成功"),
        Err(e) => println!("  ✗ 写入失败: {}", e),
    }

    // 验证
    match meter.handle_read_command(di) {
        Ok(data) => {
            let constant = decode_bcd(&data);
            println!("  新电表常数: {} imp/kWh", constant);
        }
        Err(e) => println!("  验证失败: {}", e),
    }

    println!();
}

/// 测试密码保护
fn test_password_protection(meter: &mut VirtualMeter) {
    println!("【测试 4: 密码保护】");

    // 先设置一个密码
    println!("  步骤 1: 设置密码");
    let mut write_data = Vec::new();
    write_data.extend(&[0x04, 0x07, 0x00, 0x04]); // DI: 密码
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 当前密码 (默认)
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 操作者代码
    write_data.extend(&[0x12, 0x34, 0x56, 0x78]); // 新密码: 12345678

    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("    ✓ 密码设置成功"),
        Err(e) => println!("    ✗ 密码设置失败: {}", e),
    }

    // 尝试用错误密码修改地址
    println!("\n  步骤 2: 用错误密码尝试修改地址");
    let mut write_data = Vec::new();
    write_data.extend(&[0x02, 0x01, 0x00, 0x04]); // DI: 地址
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 错误密码
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 操作者代码
    write_data.extend(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]); // 新地址

    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("    ✗ 不应该成功！"),
        Err(e) => println!("    ✓ 正确拒绝: {}", e),
    }

    // 用正确密码修改地址
    println!("\n  步骤 3: 用正确密码修改地址");
    let mut write_data = Vec::new();
    write_data.extend(&[0x02, 0x01, 0x00, 0x04]); // DI: 地址
    write_data.extend(&[0x12, 0x34, 0x56, 0x78]); // 正确密码
    write_data.extend(&[0x00, 0x00, 0x00, 0x00]); // 操作者代码
    write_data.extend(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]); // 新地址

    match meter.handle_write_command(&write_data) {
        Ok(_) => println!("    ✓ 修改成功"),
        Err(e) => println!("    ✗ 修改失败: {}", e),
    }

    // 验证地址已修改
    let di = [0x02, 0x01, 0x00, 0x04];
    match meter.handle_read_command(di) {
        Ok(data) => {
            println!(
                "    最终地址: {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                data[5], data[4], data[3], data[2], data[1], data[0]
            );
        }
        Err(e) => println!("    验证失败: {}", e),
    }

    println!();
}

/// 解码 BCD 数据
fn decode_bcd(data: &[u8]) -> u32 {
    let mut value = 0u32;

    for &byte in data.iter().rev() {
        let high = ((byte >> 4) & 0x0F) as u32;
        let low = (byte & 0x0F) as u32;
        value = value * 100 + high * 10 + low;
    }

    value
}
