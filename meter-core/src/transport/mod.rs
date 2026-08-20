// Transport 层 - 字节流与协议帧的转换
//
// 按设计方案 4.1 节实现：
// - trait FrameSource: 异步产出完整帧
// - TcpChannel: TCP连接处理
// - SerialChannel: 串口连接处理（可选）

pub mod serial;
pub mod tcp;

use tokio::sync::mpsc;

// 导出 TCP 相关类型
pub use tcp::{TcpChannel, TcpChannelConfig, TcpConnection};
// 导出串口相关类型
pub use serial::{SerialChannel, SerialChannelConfig, SerialConnection};

/// 原始帧数据（来自 Transport 层）
#[derive(Debug)]
pub struct RawFrame {
    /// 来源连接 ID（用于 12H 续传缓冲按连接隔离）
    pub conn_id: u64,

    /// 完整的字节流（68H...16H）
    pub bytes: Vec<u8>,

    /// 响应通道（mpsc：支持通配查询多表应答与续传多片段）
    pub reply_channel: mpsc::UnboundedSender<Vec<u8>>,
}

/// 帧源 trait（统一 TCP/Serial 的接口）
///
/// 职责：
/// - 从字节流中提取完整的协议帧
/// - 不做协议语义校验（由 Router/Codec 负责）
/// - 提供响应回传机制
#[async_trait::async_trait]
pub trait FrameSource: Send {
    /// 接收下一个完整帧
    ///
    /// # 返回
    /// - `Some(RawFrame)`: 收到完整帧
    /// - `None`: 连接关闭
    async fn next_frame(&mut self) -> Option<RawFrame>;

    /// 获取连接描述（用于日志）
    fn description(&self) -> String;
}

/// Transport 配置
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// TCP 监听地址
    pub tcp_listen_addr: String,

    /// TCP 端口
    pub tcp_port: u16,

    /// 最大并发连接数
    pub max_connections: usize,

    /// 帧间超时（毫秒）
    pub frame_timeout_ms: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            tcp_listen_addr: "127.0.0.1".to_string(),
            tcp_port: 8645,
            max_connections: 100,
            frame_timeout_ms: 1000,
        }
    }
}
