// Router - 协议帧路由器
//
// 按设计方案 4.3 节实现：
// 职责：
// 1. 从 Transport 层接收原始帧
// 2. 解码帧并校验
// 3. 根据地址路由到 MeterRegistry
// 4. 将响应回传到 Transport

use crate::actor::MeterRegistry;
use crate::protocol::{decode_frame, Frame};
use crate::transport::{FrameSource, RawFrame};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Router 配置
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// 工作线程数（处理连接的并发数）
    pub worker_threads: usize,

    /// 帧队列容量
    pub frame_queue_capacity: usize,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            worker_threads: 4,
            frame_queue_capacity: 1000,
        }
    }
}

/// Router 主结构
pub struct Router {
    /// 电表注册表（共享）
    registry: Arc<Mutex<MeterRegistry>>,

    /// 配置
    config: RouterConfig,
}

impl Router {
    /// 创建新的 Router
    pub fn new(registry: Arc<Mutex<MeterRegistry>>, config: RouterConfig) -> Self {
        Self { registry, config }
    }

    /// 从 MeterRegistry 创建 Router（便利方法）
    pub fn from_registry(registry: MeterRegistry, config: RouterConfig) -> Self {
        Self {
            registry: Arc::new(Mutex::new(registry)),
            config,
        }
    }

    /// 获取共享的 Registry
    pub fn registry(&self) -> Arc<Mutex<MeterRegistry>> {
        Arc::clone(&self.registry)
    }

    /// 处理单个 FrameSource（一个 TCP 连接或串口）
    ///
    /// 这个方法会循环读取帧，直到连接关闭
    pub async fn handle_frame_source(&mut self, mut source: Box<dyn FrameSource>) {
        let desc = source.description();
        info!("[Router] Handling new frame source: {}", desc);

        loop {
            match source.next_frame().await {
                Some(raw_frame) => {
                    debug!(
                        "[Router] Received frame from {}: {} bytes",
                        desc,
                        raw_frame.bytes.len()
                    );

                    // 处理帧
                    self.process_raw_frame(raw_frame).await;
                }
                None => {
                    info!("[Router] Frame source closed: {}", desc);
                    break;
                }
            }
        }
    }

    /// 处理原始帧
    async fn process_raw_frame(&mut self, raw_frame: RawFrame) {
        let total_start = Instant::now();
        info!(
            "[Router] begin processing raw frame: conn_id={}, bytes_len={}, preview={:02X?}",
            raw_frame.conn_id,
            raw_frame.bytes.len(),
            &raw_frame.bytes[..raw_frame.bytes.len().min(16)]
        );

        // 1. 解码帧
        let decode_start = Instant::now();
        let frame = match decode_frame(&raw_frame.bytes) {
            Ok(f) => f,
            Err(e) => {
                error!(
                    "[Router] decode failed: conn_id={}, bytes_len={}, error={}, decode_elapsed={:?}",
                    raw_frame.conn_id,
                    raw_frame.bytes.len(),
                    e,
                    decode_start.elapsed()
                );
                // 帧格式错误，静默丢弃（符合协议规范）
                return;
            }
        };

        debug!(
            "[Router] Decoded frame: address={:02X}{:02X}{:02X}{:02X}{:02X}{:02X}, control=0x{:02X}",
            frame.address[0], frame.address[1], frame.address[2],
            frame.address[3], frame.address[4], frame.address[5],
            frame.control
        );
        info!(
            "[Router] decode ok: conn_id={}, address={:02X}{:02X}{:02X}{:02X}{:02X}{:02X}, control=0x{:02X}, data_len={}, decode_elapsed={:?}",
            raw_frame.conn_id,
            frame.address[0], frame.address[1], frame.address[2],
            frame.address[3], frame.address[4], frame.address[5],
            frame.control,
            frame.data.len(),
            decode_start.elapsed()
        );

        // 2. 获取注册表锁并路由到目标电表
        let route_start = Instant::now();
        let mut registry = self.registry.lock().await;
        match registry
            .route_frame(frame, raw_frame.conn_id, raw_frame.reply_channel)
            .await
        {
            Ok(count) => {
                debug!("[Router] Frame routed to {} meter(s)", count);
                info!(
                    "[Router] route complete: conn_id={}, matched_meter_count={}, route_elapsed={:?}, total_elapsed={:?}",
                    raw_frame.conn_id,
                    count,
                    route_start.elapsed(),
                    total_start.elapsed()
                );
            }
            Err(e) => {
                warn!(
                    "[Router] Route error: conn_id={}, error={}, route_elapsed={:?}, total_elapsed={:?}",
                    raw_frame.conn_id,
                    e,
                    route_start.elapsed(),
                    total_start.elapsed()
                );
                // 路由失败（如地址不存在），静默丢弃
            }
        }
    }
}

