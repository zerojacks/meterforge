// Transport 层集成测试
//
// 测试 TCP 客户端 → Router → MeterActor → 响应 全流程

use meter_core::actor::{MeterActor, MeterActorConfig, MeterActorHandle, MeterRegistry};
use meter_core::protocol::{decode_frame, encode_frame, Frame};
use meter_core::simulation::{VirtualMeter, VirtualMeterConfig};
use tokio::sync::{broadcast, mpsc};

/// 测试辅助：创建并启动一个电表Actor
#[allow(dead_code)]
async fn setup_meter_actor(address: [u8; 6]) -> (MeterActorHandle, tokio::task::JoinHandle<()>) {
    let meter_config = VirtualMeterConfig {
        address,
        ..Default::default()
    };

    let meter = VirtualMeter::new(meter_config);
    let (cmd_tx, cmd_rx) = mpsc::channel(100);

    // 创建一个实际的tick广播通道
    use meter_core::actor::TickMsg;
    let (_real_tick_tx, real_tick_rx) = broadcast::channel::<TickMsg>(32);

    let actor_config = MeterActorConfig {
        address,
        ..Default::default()
    };

    let actor = MeterActor::new(meter, real_tick_rx, cmd_rx, actor_config);
    let handle = MeterActorHandle::new(cmd_tx, address);

    let actor_handle = tokio::spawn(async move {
        actor.run().await;
    });

    (handle, actor_handle)
}

#[tokio::test]
#[ignore] // 暂时忽略，需要完整的Router集成
async fn test_tcp_client_to_meter_basic() {
    println!("\n========== TCP 客户端基础通信测试 ==========\n");
    println!("注意：此测试需要完整的Router运行循环，当前被忽略");
    println!("TODO: 实现RouterRunner的并发连接处理");

    // 该测试需要以下组件完整集成：
    // 1. TcpChannel 运行监听
    // 2. RouterRunner 处理新连接
    // 3. MeterActor 处理协议命令
    // 4. 整个异步流程正确连接

    // 当前架构问题：Router 拥有 MeterRegistry，无法并发处理多个连接
    // 解决方案：使用 Arc<Mutex<MeterRegistry>> 或重新设计架构
}

#[tokio::test]
async fn test_frame_codec_roundtrip() {
    println!("\n========== 帧编解码往返测试 ==========\n");

    let address = [0x12, 0x34, 0x56, 0x78, 0x90, 0x12];
    let di = [0x00, 0x01, 0x00, 0x00];

    // 1. 创建请求帧
    let request = Frame::read(address, di);
    println!("原始请求帧:");
    println!("  地址: {:02X?}", request.address);
    println!("  控制码: 0x{:02X}", request.control);
    println!("  数据: {:02X?}", request.data);

    // 2. 编码
    let encoded = encode_frame(&request);
    println!("\n编码后: {} bytes", encoded.len());
    println!("  {:02X?}", encoded);

    // 3. 解码
    let decoded = decode_frame(&encoded).unwrap();
    println!("\n解码后:");
    println!("  地址: {:02X?}", decoded.address);
    println!("  控制码: 0x{:02X}", decoded.control);
    println!("  数据: {:02X?}", decoded.data);

    // 4. 验证
    assert_eq!(decoded.address, request.address);
    assert_eq!(decoded.control, request.control);
    assert_eq!(decoded.data, request.data);

    println!("\n✓ 帧编解码往返测试通过\n");
}

#[tokio::test]
async fn test_multiple_frames() {
    println!("\n========== 多帧连续发送测试 ==========\n");

    let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];

    // 测试多个DI
    let dis = vec![
        [0x00, 0x01, 0x00, 0x00], // 正向有功电能
        [0x00, 0x02, 0x00, 0x00], // 反向有功电能
        [0x01, 0x01, 0x00, 0x00], // A相电压
        [0x02, 0x01, 0x00, 0x00], // A相电流
    ];

    for (i, di) in dis.iter().enumerate() {
        let frame = Frame::read(address, *di);
        let bytes = encode_frame(&frame);

        println!(
            "帧 #{}: DI={:02X}{:02X}{:02X}{:02X}, {} bytes",
            i + 1,
            di[0],
            di[1],
            di[2],
            di[3],
            bytes.len()
        );

        // 验证可以正确解码
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(decoded.data[0..4], *di);
    }

    println!("\n✓ 多帧连续发送测试通过\n");
}

#[tokio::test]
async fn test_invalid_frame_handling() {
    println!("\n========== 无效帧处理测试 ==========\n");

    // 1. 帧太短
    let short_frame = vec![0x68, 0x01, 0x02];
    match decode_frame(&short_frame) {
        Err(_) => println!("✓ 正确拒绝过短的帧"),
        Ok(_) => panic!("不应该接受过短的帧"),
    }

    // 2. 校验和错误
    let bad_checksum = vec![
        0x68, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x68, 0x11, 0x04, 0x33, 0x34, 0x33, 0x33,
        0xFF, // 错误的校验和
        0x16,
    ];
    match decode_frame(&bad_checksum) {
        Err(_) => println!("✓ 正确检测校验和错误"),
        Ok(_) => panic!("不应该接受错误的校验和"),
    }

    // 3. 缺少结束符
    let no_end = vec![
        0x68, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x68, 0x11, 0x00, 0x1F, // 校验和
        0xFF, // 错误的结束符
    ];
    match decode_frame(&no_end) {
        Err(_) => println!("✓ 正确检测缺失的结束符"),
        Ok(_) => panic!("不应该接受错误的结束符"),
    }

    println!("\n✓ 无效帧处理测试通过\n");
}

#[tokio::test]
async fn test_registry_address_matching() {
    println!("\n========== 地址匹配测试 ==========\n");

    let mut registry = MeterRegistry::new();

    // 注册3个电表
    let addresses = vec![
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x07],
        [0x01, 0x02, 0xFF, 0x04, 0x05, 0x08],
    ];

    for addr in &addresses {
        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let handle = MeterActorHandle::new(cmd_tx, *addr);
        registry.register(*addr, handle).unwrap();
    }

    println!("✓ 已注册 {} 个电表", registry.count());

    // 测试精确匹配
    assert!(registry.get(&addresses[0]).is_some());
    println!("✓ 精确匹配成功");

    // 测试通配匹配
    let pattern = [0x01, 0x02, 0x03, 0x04, 0x05, 0xAA];
    let matches = registry.find_wildcard(&pattern);
    assert_eq!(matches.len(), 2); // 匹配前两个
    println!("✓ 通配匹配成功: 找到 {} 个电表", matches.len());

    // 测试全通配
    let pattern_all = [0x01, 0x02, 0xAA, 0xAA, 0xAA, 0xAA];
    let matches_all = registry.find_wildcard(&pattern_all);
    assert_eq!(matches_all.len(), 3); // 全部匹配
    println!("✓ 全通配匹配成功: 找到 {} 个电表", matches_all.len());

    println!("\n✓ 地址匹配测试通过\n");
}
