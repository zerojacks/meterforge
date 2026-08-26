// 连接管理器 - 管理串口 / TCP 服务器 / TCP 客户端通道
//
// 职责：
// - 持有电表路由注册表（Arc<Mutex<MeterRegistry>>）
// - 提供启动 TCP 服务器（监听）/ TCP 客户端（连接）/ 串口 的接口
// - 将通道产出的帧交给 Router（通过 conn_tx -> RouterRunner 的 conn_rx）

use crate::actor::MeterRegistry;
use crate::communication_log::{CommunicationLogEntry, CommunicationLogService};
use crate::persistence::{PersistRequest, PersistenceConfig, PersistenceWorker};
use crate::router::{RouterConfig, RouterRunner};
use crate::transport::{
    FrameSource, SerialChannel, SerialChannelConfig, TcpChannel, TcpChannelConfig, TcpConnection,
};
use sqlx::SqlitePool;
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{mpsc, oneshot, Mutex};

/// UI / 外部适配层可提交的连接命令。
/// 具体的串口枚举、连接生命周期和 Tokio 任务均封装在 meter-core 内部。
#[derive(Debug, Clone)]
pub enum ConnectionCommand {
    ListSerialPorts,
    ConnectSerial {
        path: String,
        settings: SerialSettings,
    },
    DisconnectSerial,
    ConnectTcpClient {
        address: String,
    },
    DisconnectTcpClient,
    StartTcpServer {
        address: String,
    },
    StopTcpServer,
    StopAll,
}

/// 串口链路参数。前端仅选择这些领域值，不接触 serialport 库类型。
#[derive(Debug, Clone, Copy)]
pub struct SerialSettings {
    pub baud_rate: u32,
    pub data_bits: SerialDataBits,
    pub parity: SerialParity,
    pub stop_bits: SerialStopBits,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud_rate: 2400,
            data_bits: SerialDataBits::Eight,
            parity: SerialParity::Even,
            stop_bits: SerialStopBits::One,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SerialDataBits {
    Five,
    Six,
    Seven,
    Eight,
}
#[derive(Debug, Clone, Copy)]
pub enum SerialParity {
    None,
    Odd,
    Even,
}
#[derive(Debug, Clone, Copy)]
pub enum SerialStopBits {
    One,
    Two,
}

/// 连接命令的统一执行结果。
#[derive(Debug, Clone)]
pub struct ConnectionResult {
    pub success: bool,
    pub message: String,
    pub serial_ports: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionStatus {
    pub serial_path: Option<String>,
    pub tcp_client_addr: Option<String>,
    pub tcp_server_addr: Option<String>,
}

impl ConnectionResult {
    fn success(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            serial_ports: Vec::new(),
        }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            serial_ports: Vec::new(),
        }
    }
}

/// 连接管理器（可 Clone，供 UI 与 tokio 侧共享）
#[derive(Clone)]
pub struct ConnectionManager {
    registry: Arc<Mutex<MeterRegistry>>,
    conn_tx: mpsc::Sender<Box<dyn FrameSource>>,
    tcp_server_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    tcp_server_shutdown: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    tcp_client_shutdown: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    /// 串口为单总线资源，同时只允许一个活动连接。
    serial_shutdown: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    status: Arc<std::sync::Mutex<ConnectionStatus>>,
    /// 连接层专属 Tokio runtime。GPUI/其他前端运行时不参与连接任务调度。
    runtime: Arc<Runtime>,
    communication_log: CommunicationLogService,
}

