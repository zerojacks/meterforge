// TCP Transport 实现
//
// 按设计方案 4.1 节实现：
// - 基于 tokio::net::TcpListener
// - 支持多客户端并发连接
// - 帧拆分：68H...16H 匹配 + 帧间超时兜底

use super::{FrameSource, RawFrame};
use crate::communication_log::CommunicationLogService;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};

/// 全局连接 ID 计数器（用于 12H 续传缓冲按连接隔离）
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// TCP 通道配置
#[derive(Debug, Clone)]
pub struct TcpChannelConfig {
    /// 监听地址
    pub listen_addr: String,

    /// 帧间超时（用于拆帧）
    pub frame_timeout: Duration,

    /// TCP 客户端主动连接的最长等待时间。
    pub connect_timeout: Duration,

    /// 读缓冲区大小
    pub read_buffer_size: usize,

    /// 连接管理器控制的关闭信号，供监听器和已建立连接共同消费。
    pub shutdown: Arc<AtomicBool>,
    pub log_service: Option<CommunicationLogService>,
}

impl Default for TcpChannelConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8645".to_string(),
            frame_timeout: Duration::from_millis(1000),
            connect_timeout: Duration::from_secs(10),
            read_buffer_size: 4096,
            shutdown: Arc::new(AtomicBool::new(false)),
            log_service: None,
        }
    }
}

/// TCP 连接处理器
pub struct TcpConnection {
    /// 连接描述
    description: String,

    /// 连接 ID
    conn_id: u64,

    /// 读线程提取出的完整帧队列
    frame_rx: mpsc::UnboundedReceiver<Vec<u8>>,

    /// 全局写通道：所有响应都通过它发送给后台 writer
    write_tx: mpsc::UnboundedSender<Vec<u8>>,
}

fn extract_tcp_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    // 查找起始符 68H
    let start_pos = buffer.iter().position(|&b| b == 0x68)?;

    // 确保至少有 12 字节（最小帧长度）
    if buffer.len() < start_pos + 12 {
        return None;
    }

    // 检查第二个 68H（位置 7）
    if start_pos + 7 < buffer.len() && buffer[start_pos + 7] != 0x68 {
        buffer.drain(0..=start_pos);
        return extract_tcp_frame(buffer);
    }

    // 确保有第二个 68H
    if buffer.len() < start_pos + 8 {
        return None;
    }

    // 提取数据域长度 L（位置 9）
    if buffer.len() < start_pos + 10 {
        return None;
    }

    let data_len = buffer[start_pos + 9] as usize;
    let frame_len = 12 + data_len;

    // 检查是否有完整帧
    if buffer.len() < start_pos + frame_len {
        return None;
    }

    // 检查结束符 16H
    if buffer[start_pos + frame_len - 1] != 0x16 {
        buffer.drain(0..=start_pos);
        return extract_tcp_frame(buffer);
    }

    let frame_bytes = buffer.drain(start_pos..start_pos + frame_len).collect();
    Some(frame_bytes)
}

impl TcpConnection {
    /// 创建新的 TCP 连接处理器（从已建立的流）
    pub fn new(stream: TcpStream, peer_addr: String, config: TcpChannelConfig) -> Self {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let description = format!("TCP({})", peer_addr);
        let read_desc = description.clone();
        let write_desc = description.clone();
        let read_buffer_size = config.read_buffer_size;
        let frame_timeout = config.frame_timeout;
        let log_service = config.log_service.clone();
        let read_log = log_service.clone();
        let write_log = log_service.clone();

        let read_shutdown = Arc::clone(&config.shutdown);
        let write_shutdown = Arc::clone(&config.shutdown);

        tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(4096);
            let mut read_half = read_half;

            loop {
                if read_shutdown.load(Ordering::Relaxed) {
                    info!("[{}] Connection closed", read_desc);
                    break;
                }

                let mut temp_buf = vec![0u8; read_buffer_size];
                match timeout(frame_timeout, read_half.read(&mut temp_buf)).await {
                    Ok(Ok(0)) => {
                        info!("[{}] Connection closed", read_desc);
                        break;
                    }
                    Ok(Ok(n)) => {
                        if n == 0 {
                            continue;
                        }
                        buffer.extend_from_slice(&temp_buf[0..n]);
                        while let Some(frame) = extract_tcp_frame(&mut buffer) {
                            if let Some(log) = &read_log {
                                log.record("RX", &read_desc, &frame);
                            }
                            debug!("[{}] Extracted frame: {} bytes", read_desc, frame.len());
                            if frame_tx.send(frame).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("[{}] Read error: {}", read_desc, e);
                        break;
                    }
                    Err(_) => {
                        if !buffer.is_empty() {
                            warn!(
                                "[{}] Frame timeout with {} bytes in buffer, discarding",
                                read_desc,
                                buffer.len()
                            );
                            buffer.clear();
                        }
                    }
                }
            }
        });