/// Router 运行器（管理多个连接）
pub struct RouterRunner {
    /// 共享的 Registry
    registry: Arc<Mutex<MeterRegistry>>,

    /// Router 配置
    config: RouterConfig,

    /// 连接接收器（从 TcpChannel 接收新连接）
    conn_rx: mpsc::Receiver<Box<dyn FrameSource>>,
}

impl RouterRunner {
    /// 创建新的 RouterRunner
    pub fn new(
        registry: Arc<Mutex<MeterRegistry>>,
        config: RouterConfig,
        conn_rx: mpsc::Receiver<Box<dyn FrameSource>>,
    ) -> Self {
        Self {
            registry,
            config,
            conn_rx,
        }
    }

    /// 运行 Router 主循环
    ///
    /// 不断接收新连接，为每个连接启动独立的处理任务
    pub async fn run(mut self) {
        info!("[RouterRunner] Started");

        loop {
            match self.conn_rx.recv().await {
                Some(source) => {
                    let desc = source.description();
                    info!("[RouterRunner] New connection: {}", desc);

                    // 为每个连接创建独立的 Router 实例（共享 Registry）
                    let registry = Arc::clone(&self.registry);
                    let config = self.config.clone();
                    let mut router = Router::new(registry, config);

                    // 在独立task中处理连接
                    tokio::spawn(async move {
                        router.handle_frame_source(source).await;
                        info!("[RouterRunner] Connection {} finished", desc);
                    });
                }
                None => {
                    warn!("[RouterRunner] Connection channel closed");
                    break;
                }
            }
        }

        info!("[RouterRunner] Stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{MeterActor, MeterActorConfig, MeterActorHandle, TickMsg};
    use crate::protocol::{decode_frame, encode_frame, Frame};
    use crate::simulation::{VirtualMeter, VirtualMeterConfig};
    use crate::transport::{FrameSource, RawFrame};
    use tokio::sync::{broadcast, mpsc};
    use tokio::time::Duration;

    #[test]
    fn test_router_creation() {
        let registry = MeterRegistry::new();
        let config = RouterConfig::default();
        let router = Router::from_registry(registry, config);

        // 验证可以获取共享的registry
        let _registry_ref = router.registry();
    }

    #[tokio::test]
    async fn test_router_with_registry() {
        let mut registry = MeterRegistry::new();

        // 注册一个电表
        let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let handle = MeterActorHandle::new(cmd_tx, address);
        registry.register(address, handle).unwrap();

        let config = RouterConfig::default();
        let router = Router::from_registry(registry, config);

        // 验证可以通过共享的registry访问
        let registry_ref = router.registry();
        let registry_guard = registry_ref.lock().await;
        assert_eq!(registry_guard.count(), 1);

        let addresses = registry_guard.addresses();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0], address);
    }

    /// 模拟的 FrameSource 用于测试
    struct MockFrameSource {
        frames: Vec<Vec<u8>>,
        current: usize,
        response_collector: mpsc::Sender<Vec<u8>>,
        conn_id: u64,
    }

    impl MockFrameSource {
        fn new(frames: Vec<Vec<u8>>, response_collector: mpsc::Sender<Vec<u8>>) -> Self {
            Self {
                frames,
                current: 0,
                response_collector,
                conn_id: 1,
            }
        }
    }

    #[async_trait::async_trait]
    impl FrameSource for MockFrameSource {
        async fn next_frame(&mut self) -> Option<RawFrame> {
            if self.current >= self.frames.len() {
                return None;
            }

            let bytes = self.frames[self.current].clone();
            self.current += 1;

            let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();

            // 启动任务收集响应（转发所有响应到 collector）
            let collector = self.response_collector.clone();
            tokio::spawn(async move {
                while let Some(response) = reply_rx.recv().await {
                    let _ = collector.send(response).await;
                }
            });

            Some(RawFrame {
                conn_id: self.conn_id,
                bytes,
                reply_channel: reply_tx,
            })
        }