impl ConnectionManager {
    /// 创建管理器，返回 (管理器, Router 连接接收器)
    ///
    /// 调用方应使用返回的 conn_rx 启动 RouterRunner。
    pub fn new(
        registry: Arc<Mutex<MeterRegistry>>,
    ) -> (Self, mpsc::Receiver<Box<dyn FrameSource>>) {
        let (conn_tx, conn_rx) = mpsc::channel(100);
        let communication_log =
            CommunicationLogService::new(std::path::PathBuf::from("logs/communication.log"));
        let manager = Self {
            registry,
            conn_tx,
            tcp_server_task: Arc::new(std::sync::Mutex::new(None)),
            tcp_server_shutdown: Arc::new(std::sync::Mutex::new(None)),
            tcp_client_shutdown: Arc::new(std::sync::Mutex::new(None)),
            serial_shutdown: Arc::new(std::sync::Mutex::new(None)),
            status: Arc::new(std::sync::Mutex::new(ConnectionStatus::default())),
            runtime: Arc::new(Runtime::new().expect("failed to create connection runtime")),
            communication_log,
        };
        (manager, conn_rx)
    }

    /// 获取路由注册表（供注册电表）
    pub fn registry(&self) -> Arc<Mutex<MeterRegistry>> {
        Arc::clone(&self.registry)
    }

    pub fn subscribe_communication_logs(
        &self,
    ) -> tokio::sync::broadcast::Receiver<CommunicationLogEntry> {
        self.communication_log.subscribe()
    }

    /// 某台电表的通信日志历史（含发给它的广播帧）。按地址过滤，不是
    /// 整条总线的全部流量——总线上可能同时挂着上千台虚拟表。
    pub fn communication_logs_for(&self, address: [u8; 6]) -> Vec<CommunicationLogEntry> {
        self.communication_log.entries_for(address)
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status.lock().unwrap().clone()
    }

    /// 在连接层专属 runtime 内启动帧路由器。
    /// 这保证 RouterRunner 内部的 Tokio task 不会泄漏到 UI 执行器。
    pub fn start_router(
        &self,
        registry: Arc<Mutex<MeterRegistry>>,
        config: RouterConfig,
        conn_rx: mpsc::Receiver<Box<dyn FrameSource>>,
    ) {
        self.runtime.spawn(async move {
            RouterRunner::new(registry, config, conn_rx).run().await;
        });
    }

    /// 连接层专属 runtime 的 Handle。
    ///
    /// `MeterActor`、`PersistenceWorker` 等依赖 tokio（`tokio::select!` /
    /// `tokio::time` / `sqlx` 的 `runtime-tokio` feature）的任务，一律要通过
    /// 这个 handle 的 `spawn`，不能扔给 GPUI/smol 的 `cx.background_executor()`
    /// —— 那边没有 tokio Runtime 上下文，涉及 tokio::time / sqlx 的代码会直接
    /// panic（"there is no reactor running..."）。
    pub fn runtime_handle(&self) -> Handle {
        self.runtime.handle().clone()
    }

    /// 在连接层专属 runtime 上 spawn 一个任务（`MeterActor::run()` 等）。
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    /// 建立（或打开）SQLite 连接池、启动 `PersistenceWorker` 并将其 spawn 到
    /// 连接层专属 runtime 上。
    ///
    /// 返回值：
    /// - `SqlitePool`：可以 clone 后塞进每个 `MeterActorConfig::db_pool`，
    ///   供低频的 admin 配置写入（ApplySimulationConfig / 冻结配置 /
    ///   结算日 / 密码变更等）直接使用——和 `PersistenceWorker` 共用同一份
    ///   连接池 / WAL 配置，不会各起一个 pool 打同一个 db 文件。
    /// - `mpsc::Sender<PersistRequest>`：挂到每个 `VirtualMeter`
    ///   （`with_persistence`）上，供高频、可容忍短暂丢失的 tick 驱动数据
    ///   （电能寄存器 flush / 负荷记录采样 / 冻结快照）走批量队列。
    pub fn start_persistence(
        &self,
        config: PersistenceConfig,
    ) -> Result<(SqlitePool, mpsc::Sender<PersistRequest>), sqlx::Error> {
        let pool = self.runtime.block_on(PersistenceWorker::connect_pool(&config))?;
        let (persist_tx, persist_rx) = mpsc::channel(config.batch_max_size * 2);
        let worker = PersistenceWorker::with_pool(pool.clone(), config, persist_rx);
        self.runtime.spawn(worker.run());
        Ok((pool, persist_tx))
    }

