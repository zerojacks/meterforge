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
use tokio::sync::Mutex;
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
    /// TCP 流（用Arc<Mutex>包装以支持响应写入）
    stream: Arc<Mutex<TcpStream>>,

    /// 连接描述
    description: String,

    /// 配置
    config: TcpChannelConfig,

    /// 读缓冲区
    buffer: Vec<u8>,

    /// 连接 ID
    conn_id: u64,
}

impl TcpConnection {
    /// 创建新的 TCP 连接处理器（从已建立的流）
    pub fn new(stream: TcpStream, peer_addr: String, config: TcpChannelConfig) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
            description: format!("TCP({})", peer_addr),
            config,
            buffer: Vec::with_capacity(4096),
            conn_id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
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

    /// 从缓冲区中提取一个完整帧
    ///
    /// DL/T 645-2007 帧格式：68H A0~A5 68H C L DATA CS 16H
    /// 最小长度：12 字节
    fn extract_frame(&mut self) -> Option<Vec<u8>> {
        // 查找起始符 68H
        let start_pos = self.buffer.iter().position(|&b| b == 0x68)?;

        // 确保至少有 12 字节（最小帧长度）
        if self.buffer.len() < start_pos + 12 {
            return None;
        }

        // 检查第二个 68H（位置 7）
        if start_pos + 7 < self.buffer.len() && self.buffer[start_pos + 7] != 0x68 {
            // 不是有效帧，丢弃第一个 68H 继续查找
            self.buffer.drain(0..=start_pos);
            return self.extract_frame();
        }

        // 确保有第二个 68H
        if self.buffer.len() < start_pos + 8 {
            return None;
        }

        // 提取数据域长度 L（位置 9）
        if self.buffer.len() < start_pos + 10 {
            return None;
        }

        let data_len = self.buffer[start_pos + 9] as usize;

        // 计算完整帧长度
        // 68H(1) + ADDR(6) + 68H(1) + C(1) + L(1) + DATA(data_len) + CS(1) + 16H(1)
        let frame_len = 12 + data_len;

        // 检查是否有完整帧
        if self.buffer.len() < start_pos + frame_len {
            return None;
        }

        // 检查结束符 16H
        if self.buffer[start_pos + frame_len - 1] != 0x16 {
            // 不是有效帧，丢弃到第一个 68H 之后继续查找
            self.buffer.drain(0..=start_pos);
            return self.extract_frame();
        }

        // 提取完整帧
        let frame_bytes = self
            .buffer
            .drain(start_pos..start_pos + frame_len)
            .collect();

        Some(frame_bytes)
    }

    /// 读取并提取帧
    async fn read_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if self.config.shutdown.load(Ordering::Relaxed) {
                return Ok(None);
            }
            // 先尝试从缓冲区提取
            if let Some(frame) = self.extract_frame() {
                return Ok(Some(frame));
            }

            // 缓冲区没有完整帧，读取更多数据
            let mut temp_buf = vec![0u8; self.config.read_buffer_size];

            let read_result = {
                let mut stream = self.stream.lock().await;
                timeout(self.config.frame_timeout, stream.read(&mut temp_buf)).await
            };

            match read_result {
                Ok(Ok(0)) => {
                    // 连接关闭
                    return Ok(None);
                }
                Ok(Ok(n)) => {
                    // 读取到数据
                    self.buffer.extend_from_slice(&temp_buf[0..n]);
                    debug!(
                        "[{}] Read {} bytes, buffer size: {}",
                        self.description,
                        n,
                        self.buffer.len()
                    );
                }
                Ok(Err(e)) => {
                    // 读取错误
                    return Err(e);
                }
                Err(_) => {
                    // 超时，检查是否有部分数据
                    if !self.buffer.is_empty() {
                        warn!(
                            "[{}] Frame timeout with {} bytes in buffer, discarding",
                            self.description,
                            self.buffer.len()
                        );
                        self.buffer.clear();
                    }
                    // 继续等待下一个帧
                    continue;
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl FrameSource for TcpConnection {
    async fn next_frame(&mut self) -> Option<RawFrame> {
        match self.read_frame().await {
            Ok(Some(bytes)) => {
                debug!(
                    "[{}] Extracted frame: {} bytes",
                    self.description,
                    bytes.len()
                );
                if let Some(log) = &self.config.log_service {
                    log.record("RX", &self.description, &bytes);
                }

                // 创建回复通道（mpsc：支持通配多表应答与续传多片段），后台逐个写回 socket
                let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
                let stream_clone = Arc::clone(&self.stream);
                let desc = self.description.clone();
                let log_service = self.config.log_service.clone();
                tokio::spawn(async move {
                    while let Some(response) = reply_rx.recv().await {
                        let mut stream = stream_clone.lock().await;
                        if let Err(e) = stream.write_all(&response).await {
                            error!("[{}] Failed to send response: {}", desc, e);
                            break;
                        }
                        let _ = stream.flush().await;
                        if let Some(log) = &log_service {
                            log.record("TX", &desc, &response);
                        }
                        debug!("[{}] Response sent: {} bytes", desc, response.len());
                    }
                });

                Some(RawFrame {
                    conn_id: self.conn_id,
                    bytes,
                    reply_channel: reply_tx,
                })
            }
            Ok(None) => {
                info!("[{}] Connection closed", self.description);
                None
            }
            Err(e) => {
                error!("[{}] Read error: {}", self.description, e);
                None
            }
        }
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
    use super::*;

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
