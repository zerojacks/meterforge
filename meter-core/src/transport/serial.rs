// Serial Transport 实现（RS485 总线仿真）
//
// 按设计方案 4.1 节实现：
// - 基于 `serialport`（偶校验，8N1）
// - 剥离前导 `FE FE FE FE`
// - 字节间超时拆帧（68H...16H 匹配 + 帧间超时兜底）
// - 与 TcpChannel 同构：产出 RawFrame 送入 Router 的 conn_tx

use super::{FrameSource, RawFrame};
use crate::communication_log::CommunicationLogService;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_serial::SerialPortBuilderExt;
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
    /// 连接描述
    description: String,

    /// 连接 ID
    conn_id: u64,

    /// 读线程提取出的完整帧队列
    frame_rx: mpsc::UnboundedReceiver<Vec<u8>>,

    /// 全局写通道：所有响应都通过它发送给后台 writer
    write_tx: mpsc::UnboundedSender<Vec<u8>>,
    log_service: Option<CommunicationLogService>,
}

impl SerialConnection {
    /// 打开串口并启动后台读线程
    pub fn open(config: SerialChannelConfig) -> Result<Self, String> {
        let port = tokio_serial::new(&config.path, config.baud_rate)
            .data_bits(config.data_bits)
            .parity(config.parity)
            .stop_bits(config.stop_bits)
            .open_native_async()
            .map_err(|e| format!("Failed to open serial port {}: {}", config.path, e))?;

        let (mut read_half, mut write_half) = tokio::io::split(port);
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let frame_timeout = config.frame_timeout;
        let path = config.path.clone();
        let shutdown = Arc::clone(&config.shutdown);
        let log_service = config.log_service.clone();
        let read_path = path.clone();
        let write_path = path.clone();
        let writer_log = log_service.clone();

        // 后台读线程：异步读取字节 → 剥离前导 → 拆帧 → 送入队列
        tokio::spawn(async move {
            let mut extractor = FrameExtractor::new();
            let mut buf = [0u8; 512];
            let mut last_byte_at = std::time::Instant::now();

            loop {
                if shutdown.load(Ordering::Relaxed) {
                    info!("[Serial({})] Reader task stopped", read_path);
                    break;
                }

                match tokio::time::timeout(Duration::from_millis(50), read_half.read(&mut buf)).await {
                    Ok(Ok(0)) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Ok(Ok(n)) => {
                        last_byte_at = std::time::Instant::now();
                        for frame in extractor.push_and_extract(&buf[..n]) {
                            if frame_tx.send(frame).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("[Serial({})] Read error: {}", read_path, e);
                        break;
                    }
                    Err(_) => {
                        if !extractor.is_empty() && last_byte_at.elapsed() > frame_timeout {
                            warn!(
                                "[Serial({})] Frame timeout, discarding {} pending bytes",
                                read_path,
                                extractor.pending()
                            );
                            extractor.clear();
                        }
                    }
                }
            }
        });

        // 后台写线程：单独处理所有应答输出，避免读线程长时间持锁导致写线程阻塞
        tokio::spawn(async move {
            while let Some(response) = write_rx.recv().await {
                let write_total_start = std::time::Instant::now();
                info!(
                    "[Serial({})] TX begin: response_len={}, first_bytes={:02X?}",
                    write_path,
                    response.len(),
                    &response[..response.len().min(16)]
                );

                match write_half.write_all(&response).await {
                    Ok(_) => {
                        info!(
                            "[Serial({})] TX write_all completed: response_len={}, write_elapsed={:?}",
                            write_path,
                            response.len(),
                            write_total_start.elapsed()
                        );
                    }
                    Err(e) => {
                        error!(
                            "[Serial({})] TX write_all failed: response_len={}, elapsed={:?}, error={}",
                            write_path,
                            response.len(),
                            write_total_start.elapsed(),
                            e
                        );
                        break;
                    }
                }

                match write_half.flush().await {
                    Ok(_) => {
                        info!(
                            "[Serial({})] TX flush completed: response_len={}, total_tx_elapsed={:?}",
                            write_path,
                            response.len(),
                            write_total_start.elapsed()
                        );
                    }
                    Err(e) => {
                        error!(
                            "[Serial({})] TX flush failed: response_len={}, total_tx_elapsed={:?}, error={}",
                            write_path,
                            response.len(),
                            write_total_start.elapsed(),
                            e
                        );
                        break;
                    }
                }

                if let Some(log) = &writer_log {
                    log.record("TX", &write_path, &response);
                }
                info!(
                    "[Serial({})] TX response fully sent: len={}, total_tx_elapsed={:?}",
                    write_path,
                    response.len(),
                    write_total_start.elapsed()
                );
            }
        });

        Ok(Self {
            description: format!("Serial({})", config.path),
            conn_id: NEXT_SERIAL_CONN_ID.fetch_add(1, Ordering::Relaxed),
            frame_rx,
            write_tx,
            log_service,
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

        Some(RawFrame {
            conn_id: self.conn_id,
            bytes,
            reply_channel: self.write_tx.clone(),
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