    /// 连接配置的唯一应用入口。
    ///
    /// 上层不需要知道 SerialChannel、TcpChannel 或 Tokio task 的存在，只发送命令并消费结果。
    pub fn execute(&self, command: ConnectionCommand) -> ConnectionResult {
        match command {
            ConnectionCommand::ListSerialPorts => self.list_serial_ports(),
            ConnectionCommand::ConnectSerial { path, settings } => {
                if path.is_empty() {
                    return ConnectionResult::failure("请选择本机串口");
                }
                match self.start_serial(path.clone(), settings) {
                    Ok(()) => ConnectionResult::success(format!("串口 {path} 已启动")),
                    Err(error) => ConnectionResult::failure(error),
                }
            }
            ConnectionCommand::DisconnectSerial => {
                self.stop_serial();
                ConnectionResult::success("串口已断开")
            }
            ConnectionCommand::ConnectTcpClient { address } => {
                match self.start_tcp_client(address.clone()) {
                    Ok(()) => ConnectionResult::success(format!("已连接 TCP 服务端 {address}")),
                    Err(error) => ConnectionResult::failure(error),
                }
            }
            ConnectionCommand::DisconnectTcpClient => {
                self.stop_tcp_client();
                ConnectionResult::success("TCP 客户端已断开")
            }
            ConnectionCommand::StartTcpServer { address } => {
                match self.start_tcp_server(address.clone()) {
                    Ok(()) => ConnectionResult::success(format!("TCP 服务已在 {address} 启动")),
                    Err(error) => ConnectionResult::failure(error),
                }
            }
            ConnectionCommand::StopTcpServer => {
                self.stop_tcp_server();
                ConnectionResult::success("TCP 服务端已停止")
            }
            ConnectionCommand::StopAll => {
                self.stop_all();
                ConnectionResult::success("已停止全部监听连接")
            }
        }
    }

    /// 在连接层专属运行时的阻塞工作线程中执行命令。
    /// 调用方只等待结果，不需要占用自己的事件线程或参与 Tokio 调度。
    pub fn execute_async(&self, command: ConnectionCommand) -> oneshot::Receiver<ConnectionResult> {
        let manager = self.clone();
        let (result_tx, result_rx) = oneshot::channel();
        self.runtime.spawn_blocking(move || {
            let _ = result_tx.send(manager.execute(command));
        });
        result_rx
    }

    /// 同步读取本机串口。该查询不创建连接，也不暴露 serialport 给 UI 层。
    pub fn list_serial_ports(&self) -> ConnectionResult {
        match serialport::available_ports() {
            Ok(ports) => ConnectionResult {
                success: true,
                message: format!("检测到 {} 个本机串口", ports.len()),
                serial_ports: ports.into_iter().map(|port| port.port_name).collect(),
            },
            Err(error) => ConnectionResult::failure(format!("读取本机串口失败: {error}")),
        }
    }

    /// 启动 TCP 服务器（监听模式，支持多客户端连接）
    pub fn start_tcp_server(&self, addr: String) -> Result<(), String> {
        self.stop_tcp_server();
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = TcpChannelConfig {
            listen_addr: addr.clone(),
            shutdown: Arc::clone(&shutdown),
            log_service: Some(self.communication_log.clone()),
            ..Default::default()
        };
        let channel = TcpChannel::new(config, self.conn_tx.clone());
        let listener = self
            .runtime
            .block_on(channel.bind())
            .map_err(|error| format!("TCP 服务启动失败: {error}"))?;
        let task = self.runtime.spawn(async move {
            let _ = channel.run_with_listener(listener).await;
        });
        *self.tcp_server_task.lock().unwrap() = Some(task);
        *self.tcp_server_shutdown.lock().unwrap() = Some(shutdown);
        self.status.lock().unwrap().tcp_server_addr = Some(addr.clone());
        tracing::info!("TCP server listening on {}", addr);
        Ok(())
    }