        fn description(&self) -> String {
            "MockFrameSource".to_string()
        }
    }

    #[tokio::test]
    async fn test_end_to_end_read_voltage() {
        println!("\n=== 端到端测试：读取A相电压 ===\n");

        // 1. 创建一个虚拟电表
        let address = [0x12, 0x34, 0x56, 0x78, 0x90, 0x12];
        let meter_config = VirtualMeterConfig {
            address,
            ..Default::default()
        };
        let meter = VirtualMeter::new(meter_config);

        // 2. 创建 MeterActor
        let (_tick_tx, tick_rx) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(100);
        let actor_config = MeterActorConfig {
            address,
            cmd_queue_capacity: 100,
            enable_persistence: false,
            db_pool: None,
            registry_tx: None,
            snapshot_tx: None,
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);
        let handle = MeterActorHandle::new(cmd_tx, address);

        // 启动 Actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // 3. 创建 MeterRegistry 并注册电表
        let mut registry = MeterRegistry::new();
        registry.register(address, handle).unwrap();
        let registry = Arc::new(Mutex::new(registry));

        // 4. 创建 Router
        let config = RouterConfig::default();
        let mut router = Router::new(Arc::clone(&registry), config);

        // 5. 构造读电压命令帧（DI = 02-01-01-00，A相电压）
        let di = [0x00, 0x01, 0x01, 0x02];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读电压命令帧: {:02X?}", frame_bytes);

        // 6. 创建 MockFrameSource
        let (response_tx, mut response_rx) = mpsc::channel(10);
        let mock_source = MockFrameSource::new(vec![frame_bytes.clone()], response_tx);

        // 7. 处理帧（在独立任务中）
        tokio::spawn(async move {
            router.handle_frame_source(Box::new(mock_source)).await;
        });

        // 8. 等待响应
        let response = tokio::time::timeout(Duration::from_secs(2), response_rx.recv())
            .await
            .expect("等待响应超时")
            .expect("未收到响应");

        println!("收到响应帧: {:02X?}", response);

        // 9. 验证响应
        let response_frame = decode_frame(&response).expect("解码响应帧失败");

        println!("响应地址: {:02X?}", response_frame.address);
        println!("响应控制码: 0x{:02X}", response_frame.control);
        println!("响应数据: {:02X?}", response_frame.data);

        // 验证控制码是正常应答
        assert_eq!(response_frame.control, 0x91, "应该收到正常应答");

        // 验证数据格式：DI(4字节) + 电压值(2字节BCD)
        assert!(response_frame.data.len() >= 6, "响应数据应包含DI+电压值");

        // 验证DI回显
        assert_eq!(&response_frame.data[0..4], &di, "响应应回显请求的DI");

        // 解析电压值
        let voltage_bytes = &response_frame.data[4..6];
        let voltage_bcd = u16::from_le_bytes([voltage_bytes[0], voltage_bytes[1]]);
        println!("电压BCD值: 0x{:04X}", voltage_bcd);

        // 默认电压是220.0V，BCD编码为0x2200（小端序）
        // 注意：DL/T645的BCD格式可能需要+33H偏移
        println!("\n✅ 端到端测试通过：Router→MeterActor→VirtualMeter→DIHandler");
    }