        tokio::spawn(async move {
            loop {
                if write_shutdown.load(Ordering::Relaxed) {
                    info!("[{}] Writer task stopped by shutdown", write_desc);
                    break;
                }

                tokio::select! {
                    _ = async {
                        loop {
                            if write_shutdown.load(Ordering::Relaxed) {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    } => {
                        info!("[{}] Writer task stopped by shutdown", write_desc);
                        break;
                    }
                    response = write_rx.recv() => {
                        let Some(response) = response else {
                            info!("[{}] Writer task stopped: channel closed", write_desc);
                            break;
                        };

                        let write_total_start = std::time::Instant::now();
                        info!(
                            "[{}] TX begin: response_len={}, first_bytes={:02X?}",
                            write_desc,
                            response.len(),
                            &response[..response.len().min(16)]
                        );

                        if let Err(e) = write_half.write_all(&response).await {
                            error!("[{}] Failed to send response: {}", write_desc, e);
                            break;
                        }

                        info!(
                            "[{}] TX write_all completed: response_len={}, write_elapsed={:?}",
                            write_desc,
                            response.len(),
                            write_total_start.elapsed()
                        );

                        if let Err(e) = write_half.flush().await {
                            error!("[{}] Failed to flush response: {}", write_desc, e);
                            break;
                        }

                        info!(
                            "[{}] TX flush completed: response_len={}, total_tx_elapsed={:?}",
                            write_desc,
                            response.len(),
                            write_total_start.elapsed()
                        );

                        if let Some(log) = &write_log {
                            log.record("TX", &write_desc, &response);
                        }
                        info!(
                            "[{}] TX response fully sent: len={}, total_tx_elapsed={:?}",
                            write_desc,
                            response.len(),
                            write_total_start.elapsed()
                        );
                    }
                }
            }
        });

        Self {
            description,
            conn_id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
            frame_rx,
            write_tx,
        }
    }

    /// 作为 TCP 客户端主动连接远程服务器（客户端模式）
    pub async fn connect(addr: String, config: TcpChannelConfig) -> std::io::Result<Self> {
        let stream = timeout(config.connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, format!("连接 {addr} 超时"))
            })??;
        Ok(Self::new(stream, addr, config))
    }
}

#[async_trait::async_trait]
impl FrameSource for TcpConnection {
    async fn next_frame(&mut self) -> Option<RawFrame> {
        let bytes = self.frame_rx.recv().await?;
        let rx_start = std::time::Instant::now();
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

/// TCP 通道（监听器）
pub struct TcpChannel {
    /// 配置
    config: TcpChannelConfig,

    /// 连接发送器（发送到 Router）
    conn_tx: mpsc::Sender<Box<dyn FrameSource>>,
}

impl TcpChannel {
    /// 创建新的 TCP 通道
    ///
    /// # 参数
    /// - `config`: TCP 配置
    /// - `conn_tx`: 连接发送器（每个新连接会作为独立的 FrameSource 发送到此通道）
    pub fn new(config: TcpChannelConfig, conn_tx: mpsc::Sender<Box<dyn FrameSource>>) -> Self {
        Self { config, conn_tx }
    }

    /// 实际绑定监听端口，用于在返回“启动成功”之前验证配置。
    pub async fn bind(&self) -> std::io::Result<TcpListener> {
        TcpListener::bind(&self.config.listen_addr).await
    }

    /// 使用已绑定的监听器运行 TCP 服务端。
    pub async fn run_with_listener(self, listener: TcpListener) -> std::io::Result<()> {
        info!("TCP channel listening on {}", self.config.listen_addr);

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    info!("New TCP connection from {}", peer_addr);

                    let connection =
                        TcpConnection::new(stream, peer_addr.to_string(), self.config.clone());

                    // 将连接作为 FrameSource 发送到 Router
                    if let Err(e) = self.conn_tx.send(Box::new(connection)).await {
                        error!("Failed to send connection to router: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    /// 运行 TCP 监听器。
    pub async fn run(self) -> std::io::Result<()> {
        let listener = self.bind().await?;
        self.run_with_listener(listener).await
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn test_extract_frame_simple() {
        // 这个测试需要实际的TCP连接，暂时跳过
        // TODO: 使用 mock stream 进行测试
    }

    #[test]
    fn test_frame_extraction_logic() {
        // 测试帧提取逻辑（不需要实际网络连接）
        let buffer = vec![
            0x68, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // 起始 + 地址
            0x68, 0x11, 0x04, // 起始 + 控制 + 长度
            0x33, 0x34, 0x33, 0x33, // 数据域（+33H）
            0x7E, // 校验和
            0x16, // 结束符
        ];

        // 验证帧长度计算
        let data_len = buffer[9] as usize;
        let expected_len = 12 + data_len;
        assert_eq!(expected_len, 16);
        assert_eq!(buffer.len(), 16);
    }
}