    /// 启动 TCP 客户端（主动连接远程服务器）
    pub fn start_tcp_client(&self, addr: String) -> Result<(), String> {
        self.stop_tcp_client();
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = TcpChannelConfig {
            shutdown: Arc::clone(&shutdown),
            log_service: Some(self.communication_log.clone()),
            ..Default::default()
        };
        let conn_tx = self.conn_tx.clone();
        let connect_addr = addr.clone();
        self.runtime.block_on(async move {
            let conn = TcpConnection::connect(connect_addr, config)
                .await
                .map_err(|e| format!("TCP 连接失败: {}", e))?;
            conn_tx
                .send(Box::new(conn))
                .await
                .map_err(|_| "Router 通道已关闭".to_string())
        })?;
        *self.tcp_client_shutdown.lock().unwrap() = Some(shutdown);
        self.status.lock().unwrap().tcp_client_addr = Some(addr.clone());
        tracing::info!("TCP client connected to {}", addr);
        Ok(())
    }

    /// 启动串口通道（RS485 总线仿真）
    pub fn start_serial(&self, path: String, settings: SerialSettings) -> Result<(), String> {
        // 连接新串口前先释放旧串口，避免同时占用多个 COM 设备。
        self.stop_serial();
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = SerialChannelConfig {
            path: path.clone(),
            baud_rate: settings.baud_rate,
            data_bits: match settings.data_bits {
                SerialDataBits::Five => serialport::DataBits::Five,
                SerialDataBits::Six => serialport::DataBits::Six,
                SerialDataBits::Seven => serialport::DataBits::Seven,
                SerialDataBits::Eight => serialport::DataBits::Eight,
            },
            parity: match settings.parity {
                SerialParity::None => serialport::Parity::None,
                SerialParity::Odd => serialport::Parity::Odd,
                SerialParity::Even => serialport::Parity::Even,
            },
            stop_bits: match settings.stop_bits {
                SerialStopBits::One => serialport::StopBits::One,
                SerialStopBits::Two => serialport::StopBits::Two,
            },
            shutdown: Arc::clone(&shutdown),
            log_service: Some(self.communication_log.clone()),
            ..Default::default()
        };
        let channel = SerialChannel::new(config, self.conn_tx.clone());
        // `run` 只有在真实串口已打开并交给 Router 后才返回。
        self.runtime.block_on(channel.run())?;
        // Channel task 只负责把连接交给 Router；真正的关闭通过 shutdown 标志
        // 传递到 SerialConnection 的读线程，随后 Router 收到 None 并释放端口。
        *self.serial_shutdown.lock().unwrap() = Some(shutdown);
        self.status.lock().unwrap().serial_path = Some(path.clone());
        tracing::info!(
            "Serial channel started: {} @ {} bps",
            path,
            settings.baud_rate
        );
        Ok(())
    }

    /// 停止所有监听类任务（TCP 服务器 / 串口）
    pub fn stop_all(&self) {
        self.stop_serial();
        self.stop_tcp_client();
        self.stop_tcp_server();
        tracing::info!("All connection listeners stopped");
    }

    /// 停止当前串口读循环并释放端口句柄。
    pub fn stop_serial(&self) {
        if let Some(shutdown) = self.serial_shutdown.lock().unwrap().take() {
            shutdown.store(true, Ordering::Relaxed);
            tracing::info!("Serial channel stopped");
        }
        self.status.lock().unwrap().serial_path = None;
    }

    pub fn stop_tcp_client(&self) {
        if let Some(shutdown) = self.tcp_client_shutdown.lock().unwrap().take() {
            shutdown.store(true, Ordering::Relaxed);
            tracing::info!("TCP client disconnected");
        }
        self.status.lock().unwrap().tcp_client_addr = None;
    }

    pub fn stop_tcp_server(&self) {
        if let Some(shutdown) = self.tcp_server_shutdown.lock().unwrap().take() {
            shutdown.store(true, Ordering::Relaxed);
        }
        if let Some(task) = self.tcp_server_task.lock().unwrap().take() {
            task.abort();
            tracing::info!("TCP server stopped");
        }
        self.status.lock().unwrap().tcp_server_addr = None;
    }
}