    #[tokio::test]
    async fn test_end_to_end_read_energy() {
        println!("\n=== 端到端测试：读取正向有功总电能 ===\n");

        // 1. 创建虚拟电表
        let address = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let meter_config = VirtualMeterConfig {
            address,
            ..Default::default()
        };
        let meter = VirtualMeter::new(meter_config);

        // 2. 创建 MeterActor
        let (_tick_tx, tick_rx) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(100);
        let actor_config = MeterActorConfig {
            address,
            cmd_queue_capacity: 100,
            enable_persistence: false,
            db_pool: None,
            registry_tx: None,
            snapshot_tx: None,
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);
        let handle = MeterActorHandle::new(cmd_tx, address);

        // 启动 Actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // 3. 创建 MeterRegistry 并注册电表
        let mut registry = MeterRegistry::new();
        registry.register(address, handle).unwrap();
        let registry = Arc::new(Mutex::new(registry));

        // 4. 创建 Router
        let config = RouterConfig::default();
        let mut router = Router::new(Arc::clone(&registry), config);

        // 5. 构造读电能命令帧（DI = 00-01-00-00，正向有功总电能）
        let di = [0x00, 0x00, 0x01, 0x00];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读电能命令帧: {:02X?}", frame_bytes);

        // 6. 创建 MockFrameSource
        let (response_tx, mut response_rx) = mpsc::channel(10);
        let mock_source = MockFrameSource::new(vec![frame_bytes.clone()], response_tx);

        // 7. 处理帧
        tokio::spawn(async move {
            router.handle_frame_source(Box::new(mock_source)).await;
        });

        // 8. 等待响应
        let response = tokio::time::timeout(Duration::from_secs(2), response_rx.recv())
            .await
            .expect("等待响应超时")
            .expect("未收到响应");

        println!("收到响应帧: {:02X?}", response);

        // 9. 验证响应
        let response_frame = decode_frame(&response).expect("解码响应帧失败");

        println!("响应地址: {:02X?}", response_frame.address);
        println!("响应控制码: 0x{:02X}", response_frame.control);
        println!("响应数据: {:02X?}", response_frame.data);

        // 验证控制码
        assert_eq!(response_frame.control, 0x91, "应该收到正常应答");

        // 验证数据格式：DI(4字节) + 电能值(4字节BCD)
        assert!(response_frame.data.len() >= 8, "响应数据应包含DI+电能值");

        // 验证DI回显
        assert_eq!(&response_frame.data[0..4], &di, "响应应回显请求的DI");

        // 解析电能值
        let energy_bytes = &response_frame.data[4..8];
        println!("电能数据: {:02X?}", energy_bytes);

        println!("\n✅ 端到端测试通过：Router→MeterActor→VirtualMeter→DIHandler");
    }

    #[tokio::test]
    async fn test_end_to_end_invalid_di() {
        println!("\n=== 端到端测试：读取无效DI（验证错误处理）===\n");

        // 1. 创建虚拟电表
        let address = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let meter_config = VirtualMeterConfig {
            address,
            ..Default::default()
        };
        let meter = VirtualMeter::new(meter_config);

        // 2. 创建 MeterActor
        let (_tick_tx, tick_rx) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(100);
        let actor_config = MeterActorConfig {
            address,
            cmd_queue_capacity: 100,
            enable_persistence: false,
            db_pool: None,
            registry_tx: None,
            snapshot_tx: None,
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);
        let handle = MeterActorHandle::new(cmd_tx, address);

        // 启动 Actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // 3. 创建 MeterRegistry 并注册电表
        let mut registry = MeterRegistry::new();
        registry.register(address, handle).unwrap();
        let registry = Arc::new(Mutex::new(registry));

        // 4. 创建 Router
        let config = RouterConfig::default();
        let mut router = Router::new(Arc::clone(&registry), config);

        // 5. 构造读取无效DI的命令帧
        let di = [0xFF, 0xFF, 0xFF, 0xFF];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读取无效DI命令帧: {:02X?}", frame_bytes);

        // 6. 创建 MockFrameSource
        let (response_tx, mut response_rx) = mpsc::channel(10);
        let mock_source = MockFrameSource::new(vec![frame_bytes.clone()], response_tx);

        // 7. 处理帧
        tokio::spawn(async move {
            router.handle_frame_source(Box::new(mock_source)).await;
        });

        // 8. 等待响应
        let response = tokio::time::timeout(Duration::from_secs(2), response_rx.recv())
            .await
            .expect("等待响应超时")
            .expect("未收到响应");

        println!("收到响应帧: {:02X?}", response);

        // 9. 验证响应
        let response_frame = decode_frame(&response).expect("解码响应帧失败");

        println!("响应地址: {:02X?}", response_frame.address);
        println!("响应控制码: 0x{:02X}", response_frame.control);
        println!("响应数据: {:02X?}", response_frame.data);

        // 验证异常应答
        assert_eq!(response_frame.control, 0xD1, "应该收到异常应答");

        // 验证错误码
        assert_eq!(response_frame.data.len(), 1, "异常应答应包含1字节错误码");
        let error_code = response_frame.data[0];
        println!("错误码: 0x{:02X}", error_code);

        // 错误码应该是 NO_DATA (0x02)
        assert_eq!(error_code, 0x02, "错误码应该是NO_DATA");

        println!("\n✅ 端到端测试通过：正确处理无效DI并返回异常应答");
    }

