// Serial Transport 实现（RS485 总线仿真）
//
// 按设计方案 4.1 节实现：
// - 基于 `serialport`（偶校验，8N1）
// - 剥离前导 `FE FE FE FE`
// - 字节间超时拆帧（68H...16H 匹配 + 帧间超时兜底）
// - 与 TcpChannel 同构：产出 RawFrame 送入 Router 的 conn_tx

use super::{FrameSource, RawFrame};
use crate::communication_log::CommunicationLogService;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// 全局串口连接 ID 计数器
static NEXT_SERIAL_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// 串口通道配置
#[derive(Debug, Clone)]
pub struct SerialChannelConfig {
    /// 串口设备路径，如 /dev/ttyUSB0、COM3
    pub path: String,

    /// 初始波特率（645 默认 2400 bps）
    pub baud_rate: u32,

    pub data_bits: serialport::DataBits,
    pub parity: serialport::Parity,
    pub stop_bits: serialport::StopBits,

    /// 由连接管理器设置，用于关闭活动串口读线程并释放设备句柄。
    pub shutdown: Arc<AtomicBool>,

    /// 帧间超时（用于拆帧兜底）
    pub frame_timeout: Duration,
    pub log_service: Option<CommunicationLogService>,
}

impl Default for SerialChannelConfig {
    fn default() -> Self {
        Self {
            path: "/dev/ttyUSB0".to_string(),
            baud_rate: 2400,
            data_bits: serialport::DataBits::Eight,
            parity: serialport::Parity::Even,
            stop_bits: serialport::StopBits::One,
            shutdown: Arc::new(AtomicBool::new(false)),
            frame_timeout: Duration::from_millis(1000),
            log_service: None,
        }
    }
}

/// 帧提取缓冲区：剥离前导 + 68H...16H 匹配 + 超时兜底
struct FrameExtractor {
    buffer: Vec<u8>,
}

impl FrameExtractor {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
        }
    }

    /// 追加字节并提取所有完整帧
    fn push_and_extract(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(data);
        let mut frames = Vec::new();
        while let Some(frame) = self.extract_frame() {
            frames.push(frame);
        }
        frames
    }

    /// 尝试从缓冲区提取一个完整帧
    fn extract_frame(&mut self) -> Option<Vec<u8>> {
        // 剥离前导字节（FE 唤醒前导，不计入帧）
        let start = self.buffer.iter().position(|&b| b == 0x68)?;
        if start > 0 {
            self.buffer.drain(0..start);
        }

        // 最小帧长度 12
        if self.buffer.len() < 12 {
            return None;
        }

        // 第二个 68H（位置 7）
        if self.buffer[7] != 0x68 {
            self.buffer.drain(0..1);
            return self.extract_frame();
        }

        // 数据域长度 L（位置 9）
        let data_len = self.buffer[9] as usize;
        let frame_len = 12 + data_len;

        if self.buffer.len() < frame_len {
            return None;
        }

        // 结束符 16H
        if self.buffer[frame_len - 1] != 0x16 {
            self.buffer.drain(0..1);
            return self.extract_frame();
        }

        let frame = self.buffer.drain(0..frame_len).collect();
        Some(frame)
    }

    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn pending(&self) -> usize {
        self.buffer.len()
    }

    fn clear(&mut self) {
        self.buffer.clear();
    }
}

/// 串口连接（实现 FrameSource）
pub struct SerialConnection {
    /// 串口（Arc<Mutex> 包装以支持后台线程读 + 响应写）
    port: Arc<Mutex<Box<dyn serialport::SerialPort>>>,

    /// 连接描述
    description: String,

    /// 连接 ID
    conn_id: u64,

    /// 读线程提取出的完整帧队列
    frame_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    log_service: Option<CommunicationLogService>,
}

impl SerialConnection {
    /// 打开串口并启动后台读线程
    pub fn open(config: SerialChannelConfig) -> Result<Self, String> {
        let port = serialport::new(&config.path, config.baud_rate)
            .data_bits(config.data_bits)
            .parity(config.parity)
            .stop_bits(config.stop_bits)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|e| format!("Failed to open serial port {}: {}", config.path, e))?;

        let port = Arc::new(Mutex::new(port));
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // 后台读线程：读字节 → 剥离前导 → 拆帧 → 送入队列
        let port_clone = Arc::clone(&port);
        let frame_timeout = config.frame_timeout;
        let path = config.path.clone();
        let shutdown = Arc::clone(&config.shutdown);
        std::thread::spawn(move || {
            let mut extractor = FrameExtractor::new();
            let mut buf = [0u8; 512];
            let mut last_byte_at = std::time::Instant::now();

            while !shutdown.load(Ordering::Relaxed) {
                let mut port = match port_clone.lock() {
                    Ok(p) => p,
                    Err(_) => break,
                };

                match port.read(&mut buf) {
                    Ok(0) => {
                        drop(port);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(n) => {
                        last_byte_at = std::time::Instant::now();
                        drop(port);
                        for frame in extractor.push_and_extract(&buf[..n]) {
                            if frame_tx.send(frame).is_err() {
                                return; // 接收端已关闭
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        drop(port);
                        // 帧间超时：丢弃残留的半帧
                        if !extractor.is_empty() && last_byte_at.elapsed() > frame_timeout {
                            warn!(
                                "[Serial({})] Frame timeout, discarding {} pending bytes",
                                path,
                                extractor.pending()
                            );
                            extractor.clear();
                        }
                    }
                    Err(e) => {
                        error!("[Serial({})] Read error: {}", path, e);
                        break;
                    }
                }
            }

            info!("[Serial({})] Reader thread stopped", path);
        });

        Ok(Self {
            port,
            description: format!("Serial({})", config.path),
            conn_id: NEXT_SERIAL_CONN_ID.fetch_add(1, Ordering::Relaxed),
            frame_rx,
            log_service: config.log_service,
        })
    }
}

#[async_trait::async_trait]
impl FrameSource for SerialConnection {
    async fn next_frame(&mut self) -> Option<RawFrame> {
        let bytes = self.frame_rx.recv().await?;
        let rx_start = std::time::Instant::now();
        if let Some(log) = &self.log_service {
            log.record("RX", &self.description, &bytes);
        }

        debug!(
            "[{}] Extracted frame: {} bytes",
            self.description,
            bytes.len()
        );
        info!(
            "[{}] RX frame accepted: len={}, first_bytes={:02X?}, elapsed_after_read={:?}",
            self.description,
            bytes.len(),
            &bytes[..bytes.len().min(16)],
            rx_start.elapsed()
        );

        // 创建回复通道（mpsc），后台逐个写回串口
        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let port = Arc::clone(&self.port);
        let desc = self.description.clone();
        let log_service = self.log_service.clone();
        tokio::spawn(async move {
            while let Some(response) = reply_rx.recv().await {
                let write_total_start = std::time::Instant::now();
                let lock_start = std::time::Instant::now();
                info!(
                    "[{}] TX begin: response_len={}, before_lock_elapsed={:?}, first_bytes={:02X?}",
                    desc,
                    response.len(),
                    lock_start.elapsed(),
                    &response[..response.len().min(16)]
                );

                let mut port = match port.lock() {
                    Ok(p) => p,
                    Err(_) => {
                        error!(
                            "[{}] TX aborted: failed to acquire serial port lock after {:?}",
                            desc,
                            lock_start.elapsed()
                        );
                        break;
                    }
                };

                let lock_elapsed = lock_start.elapsed();
                info!(
                    "[{}] TX lock acquired: response_len={}, lock_elapsed={:?}",
                    desc,
                    response.len(),
                    lock_elapsed
                );

                let write_start = std::time::Instant::now();
                let write_result = port.write_all(&response);
                let write_elapsed = write_start.elapsed();
                match write_result {
                    Ok(_) => {
                        info!(
                            "[{}] TX write_all completed: response_len={}, write_elapsed={:?}",
                            desc,
                            response.len(),
                            write_elapsed
                        );
                    }
                    Err(e) => {
                        error!(
                            "[{}] TX write_all failed: response_len={}, write_elapsed={:?}, error={}",
                            desc,
                            response.len(),
                            write_elapsed,
                            e
                        );
                        break;
                    }
                }

                let flush_start = std::time::Instant::now();
                let flush_result = port.flush();
                let flush_elapsed = flush_start.elapsed();
                match flush_result {
                    Ok(_) => {
                        info!(
                            "[{}] TX flush completed: response_len={}, flush_elapsed={:?}, total_tx_elapsed={:?}",
                            desc,
                            response.len(),
                            flush_elapsed,
                            write_total_start.elapsed()
                        );
                    }
                    Err(e) => {
                        error!(
                            "[{}] TX flush failed: response_len={}, flush_elapsed={:?}, total_tx_elapsed={:?}, error={}",
                            desc,
                            response.len(),
                            flush_elapsed,
                            write_total_start.elapsed(),
                            e
                        );
                        break;
                    }
                }

                if let Some(log) = &log_service {
                    log.record("TX", &desc, &response);
                }
                debug!(
                    "[{}] Response sent: {} bytes in {:?}",
                    desc,
                    response.len(),
                    write_total_start.elapsed()
                );
                info!(
                    "[{}] TX response fully sent: len={}, total_tx_elapsed={:?}, lock_elapsed={:?}, write_elapsed={:?}, flush_elapsed={:?}",
                    desc,
                    response.len(),
                    write_total_start.elapsed(),
                    lock_elapsed,
                    write_elapsed,
                    flush_elapsed
                );
            }
        });

        Some(RawFrame {
            conn_id: self.conn_id,
            bytes,
            reply_channel: reply_tx,
        })
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

/// 串口通道（监听器，与 TcpChannel 同构）
pub struct SerialChannel {
    /// 配置
    config: SerialChannelConfig,

    /// 连接发送器（发送到 Router）
    conn_tx: mpsc::Sender<Box<dyn FrameSource>>,
}

impl SerialChannel {
    /// 创建串口通道
    pub fn new(config: SerialChannelConfig, conn_tx: mpsc::Sender<Box<dyn FrameSource>>) -> Self {
        Self { config, conn_tx }
    }

    /// 打开串口并将连接送入 Router
    ///
    /// 注意：645 默认方案 C（仿真模式）下，17H 改通信速率只更新表状态，
    /// 不真实重开串口硬件参数。
    pub async fn run(self) -> Result<(), String> {
        let connection = SerialConnection::open(self.config)?;
        info!("Serial channel connected: {}", connection.description());

        if let Err(e) = self.conn_tx.send(Box::new(connection)).await {
            error!("Failed to send serial connection to router: {}", e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_frame_with_preamble() {
        let mut extractor = FrameExtractor::new();

        // 前导 FE FE FE FE + 完整帧
        let mut data = vec![0xFE, 0xFE, 0xFE, 0xFE];
        data.extend_from_slice(&[
            0x68, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // 起始 + 地址
            0x68, 0x11, 0x04, // 起始 + 控制 + 长度
            0x33, 0x34, 0x33, 0x33, // 数据域
            0x7E, // 校验和
            0x16, // 结束符
        ]);

        let frames = extractor.push_and_extract(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], 0x68); // 前导已被剥离
        assert_eq!(frames[0][frames[0].len() - 1], 0x16);
        assert!(extractor.is_empty());
    }

    #[test]
    fn test_extract_incomplete_frame() {
        let mut extractor = FrameExtractor::new();
        // 只给半帧，不应提取
        let frames = extractor.push_and_extract(&[0x68, 0x01, 0x02, 0x03, 0x04, 0x05]);
        assert!(frames.is_empty());
        assert!(!extractor.is_empty());
    }
}