    #[tokio::test]
    async fn test_end_to_end_multiple_meters() {
        println!("\n=== 端到端测试：多电表场景 ===\n");

        // 1. 创建两个虚拟电表
        let address1 = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        let address2 = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00];

        let meter1 = VirtualMeter::new(VirtualMeterConfig {
            address: address1,
            ..Default::default()
        });
        let meter2 = VirtualMeter::new(VirtualMeterConfig {
            address: address2,
            ..Default::default()
        });

        // 2. 创建两个 MeterActor
        let (_tick_tx, tick_rx1) = broadcast::channel(16);
        let (_tick_tx, tick_rx2) = broadcast::channel(16);

        let (cmd_tx1, cmd_rx1) = mpsc::channel(100);
        let (cmd_tx2, cmd_rx2) = mpsc::channel(100);

        let actor1 = MeterActor::new(
            meter1,
            tick_rx1,
            cmd_rx1,
            MeterActorConfig {
                address: address1,
                enable_persistence: false,
                ..Default::default()
            },
        );
        let actor2 = MeterActor::new(
            meter2,
            tick_rx2,
            cmd_rx2,
            MeterActorConfig {
                address: address2,
                enable_persistence: false,
                ..Default::default()
            },
        );

        let handle1 = MeterActorHandle::new(cmd_tx1, address1);
        let handle2 = MeterActorHandle::new(cmd_tx2, address2);

        // 启动 Actors
        tokio::spawn(async move { actor1.run().await });
        tokio::spawn(async move { actor2.run().await });

        // 3. 创建 MeterRegistry 并注册两个电表
        let mut registry = MeterRegistry::new();
        registry.register(address1, handle1).unwrap();
        registry.register(address2, handle2).unwrap();
        let registry = Arc::new(Mutex::new(registry));

        // 4. 创建 Router
        let config = RouterConfig::default();
        let mut router = Router::new(Arc::clone(&registry), config);

        // 5. 构造两个读电压命令（分别发给两个表）
        let di = [0x00, 0x01, 0x01, 0x02]; // A相电压
        let frame1 = Frame::read(address1, di);
        let frame2 = Frame::read(address2, di);

        let frame_bytes1 = encode_frame(&frame1);
        let frame_bytes2 = encode_frame(&frame2);

        println!("发送给表1的命令: {:02X?}", frame_bytes1);
        println!("发送给表2的命令: {:02X?}", frame_bytes2);

        // 6. 创建 MockFrameSource（包含两个帧）
        let (response_tx, mut response_rx) = mpsc::channel(10);
        let mock_source = MockFrameSource::new(vec![frame_bytes1, frame_bytes2], response_tx);

        // 7. 处理帧
        tokio::spawn(async move {
            router.handle_frame_source(Box::new(mock_source)).await;
        });

        // 8. 收集两个响应
        let mut responses = Vec::new();
        for i in 0..2 {
            let response = tokio::time::timeout(Duration::from_secs(2), response_rx.recv())
                .await
                .expect(&format!("等待响应{}超时", i + 1))
                .expect(&format!("未收到响应{}", i + 1));
            responses.push(response);
        }

        println!("\n收到{}个响应", responses.len());

        // 9. 验证两个响应
        for (i, response) in responses.iter().enumerate() {
            println!("\n验证响应{}: {:02X?}", i + 1, response);

            let response_frame = decode_frame(response).expect("解码响应帧失败");

            println!("  地址: {:02X?}", response_frame.address);
            println!("  控制码: 0x{:02X}", response_frame.control);

            // 验证正常应答
            assert_eq!(response_frame.control, 0x91, "应该收到正常应答");

            // 验证地址匹配
            assert!(
                response_frame.address == address1 || response_frame.address == address2,
                "响应地址应该是两个表之一"
            );
        }

        println!("\n✅ 端到端测试通过：多电表并发处理");
    }
}
