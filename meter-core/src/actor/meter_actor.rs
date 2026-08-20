// MeterActor - 单个电表的Actor实现
//
// 按设计方案 4.5 节实现：
// - select! 主循环处理 tick/protocol_cmd/admin_cmd
// - 订阅全局 tick 广播
// - 持有 VirtualMeter 和 PersistenceWorker 发送器

use super::messages::{AdminCommand, EngineMsg, RegistryMsg, TickMsg};
use crate::protocol::format::format_address;
use crate::simulation::VirtualMeter;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

/// 单帧最大数据片段字节数（续传时 DATA = DI(4) + SEQ(1) + 数据片段 ≤ L=200）
const MAX_FRAGMENT: usize = 195;

/// 续传缓冲（12H 读后续数据），键 = (conn_id, DI)
struct PendingContinuation {
    /// 完整响应数据（已编码，含 DI 之外的值字节）
    full_data: Vec<u8>,
    /// 下次续传起始偏移
    next_offset: usize,
    /// 下次期望的 SEQ（从 1 递增）
    next_seq: u8,
    /// 创建时刻（用于过期检测）
    created_at: Instant,
}

/// MeterActor 配置
#[derive(Debug, Clone)]
pub struct MeterActorConfig {
    /// 电表地址（12位BCD）
    pub address: [u8; 6],

    /// 命令队列容量
    pub cmd_queue_capacity: usize,

    /// 是否启用持久化
    pub enable_persistence: bool,

    /// 数据库连接池（可选，用于优雅关闭）
    pub db_pool: Option<sqlx::SqlitePool>,

    /// 注册表联动发送器（可选，用于 15H 写通信地址后更新路由表）
    pub registry_tx: Option<mpsc::Sender<RegistryMsg>>,

    /// 快照推送发送器（可选，用于向 UI Entity 推送实时快照）
    pub snapshot_tx: Option<mpsc::UnboundedSender<crate::snapshot::MeterSnapshot>>,
}

impl Default for MeterActorConfig {
    fn default() -> Self {
        Self {
            address: [0x12, 0x34, 0x56, 0x78, 0x90, 0x12],
            cmd_queue_capacity: 100,
            enable_persistence: true,
            db_pool: None,
            registry_tx: None,
            snapshot_tx: None,
        }
    }
}

/// MeterActor - 单表Actor
///
/// 架构说明（按设计方案 4.5 节）：
/// - 每个表是独立的 tokio task，拥有独立的 MeterState
/// - 通过消息传递与外界交互（不共享状态，无锁）
/// - 订阅全局 tick 广播统一推进仿真
/// - 命令队列串行处理，保证结果可复现
pub struct MeterActor {
    /// 虚拟电表实例
    meter: VirtualMeter,

    /// Tick 接收器（全局广播订阅）
    tick_rx: broadcast::Receiver<TickMsg>,

    /// 命令接收器
    cmd_rx: mpsc::Receiver<EngineMsg>,

    /// 配置
    config: MeterActorConfig,

    /// 续传缓冲（12H 读后续数据）：key = (conn_id, DI)
    continuation_cache: HashMap<(u64, [u8; 4]), PendingContinuation>,
}

impl MeterActor {
    /// 创建新的 MeterActor
    ///
    /// 参数：
    /// - meter: VirtualMeter 实例（已配置好持久化通道）
    /// - tick_rx: 全局 tick 广播接收器
    /// - cmd_rx: 命令接收器
    /// - config: Actor 配置
    pub fn new(
        meter: VirtualMeter,
        tick_rx: broadcast::Receiver<TickMsg>,
        cmd_rx: mpsc::Receiver<EngineMsg>,
        config: MeterActorConfig,
    ) -> Self {
        Self {
            meter,
            tick_rx,
            cmd_rx,
            config,
            continuation_cache: HashMap::new(),
        }
    }

    /// 运行 Actor 主循环
    ///
    /// 按设计方案 4.5 节实现 select! 循环：
    /// - on_tick: 推进仿真
    /// - on_protocol_command: 处理协议命令
    /// - on_admin_command: 处理管理命令
    pub async fn run(mut self) {
        let address_str = format_address(&self.config.address);

        info!("[MeterActor {}] Started", address_str);

        loop {
            tokio::select! {
                // 处理 tick 广播
                Ok(tick) = self.tick_rx.recv() => {
                    self.on_tick(tick).await;
                }

                // 处理命令
                Some(msg) = self.cmd_rx.recv() => {
                    match msg {
                        EngineMsg::ProtocolCommand { conn_id, frame, reply_tx } => {
                            self.on_protocol_command(conn_id, frame, reply_tx).await;
                        }
                        EngineMsg::AdminCommand { cmd, reply_tx } => {
                            let should_shutdown = matches!(cmd, AdminCommand::Shutdown);
                            self.on_admin_command(cmd, reply_tx).await;
                        }
                    }
                }

                // 通道关闭时退出
                else => {
                    warn!("[MeterActor {}] All channels closed, exiting", address_str);
                    break;
                }
            }
        }

        info!("[MeterActor {}] Stopped", address_str);
    }

    /// 处理 Tick 消息
    ///
    /// 按设计方案 4.5.1 节执行：
    /// 1. 虚拟时钟推进
    /// 2. 负荷模型取瞬时量
    /// 3. 脉冲累加电能量
    /// 4. 最大需量滑差窗口
    /// 5. 事件检测
    /// 6. 冻结调度检查
    /// 7. 负荷记录采样调度
    async fn on_tick(&mut self, tick: TickMsg) {
        // VirtualMeter.tick() 已封装了完整的仿真逻辑
        self.meter.tick(
            tick.wall_elapsed,
            tick.time_scale * self.meter.state().simulation_time_scale,
        );

        // 注意：冻结和电能flush已在 VirtualMeter.tick() 中处理
        // - process_pending_freeze() 处理冻结触发
        // - check_energy_flush() 处理电能寄存器刷新

        // 推送实时快照到 UI（best-effort）
        self.push_snapshot();

        #[cfg(debug_assertions)]
        {
            // 每10秒打印一次状态（用于调试）
            static mut TICK_COUNT: u64 = 0;
            unsafe {
                TICK_COUNT += 1;
                if TICK_COUNT % 10 == 0 {
                    debug!(
                        "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Tick #{}, vtime={}",
                        self.config.address[5],
                        self.config.address[4],
                        self.config.address[3],
                        self.config.address[2],
                        self.config.address[1],
                        self.config.address[0],
                        TICK_COUNT,
                        self.meter.state().virtual_time.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
        }
    }

    /// 推送实时快照到 UI（best-effort，失败不 panic）
    fn push_snapshot(&self) {
        if let Some(tx) = &self.config.snapshot_tx {
            let snapshot = crate::snapshot::MeterSnapshot::from_state(
                self.meter.state(),
                self.meter.load_model_config(),
                true,
            );
            let _ = tx.send(snapshot);
        }
    }

    /// 处理协议命令
    ///
    /// 按设计方案 4.5.2 节实现：
    /// - 按控制码路由到对应 handler
    /// - 权限与密码校验
    /// - 读数据/写数据/冻结命令等
    async fn on_protocol_command(
        &mut self,
        conn_id: u64,
        frame: crate::protocol::Frame,
        reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) {
        use crate::protocol::{encode_error_response, encode_frame, ErrorInfoWord};

        let cmd_start = Instant::now();
        let control = frame.control;
        let data_len = frame.data.len();
        let frame_preview = frame.data[..frame.data.len().min(16)].to_vec();
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] protocol command start: conn_id={}, control=0x{:02X}, data_len={}, data={:02X?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            conn_id,
            control,
            data_len,
            frame_preview
        );

        // 根据控制码类型处理
        let response_bytes = match control & 0x1F {
            0x11 => {
                // 读数据命令（支持续传）
                self.handle_read_command(frame, conn_id).await
            }
            0x12 => {
                // 读后续数据命令（续传）
                self.handle_follow_on_command(frame, conn_id).await
            }
            0x13 => {
                // 读通信地址命令
                self.handle_read_comm_address(frame).await
            }
            0x14 => {
                // 写数据命令
                self.handle_write_command(frame).await
            }
            0x15 => {
                // 写通信地址命令
                self.handle_write_comm_address(frame).await
            }
            0x08 => {
                // 广播校时命令
                self.handle_broadcast_time_sync(frame).await
            }
            0x16 => {
                // 冻结命令
                self.handle_freeze_command(frame).await
            }
            0x17 => {
                // 更改通信速率命令
                self.handle_change_baudrate_command(frame).await
            }
            0x18 => {
                // 修改密码命令
                self.handle_change_password_command(frame).await
            }
            0x19 => {
                // 需量清零命令
                self.handle_demand_clear_command(frame).await
            }
            0x1A => {
                // 电表清零命令
                self.handle_meter_clear_command(frame).await
            }
            0x1B => {
                // 事件清零命令
                self.handle_event_clear_command(frame).await
            }
            _ => {
                // 其他命令暂未实现
                encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::NO_DATA),
                )
            }
        };

        if let Err(_) = reply_tx.send(response_bytes.clone()) {
            warn!(
                "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Failed to send response (receiver dropped)",
                self.config.address[5], self.config.address[4], self.config.address[3],
                self.config.address[2], self.config.address[1], self.config.address[0]
            );
        }

        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] protocol command finished: conn_id={}, control=0x{:02X}, response_len={}, elapsed={:?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            conn_id,
            control,
            response_bytes.len(),
            cmd_start.elapsed()
        );
    }

    /// 处理读数据命令（11H）
    async fn handle_read_command(
        &mut self,
        frame: crate::protocol::Frame,
        conn_id: u64,
    ) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, parse_di, ErrorInfoWord, Frame as ProtocolFrame,
        };

        let start = Instant::now();
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] handle_read_command start: conn_id={}, data_len={}, data={:02X?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            conn_id,
            frame.data.len(),
            &frame.data[..frame.data.len().min(16)]
        );

        // 解析DI
        let parse_start = Instant::now();
        let (di, _rest) = match parse_di(&frame.data) {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Failed to parse DI: {}, elapsed={:?}",
                    self.config.address[5], self.config.address[4], self.config.address[3],
                    self.config.address[2], self.config.address[1], self.config.address[0],
                    e,
                    parse_start.elapsed()
                );
                return encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::OTHER),
                );
            }
        };
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] DI parse ok: DI={:02X?}, parse_elapsed={:?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            di,
            parse_start.elapsed()
        );

        // 通过 VirtualMeter 查询数据（异步，支持负荷记录/历史冻结的数据库查询）
        let db_pool = self.config.db_pool.as_ref();
        let read_start = Instant::now();
        match self.meter.handle_read_async(&frame.data, db_pool).await {
            Ok(data_bytes) => {
                info!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] meter.handle_read_async complete: DI={:02X?}, data_len={}, read_elapsed={:?}",
                    self.config.address[5], self.config.address[4], self.config.address[3],
                    self.config.address[2], self.config.address[1], self.config.address[0],
                    di,
                    data_bytes.len(),
                    read_start.elapsed()
                );

                // 成功：构造响应帧 (DI + 数据)
                if data_bytes.len() <= MAX_FRAGMENT {
                    let response_frame = ProtocolFrame::response(frame.address, di, data_bytes);
                    let response = encode_frame(&response_frame);
                    info!(
                        "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] single-frame response built: len={}, total_elapsed={:?}",
                        self.config.address[5], self.config.address[4], self.config.address[3],
                        self.config.address[2], self.config.address[1], self.config.address[0],
                        response.len(),
                        start.elapsed()
                    );
                    return response;
                } else {
                    let total_data_len = data_bytes.len();
                    let first = data_bytes[..MAX_FRAGMENT].to_vec();
                    self.continuation_cache.insert(
                        (conn_id, di),
                        PendingContinuation {
                            full_data: data_bytes,
                            next_offset: MAX_FRAGMENT,
                            next_seq: 1,
                            created_at: Instant::now(),
                        },
                    );

                    let mut resp_data = di.to_vec();
                    resp_data.extend_from_slice(&first);
                    let response_frame = ProtocolFrame {
                        address: frame.address,
                        control: 0xB1, // 0x91 | 0x20（D5=1）
                        data: resp_data,
                    };
                    let response = encode_frame(&response_frame);
                    info!(
                        "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] fragmented response built: first_fragment_len={}, total_data_len={}, total_elapsed={:?}",
                        self.config.address[5], self.config.address[4], self.config.address[3],
                        self.config.address[2], self.config.address[1], self.config.address[0],
                        first.len(),
                        total_data_len,
                        start.elapsed()
                    );
                    return response;
                }
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] DIHandler read failed: {}, read_elapsed={:?}, total_elapsed={:?}",
                    self.config.address[5], self.config.address[4], self.config.address[3],
                    self.config.address[2], self.config.address[1], self.config.address[0],
                    err_msg,
                    read_start.elapsed(),
                    start.elapsed()
                );

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("未支持") || err_msg.contains("不存在") {
                    ErrorInfoWord::NO_DATA // 无请求数据
                } else if err_msg.contains("权限") || err_msg.contains("认证") {
                    ErrorInfoWord::PASSWORD_ERR // 密码错误/未授权
                } else {
                    ErrorInfoWord::OTHER // 其他错误
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理读后续数据命令（12H）
    ///
    /// 请求 DATA = DI(4) + SEQ(1)；响应 DATA = DI(4) + SEQ(1) + 数据片段。
    /// SEQ 从 1 递增，末片 D5=0，非末片 D5=1。
    async fn handle_follow_on_command(
        &mut self,
        frame: crate::protocol::Frame,
        conn_id: u64,
    ) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        let start = Instant::now();
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] handle_follow_on_command start: conn_id={}, data={:02X?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            conn_id,
            &frame.data[..frame.data.len().min(16)]
        );

        if frame.data.len() != 5 {
            return encode_error_response(
                frame.address,
                frame.control,
                ErrorInfoWord::new(ErrorInfoWord::OTHER),
            );
        }

        let di = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let seq = frame.data[4];
        let key = (conn_id, di);

        // 读取缓冲状态
        let lookup_start = Instant::now();
        let (start_offset, expected_seq, total_len) = match self.continuation_cache.get(&key) {
            Some(p) => (p.next_offset, p.next_seq, p.full_data.len()),
            None => {
                return encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::NO_DATA),
                );
            }
        };
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] follow_on cache lookup: DI={:02X?}, seq={}, expected_seq={}, total_len={}, lookup_elapsed={:?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            di,
            seq,
            expected_seq,
            total_len,
            lookup_start.elapsed()
        );

        // SEQ 校验
        if seq != expected_seq {
            self.continuation_cache.remove(&key);
            return encode_error_response(
                frame.address,
                frame.control,
                ErrorInfoWord::new(ErrorInfoWord::OTHER),
            );
        }

        let end = (start_offset + MAX_FRAGMENT).min(total_len);
        let fragment = match self.continuation_cache.get(&key) {
            Some(p) => p.full_data[start_offset..end].to_vec(),
            None => {
                return encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::NO_DATA),
                );
            }
        };
        let has_more = end < total_len;

        // 更新或清理缓冲
        if has_more {
            if let Some(p) = self.continuation_cache.get_mut(&key) {
                p.next_offset = end;
                p.next_seq = seq.wrapping_add(1);
                p.created_at = Instant::now();
            }
        } else {
            self.continuation_cache.remove(&key);
        }

        let mut resp_data = di.to_vec();
        resp_data.push(seq);
        resp_data.extend_from_slice(&fragment);

        let control = if has_more { 0xB2 } else { 0x92 };
        let response_frame = ProtocolFrame {
            address: frame.address,
            control,
            data: resp_data,
        };
        let response = encode_frame(&response_frame);
        info!(
            "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] follow_on response built: DI={:02X?}, seq={}, fragment_len={}, has_more={}, control=0x{:02X}, total_elapsed={:?}",
            self.config.address[5], self.config.address[4], self.config.address[3],
            self.config.address[2], self.config.address[1], self.config.address[0],
            di,
            seq,
            fragment.len(),
            has_more,
            control,
            start.elapsed()
        );
        response
    }

    /// 处理读通信地址命令（13H）
    ///
    /// 请求 DATA 为空；响应 C=93H、DATA=6 字节地址（不加 +33H 偏移）。异常无应答。
    async fn handle_read_comm_address(&self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::encode_frame_raw;

        if !frame.data.is_empty() {
            return vec![]; // 异常无应答
        }

        let addr = self.meter.address();
        encode_frame_raw(addr, 0x93, &addr)
    }

    /// 处理写通信地址命令（15H）
    ///
    /// DATA = 新地址(6) + 密码(4) + 操作者代码(4) = 14 字节；响应用新地址 C=95H。
    async fn handle_write_comm_address(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        if frame.data.len() != 14 {
            return encode_error_response(
                frame.address,
                frame.control,
                ErrorInfoWord::new(ErrorInfoWord::OTHER),
            );
        }

        let new_address = [
            frame.data[0],
            frame.data[1],
            frame.data[2],
            frame.data[3],
            frame.data[4],
            frame.data[5],
        ];
        let pa0 = frame.data[6];
        let password = [frame.data[7], frame.data[8], frame.data[9]];

        let level = (pa0 >> 4) & 0x0F;
        if !self.meter.state().password_config.verify(level, &password) || level > 4 {
            return encode_error_response(
                frame.address,
                frame.control,
                ErrorInfoWord::new(ErrorInfoWord::PASSWORD_ERR),
            );
        }

        let old_address = self.meter.address();
        self.meter.state_mut().address = new_address;
        self.config.address = new_address;

        // 联动 Registry 更新路由表（best-effort）
        if let Some(registry_tx) = &self.config.registry_tx {
            let _ = registry_tx.send(RegistryMsg::UpdateAddress {
                old: old_address,
                new: new_address,
            });
        }

        let response_frame = ProtocolFrame {
            address: new_address,
            control: 0x95,
            data: new_address.to_vec(),
        };
        encode_frame(&response_frame)
    }

    /// 处理广播校时命令（08H）
    ///
    /// 按设计方案实现：
    /// - DATA格式：6字节 `ss mm hh DD MM YY`（BCD编码，已做-33H还原）
    /// - 广播命令无应答
    /// - 立即将虚拟时钟调整为指定时间
    async fn handle_broadcast_time_sync(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::format::bcd_to_u64;
        use chrono::TimeZone;

        // 广播命令无应答，直接返回空
        if frame.data.len() != 6 {
            warn!(
                "[MeterActor] Invalid broadcast time sync data length: {}",
                frame.data.len()
            );
            return vec![];
        }

        // 解析时间：ss mm hh DD MM YY（BCD格式，低字节先传）
        // 注意：DATA区已经在decode_frame中做了-33H还原
        let ss = match bcd_to_u64(&frame.data[0..1]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse seconds: {}", e);
                return vec![];
            }
        };

        let mm = match bcd_to_u64(&frame.data[1..2]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse minutes: {}", e);
                return vec![];
            }
        };

        let hh = match bcd_to_u64(&frame.data[2..3]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse hours: {}", e);
                return vec![];
            }
        };

        let dd = match bcd_to_u64(&frame.data[3..4]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse day: {}", e);
                return vec![];
            }
        };

        let month = match bcd_to_u64(&frame.data[4..5]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse month: {}", e);
                return vec![];
            }
        };

        let yy = match bcd_to_u64(&frame.data[5..6]) {
            Ok(v) => v as u32,
            Err(e) => {
                warn!("[MeterActor] Failed to parse year: {}", e);
                return vec![];
            }
        };

        // 将2位年份转换为4位（假设20xx年）
        let year = 2000 + yy;

        // 构造DateTime
        match chrono::Utc
            .with_ymd_and_hms(year as i32, month, dd, hh, mm, ss)
            .single()
        {
            Some(dt) => {
                info!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Broadcast time sync to: {}",
                    self.config.address[5],
                    self.config.address[4],
                    self.config.address[3],
                    self.config.address[2],
                    self.config.address[1],
                    self.config.address[0],
                    dt.format("%Y-%m-%d %H:%M:%S")
                );

                // 按附录A.4生成校时记录（03-31-01，数据=校时前时间+校时后时间）
                {
                    let state = self.meter.state_mut();
                    let before = state.virtual_time;
                    let mut data = encode_645_datetime(&before);
                    data.extend(encode_645_datetime(&dt));
                    state.add_event_record(0x31, 0x01, before, data);

                    // 设置虚拟时钟
                    state.virtual_time = dt;
                }
            }
            None => {
                warn!(
                    "[MeterActor] Invalid datetime: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    year, month, dd, hh, mm, ss
                );
            }
        }

        // 广播命令无应答
        vec![]
    }

    /// 处理写数据命令（14H）
    ///
    /// 按设计方案 4.5.2 节实现：
    /// DATA格式：DI(4) + 密码(4) + 操作者代码(4) + 数据
    ///
    /// 架构说明：
    /// - MeterActor只负责解析帧格式和路由
    /// - 密码验证、权限检查、数据写入都委托给VirtualMeter
    async fn handle_write_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 最小长度检查：DI(4) + 密码(4) + 操作者代码(4) + 数据(至少1字节) = 13字节
        if frame.data.len() < 13 {
            warn!(
                "[MeterActor] Write command data too short: {} bytes",
                frame.data.len()
            );
            return encode_error_response(
                frame.address,
                frame.control,
                ErrorInfoWord::new(ErrorInfoWord::OTHER),
            );
        }

        info!("[MeterActor] Write command data: {:02X?}", frame.data);

        // 委托给VirtualMeter处理（包括密码验证、权限检查、数据写入）
        match self.meter.handle_write_command(&frame.data) {
            Ok(operator_code) => {
                let di = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
                info!("[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Write DI {:02X}{:02X}{:02X}{:02X} success, operator={:02X?}",
                    self.config.address[5], self.config.address[4],
                    self.config.address[3], self.config.address[2],
                    self.config.address[1], self.config.address[0],
                    di[3], di[2], di[1], di[0],
                    operator_code);

                // TODO: 生成编程记录事件（03 30 xx类）

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x94, // 写数据正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                let di = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
                warn!(
                    "[MeterActor] Write DI {:02X}{:02X}{:02X}{:02X} failed: {}",
                    di[3], di[2], di[1], di[0], err_msg
                );

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("密码") || err_msg.contains("权限") {
                    ErrorInfoWord::PASSWORD_ERR
                } else if err_msg.contains("只读")
                    || err_msg.contains("不支持写入")
                    || err_msg.contains("不存在")
                {
                    ErrorInfoWord::NO_DATA
                } else {
                    ErrorInfoWord::OTHER
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理冻结命令（16H）
    async fn handle_freeze_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理冻结命令
        match self.meter.handle_freeze_command(&frame.data) {
            Ok(()) => {
                info!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Freeze command success",
                    self.config.address[5],
                    self.config.address[4],
                    self.config.address[3],
                    self.config.address[2],
                    self.config.address[1],
                    self.config.address[0]
                );

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x96, // 冻结命令正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Freeze command failed: {}", err_msg);

                encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::OTHER),
                )
            }
        }
    }

    /// 处理更改通信速率命令（17H）
    async fn handle_change_baudrate_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理速率变更
        match self.meter.handle_change_baudrate_command(&frame.data) {
            Ok(()) => {
                info!("[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Baudrate change success, new code: 0x{:02X}",
                    self.config.address[5], self.config.address[4],
                    self.config.address[3], self.config.address[2],
                    self.config.address[1], self.config.address[0],
                    frame.data[0]);

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x97, // 更改通信速率正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Change baudrate failed: {}", err_msg);

                encode_error_response(
                    frame.address,
                    frame.control,
                    ErrorInfoWord::new(ErrorInfoWord::OTHER),
                )
            }
        }
    }

    /// 处理修改密码命令（18H）
    async fn handle_change_password_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理密码修改
        match self.meter.handle_change_password_command(&frame.data) {
            Ok(()) => {
                info!(
                    "[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Password change success",
                    self.config.address[5],
                    self.config.address[4],
                    self.config.address[3],
                    self.config.address[2],
                    self.config.address[1],
                    self.config.address[0]
                );

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x98, // 修改密码正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Change password failed: {}", err_msg);

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("密码") {
                    ErrorInfoWord::PASSWORD_ERR
                } else if err_msg.contains("权限") {
                    ErrorInfoWord::PASSWORD_ERR
                } else {
                    ErrorInfoWord::OTHER
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理需量清零命令（19H）
    async fn handle_demand_clear_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理需量清零
        match self.meter.handle_demand_clear_command(&frame.data) {
            Ok(operator_code) => {
                info!("[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Demand clear success, operator={:02X?}",
                    self.config.address[5], self.config.address[4],
                    self.config.address[3], self.config.address[2],
                    self.config.address[1], self.config.address[0],
                    operator_code);

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x99, // 需量清零正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Demand clear failed: {}", err_msg);

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("密码") || err_msg.contains("权限") {
                    ErrorInfoWord::PASSWORD_ERR
                } else {
                    ErrorInfoWord::OTHER
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理电表清零命令（1AH）
    async fn handle_meter_clear_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理电表清零
        match self.meter.handle_meter_clear_command(&frame.data) {
            Ok(operator_code) => {
                info!("[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Meter clear success, operator={:02X?}",
                    self.config.address[5], self.config.address[4],
                    self.config.address[3], self.config.address[2],
                    self.config.address[1], self.config.address[0],
                    operator_code);

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x9A, // 电表清零正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Meter clear failed: {}", err_msg);

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("密码") || err_msg.contains("权限") {
                    ErrorInfoWord::PASSWORD_ERR
                } else {
                    ErrorInfoWord::OTHER
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理事件清零命令（1BH）
    async fn handle_event_clear_command(&mut self, frame: crate::protocol::Frame) -> Vec<u8> {
        use crate::protocol::{
            encode_error_response, encode_frame, ErrorInfoWord, Frame as ProtocolFrame,
        };

        // 调用VirtualMeter处理事件清零
        match self.meter.handle_event_clear_command(&frame.data) {
            Ok(operator_code) => {
                info!("[MeterActor {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}] Event clear success, operator={:02X?}",
                    self.config.address[5], self.config.address[4],
                    self.config.address[3], self.config.address[2],
                    self.config.address[1], self.config.address[0],
                    operator_code);

                // 成功：返回正常应答（L=0，无DATA）
                let response_frame = ProtocolFrame {
                    address: frame.address,
                    control: 0x9B, // 事件清零正常应答
                    data: vec![],  // 无数据
                };

                encode_frame(&response_frame)
            }
            Err(err_msg) => {
                // 失败：返回错误响应
                warn!("[MeterActor] Event clear failed: {}", err_msg);

                // 根据错误消息判断错误类型
                let error_code = if err_msg.contains("密码") || err_msg.contains("权限") {
                    ErrorInfoWord::PASSWORD_ERR
                } else {
                    ErrorInfoWord::OTHER
                };

                encode_error_response(frame.address, frame.control, ErrorInfoWord::new(error_code))
            }
        }
    }

    /// 处理管理命令
    ///
    /// 支持的管理命令：
    /// - GetSnapshot: 获取表状态快照
    /// - SetVirtualTime: 设置虚拟时间
    /// - SetEnergy: 设置电能值
    /// - SetLoadModel: 设置负荷模型
    /// - TriggerFreeze: 触发冻结
    /// - ForceFlushEnergy: 强制刷新电能寄存器
    /// - GetAddress: 获取地址
    /// - Shutdown: 优雅关闭
    async fn on_admin_command(
        &mut self,
        cmd: AdminCommand,
        reply_tx: oneshot::Sender<Result<String, String>>,
    ) {
        let result = match cmd {
            AdminCommand::GetSnapshot => {
                // 返回表状态的JSON快照
                let state = self.meter.state();
                let simulation = crate::snapshot::MeterSnapshot::from_state(
                    state,
                    self.meter.load_model_config(),
                    true,
                )
                .simulation;
                let snapshot = serde_json::json!({
                    "address": format_address(&state.address),
                    "virtual_time": state.virtual_time.to_rfc3339(),
                    "voltage_a": state.voltage_a,
                    "voltage_b": state.voltage_b,
                    "voltage_c": state.voltage_c,
                    "current_a": state.current_a,
                    "current_b": state.current_b,
                    "current_c": state.current_c,
                    "frequency": state.frequency,
                    "baudrate": state.baudrate,
                    "simulation": simulation,
                });
                Ok(snapshot.to_string())
            }

            AdminCommand::SetVirtualTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            } => {
                use chrono::{Utc, TimeZone};
                match Utc
                    .with_ymd_and_hms(
                        year as i32,
                        month as u32,
                        day as u32,
                        hour as u32,
                        minute as u32,
                        second as u32,
                    )
                    .single()
                {
                    Some(dt) => {
                        self.meter.state_mut().virtual_time = dt;
                        Ok(format!(
                            "Virtual time set to: {}",
                            dt.format("%Y-%m-%d %H:%M:%S")
                        ))
                    }
                    None => Err("Invalid datetime".to_string()),
                }
            }

            AdminCommand::SetEnergy {
                energy_type,
                rate,
                value,
            } => {
                use crate::simulation::state::EnergyType;
                let et = match energy_type {
                    1 => EnergyType::ForwardActive,
                    2 => EnergyType::ReverseActive,
                    3 => EnergyType::ForwardReactive,
                    4 => EnergyType::ReverseReactive,
                    _ => {
                        let _ = reply_tx.send(Err("Invalid energy_type".to_string()));
                        return;
                    }
                };

                self.meter.state_mut().set_energy(et, rate, value);
                Ok(format!(
                    "Energy set: type={}, rate={:?}, value={:.3} kWh",
                    energy_type, rate, value
                ))
            }

            AdminCommand::SetLoadModel {
                voltage,
                current,
                power_factor,
            } => self
                .meter
                .set_load_model(voltage, current, power_factor)
                .map(|_| {
                    format!("Load model set: {voltage:.1} V, {current:.3} A, PF {power_factor:.3}")
                }),

            AdminCommand::ApplySimulationConfig { config } => {
                // clone for persistence after apply
                let config_for_persist = config.clone();
                let apply_res = self.meter.apply_simulation_config(config);
                if apply_res.is_ok() {
                    if let Some(pool) = &self.config.db_pool {
                        let addr = format_address(&self.config.address);
                        if let Err(e) = crate::persistence::PersistenceWorker::save_simulation_config(
                            pool,
                            &addr,
                            &config_for_persist,
                        )
                        .await
                        {
                            warn!("[MeterActor {}] Failed to persist simulation config: {}", addr, e);
                        }
                    }
                }
                apply_res.map(|_| "Simulation configuration applied".to_string())
            }

            AdminCommand::ChangePassword {
                level,
                new_password,
            } => {
                if level > 9 {
                    Err("Invalid password level (0-9)".to_string())
                } else {
                    // new_password = PA0 P0 P1 P2，取低 3 字节作为密码值
                    let pwd = [new_password[1], new_password[2], new_password[3]];
                    self.meter
                        .state_mut()
                        .password_config
                        .set_password(level, &pwd);
                    Ok(format!("Password level {} updated", level))
                }
            }

            AdminCommand::SetBaudrate { baudrate } => match baudrate {
                0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 => {
                    self.meter.state_mut().baudrate = baudrate;
                    Ok(format!("Baudrate set to 0x{:02X}", baudrate))
                }
                _ => Err("Invalid baudrate code".to_string()),
            },

            AdminCommand::ClearMaxDemand => {
                self.meter.state_mut().max_demand = 0.0;
                self.meter.state_mut().max_demand_time = chrono::Utc::now();
                Ok("Max demand cleared".to_string())
            }

            AdminCommand::ClearMeter => {
                let state = self.meter.state_mut();
                state.energy_registers.clear();
                state.max_demand = 0.0;
                state.max_demand_time = chrono::Utc::now();
                Ok("Meter cleared".to_string())
            }

            AdminCommand::SetTouConfig { time_slots } => {
                use crate::simulation::state::TimeSlot;
                if time_slots.len() > 14 {
                    Err("Too many time slots (max 14)".to_string())
                } else {
                    let slots: Vec<TimeSlot> = time_slots
                        .into_iter()
                        .map(|(h, m, r)| TimeSlot {
                            start_hour: h,
                            start_minute: m,
                            rate_number: r,
                        })
                        .collect();
                    let state = self.meter.state_mut();
                    state.tou_config.day_table_1.slots = slots;
                    state.num_time_slots = state.tou_config.day_table_1.slots.len() as u8;
                    Ok("TOU config updated".to_string())
                }
            }

            AdminCommand::ApplyFreezeConfig {
                timed_mode,
                instant_mode,
                appointment_mode,
                hourly_mode,
                daily_mode,
                daily_time,
                hourly_start,
                hourly_interval_min,
                appointment_time,
            } => {
                {
                    let state = self.meter.state_mut();
                    state.freeze_config.timed_freeze_mode = timed_mode;
                    state.freeze_config.instant_freeze_mode = instant_mode;
                    state.freeze_config.appointment_freeze_mode = appointment_mode;
                    state.hourly_freeze_mode = hourly_mode;
                    state.daily_freeze_mode = daily_mode;
                    state.daily_freeze_time = daily_time;
                    state.hourly_freeze_start = hourly_start;
                    state.hourly_freeze_interval_min = hourly_interval_min;
                    state.appointment_freeze_time = appointment_time;
                    // 新的约定冻结时间允许再次触发
                    state.appointment_freeze_fired = false;
                }
                // 持久化冻结配置
                if let Some(pool) = &self.config.db_pool {
                    let addr = format_address(&self.config.address);
                    if let Err(e) = crate::persistence::PersistenceWorker::save_freeze_config(
                        pool,
                        &addr,
                        timed_mode,
                        instant_mode,
                        appointment_mode,
                        hourly_mode,
                        daily_mode,
                        daily_time,
                        hourly_start,
                        hourly_interval_min,
                        appointment_time,
                    )
                    .await
                    {
                        warn!("[MeterActor {}] Failed to persist freeze config: {}", addr, e);
                    }
                }
                Ok("Freeze configuration applied".to_string())
            }

            AdminCommand::ApplySettlementDays { days, hours } => {
                if days.iter().any(|&d| d > 28) {
                    Err("结算日 DD 必须在 1~28 之间（0 表示不启用）".to_string())
                } else if hours.iter().any(|&h| h > 23) {
                    Err("结算日 hh 必须在 0~23 之间".to_string())
                } else {
                    let state = self.meter.state_mut();
                    state.settlement_days = days;
                    state.settlement_hours = hours;

                    // 持久化结算日
                    if let Some(pool) = &self.config.db_pool {
                        let addr = format_address(&self.config.address);
                        if let Err(e) = crate::persistence::PersistenceWorker::save_settlement_days(
                            pool,
                            &addr,
                            days,
                            hours,
                        )
                        .await
                        {
                            warn!("[MeterActor {}] Failed to persist settlement days: {}", addr, e);
                        }
                    }

                    Ok("Settlement days applied".to_string())
                }
            }

            AdminCommand::ApplyLoadRecordConfig {
                mode_word,
                start_time,
                intervals,
            } => {
                let state = self.meter.state_mut();
                state.load_record_config.mode_word = mode_word;
                state.load_record_start_time = start_time;
                state.load_record_config.intervals = intervals;

                // 持久化负荷记录配置
                if let Some(pool) = &self.config.db_pool {
                    let addr: String = format_address(&self.config.address);
                    if let Err(e) = crate::persistence::PersistenceWorker::save_load_record_config(
                        pool,
                        &addr,
                        mode_word,
                        start_time,
                        intervals,
                    )
                    .await
                    {
                        warn!("[MeterActor {}] Failed to persist load record config: {}", addr, e);
                    }
                }

                Ok("Load record configuration applied".to_string())
            }

            AdminCommand::InjectFault {
                event_type,
                phase,
                active,
            } => {
                // 相别故障需要 phase 1~3；系统级/记录类事件 phase 必须为 0
                const PHASE_FAULTS: [u8; 6] = [0x01, 0x02, 0x03, 0x04, 0x0B, 0x0C];
                const SYSTEM_FAULTS: [u8; 8] = [0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0F, 0x32];
                if PHASE_FAULTS.contains(&event_type) {
                    if !(1..=3).contains(&phase) {
                        Err("相别故障 phase 必须是 1/2/3 (A/B/C)".to_string())
                    } else {
                        self.meter.set_forced_fault(event_type, phase, active);
                        Ok(format!(
                            "Fault injection: type={:02X}, phase={}, active={}",
                            event_type, phase, active
                        ))
                    }
                } else if SYSTEM_FAULTS.contains(&event_type) {
                    if phase != 0 {
                        Err("系统级事件 phase 必须是 0".to_string())
                    } else {
                        self.meter.set_forced_fault(event_type, 0, active);
                        Ok(format!(
                            "Fault injection: type={:02X}, active={}",
                            event_type, active
                        ))
                    }
                } else {
                    Err("event_type 必须是 01~0F 故障类或 32 清零记录".to_string())
                }
            }

            AdminCommand::TriggerFreeze { freeze_type } => {
                use crate::simulation::state::{FreezeTrigger, FreezeType};
                let trigger = match freeze_type {
                    0 => FreezeTrigger::Timed,
                    1 => FreezeTrigger::Instant,
                    _ => {
                        let _ = reply_tx.send(Err("Invalid freeze_type".to_string()));
                        return;
                    }
                };

                // 手动设置pending标志，下次tick会处理
                let ft = match freeze_type {
                    0 => FreezeType::Timed,
                    1 => FreezeType::Instant(0),
                    _ => unreachable!(),
                };
                self.meter.state_mut().pending_freeze_triggers.push(ft);
                Ok(format!("Freeze triggered: type={}", freeze_type))
            }

            AdminCommand::ForceFlushEnergy => {
                // TODO: 添加公开方法强制flush
                Err("ForceFlushEnergy not yet implemented".to_string())
            }

            AdminCommand::GetAddress => {
                let addr = self.meter.address();
                Ok(format_address(&addr))
            }

            AdminCommand::Shutdown => {
                self.graceful_shutdown().await;
                Ok("Shutting down...".to_string())
            },

            AdminCommand::SaveState => {
                // TODO: 触发最终flush
                // 1. 强制flush电能寄存器
                // 2. 保存虚拟时钟到meters表
                // 3. 等待PersistenceWorker完成
                Err("SaveState not yet fully implemented".to_string())
            }

            AdminCommand::LoadFreezeHistory => self.load_freeze_history().await,

            AdminCommand::LoadLoadProfileHistory { max_records } => {
                self.load_load_profile_history(max_records).await
            }
        };

        // 命令执行后立即推送快照（反映最新状态）
        self.push_snapshot();

        let _ = reply_tx.send(result);
    }

    /// 加载冻结历史：合并内存环形缓冲 + 数据库历史，去重后按时间倒序，
    /// 序列化为 JSON 字符串返回（供 `AdminCommand::LoadFreezeHistory` 使用）。
    ///
    /// - 内存部分：遍历全部触发类型的环形缓冲，按缓冲区内位置计算真实序号
    ///   （01=最近一次，与协议 DI0 语义一致）。
    /// - 数据库部分：仅在启用了持久化（`db_pool` 存在）时查询，查询失败只是
    ///   缺少更早的数据，不影响内存部分的展示，因此不会让整个命令失败。
    /// - 去重 key 用 `(trigger, snapshot_time_ms)` 而不是 occurrence_idx——
    ///   两边的序号编码方式不完全一致（内存按位置、数据库按挪号），时间戳才是
    ///   稳定标识同一次冻结事件的字段。
    async fn load_freeze_history(&mut self) -> Result<String, String> {
        use crate::simulation::state::FreezeTrigger;
        use crate::snapshot::FreezeSnapshotSummary;

        const ALL_TRIGGERS: [FreezeTrigger; 7] = [
            FreezeTrigger::Timed,
            FreezeTrigger::Instant,
            FreezeTrigger::TimeZoneSwitch,
            FreezeTrigger::DayTableSwitch,
            FreezeTrigger::Hourly,
            FreezeTrigger::Daily,
            FreezeTrigger::LadderSwitch,
        ];

        let address = format_address(&self.config.address);
        let state = self.meter.state();

        // 1. 内存环形缓冲
        let mut merged: Vec<FreezeSnapshotSummary> = ALL_TRIGGERS
            .iter()
            .flat_map(|trigger| {
                let trigger_label = format!("{:?}", trigger);
                state
                    .get_all_freeze_snapshots(*trigger)
                    .into_iter()
                    .enumerate()
                    .map(move |(position, snap)| {
                        FreezeSnapshotSummary::from_freeze_data(
                            trigger_label.clone(),
                            (position + 1) as u8,
                            snap.snapshot_time.timestamp_millis(),
                            &snap.data,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        // 2. 数据库历史（失败不阻断加载，只是没有更早的数据）
        if let Some(pool) = &self.config.db_pool {
            match crate::persistence::PersistenceWorker::query_freeze_history(pool, &address, 500)
                .await
            {
                Ok(rows) => {
                    for row in rows {
                        match serde_json::from_value::<crate::simulation::FreezeData>(
                            row.payload.clone(),
                        ) {
                            Ok(data) => {
                                let trigger_label = FreezeTrigger::from_di2(row.trigger_type)
                                    .map(|t| format!("{:?}", t))
                                    .unwrap_or_else(|| {
                                        format!("Unknown({:02X})", row.trigger_type)
                                    });
                                merged.push(FreezeSnapshotSummary::from_freeze_data(
                                    trigger_label,
                                    row.occurrence_idx,
                                    row.snapshot_time.timestamp_millis(),
                                    &data,
                                ));
                            }
                            Err(e) => {
                                warn!(
                                    "[MeterActor {}] 冻结快照 payload 解析失败: {}",
                                    address, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("[MeterActor {}] 冻结历史查询失败: {}", address, e);
                }
            }
        }

        // 3. 去重 + 按时间倒序
        let mut seen = std::collections::HashSet::new();
        merged.retain(|s| seen.insert((s.trigger.clone(), s.snapshot_time_ms)));
        merged.sort_by_key(|s| std::cmp::Reverse(s.snapshot_time_ms));

        serde_json::to_string(&merged).map_err(|e| e.to_string())
    }

    /// 加载最近的负荷记录（供 `AdminCommand::LoadLoadProfileHistory` 使用）
    ///
    /// 负荷记录落库后不维护内存历史（每类各自独立采样间隔，靠数据库查询
    /// 而非环形缓冲，见 `0003_load_records_json.sql` 的设计说明），所以
    /// 这里只查数据库；没有开启持久化（`db_pool` 为 `None`）时返回空列表
    /// 而非报错，与"未启用持久化=没有历史数据可看"的语义一致。
    async fn load_load_profile_history(&mut self, max_records: u32) -> Result<String, String> {
        use crate::simulation::state::LoadRecordData;
        use crate::snapshot::LoadRecordSnapshot;

        let address = format_address(&self.config.address);

        let Some(pool) = &self.config.db_pool else {
            return serde_json::to_string(&Vec::<LoadRecordSnapshot>::new())
                .map_err(|e| e.to_string());
        };

        // 取最近 max_records 条负荷记录——不设时间窗口（见
        // `PersistenceWorker::query_recent_load_records` 的说明：仿真通常
        // 开倍速，虚拟时钟可能已经跑到比真实时间靠后，用真实 `Utc::now()`
        // 划时间窗口会把这些记录全部过滤掉）。
        let rows = crate::persistence::PersistenceWorker::query_recent_load_records(
            pool,
            &address,
            max_records,
        )
        .await
        .map_err(|e| format!("负荷记录查询失败: {e}"))?;

        let snapshots: Vec<LoadRecordSnapshot> = rows
            .into_iter()
            .filter_map(|row| {
                // 反序列化JSON payload
                let data: LoadRecordData = serde_json::from_value(row.payload).ok()?;

                // 使用 LoadRecordSnapshot::from_load_record_data 统一转换
                Some(LoadRecordSnapshot::from_load_record_data(
                    row.class_id,
                    row.sample_time.timestamp_millis(),
                    &data,
                ))
            })
            .collect();

        serde_json::to_string(&snapshots).map_err(|e| e.to_string())
    }

    /// 执行优雅关闭
    ///
    /// 在Actor退出前执行：
    /// 1. 强制flush电能寄存器
    /// 2. 保存虚拟时钟
    /// 3. 等待持久化完成
    async fn graceful_shutdown(&mut self) {
        let address_str = format_address(&self.config.address);

        info!(
            "[MeterActor {}] Performing graceful shutdown...",
            address_str
        );

        // 强制flush电能寄存器和虚拟时间
        // 这里不仅发送到 PersistenceWorker 队列，还要直接写数据库确保数据保存
        
        // 方案1：通过 PersistenceWorker 队列（异步，可能丢失）
        if let Some(_persist_tx) = self.meter.force_flush_energy() {
            debug!("[MeterActor {}] Sent flush request to PersistenceWorker", address_str);
        }

        // 方案2：直接写数据库（同步，保证成功）
        if let Some(pool) = &self.config.db_pool {
            // 保存虚拟时钟
            match self.meter.save_virtual_time(pool).await {
                Ok(_) => {
                    debug!("[MeterActor {}] Saved virtual time to database", address_str);
                }
                Err(e) => {
                    warn!(
                        "[MeterActor {}] Failed to save virtual time: {}",
                        address_str, e
                    );
                }
            }
        }

        info!("[MeterActor {}] Graceful shutdown completed", address_str);
    }
}

/// MeterActor 句柄（用于外部与 Actor 交互）
#[derive(Clone)]
pub struct MeterActorHandle {
    /// 命令发送器
    pub cmd_tx: mpsc::Sender<EngineMsg>,

    /// 电表地址
    pub address: [u8; 6],
}

impl MeterActorHandle {
    /// 创建新的句柄
    pub fn new(cmd_tx: mpsc::Sender<EngineMsg>, address: [u8; 6]) -> Self {
        Self { cmd_tx, address }
    }

    /// 发送引擎消息（通用接口）
    pub async fn send_engine_msg(&self, msg: EngineMsg) -> Result<(), String> {
        self.cmd_tx
            .send(msg)
            .await
            .map_err(|_| "Failed to send message (actor stopped)".to_string())
    }

    /// 发送协议命令（从原始字节）
    pub async fn send_protocol_command(&self, frame_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        use crate::protocol::decode_frame;

        // 解码帧
        let frame =
            decode_frame(&frame_bytes).map_err(|e| format!("Failed to decode frame: {}", e))?;

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel();

        self.cmd_tx
            .send(EngineMsg::ProtocolCommand {
                conn_id: 0,
                frame,
                reply_tx,
            })
            .await
            .map_err(|_| "Failed to send command (actor stopped)".to_string())?;

        reply_rx
            .recv()
            .await
            .ok_or_else(|| "Failed to receive response (actor dropped reply)".to_string())
    }

    /// 发送管理命令
    pub async fn send_admin_command(&self, cmd: AdminCommand) -> Result<String, String> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.cmd_tx
            .send(EngineMsg::AdminCommand { cmd, reply_tx })
            .await
            .map_err(|_| "Failed to send command (actor stopped)".to_string())?;

        reply_rx
            .await
            .map_err(|_| "Failed to receive response (actor dropped reply)".to_string())?
    }
}

/// 校时记录用时间编码（ss mm hh DD MM YY，6字节BCD）
fn encode_645_datetime(dt: &chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    let to_bcd = |v: u8| ((v / 10) << 4) | (v % 10);
    vec![
        to_bcd(dt.second() as u8),
        to_bcd(dt.minute() as u8),
        to_bcd(dt.hour() as u8),
        to_bcd(dt.day() as u8),
        to_bcd(dt.month() as u8),
        to_bcd((dt.year() % 100) as u8),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{decode_frame, Frame};
    use crate::simulation::{VirtualMeter, VirtualMeterConfig};
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn apply_simulation_configuration_updates_actor_snapshot() {
        use crate::simulation::{LoadModelConfig, LoadProfile, SimulationConfig};

        let (_tick_tx, tick_rx) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let actor = MeterActor::new(
            VirtualMeter::new(config),
            tick_rx,
            cmd_rx,
            MeterActorConfig {
                address,
                ..Default::default()
            },
        );
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move { actor.run().await });

        let simulation = SimulationConfig {
            load_model: LoadModelConfig {
                profile: LoadProfile::Fixed(0.4),
                voltage_noise_v: 0.0,
                frequency_noise_hz: 0.0,
                power_factor_noise: 0.0,
                power_factor_min: 0.0,
                power_factor_max: 1.0,
                phase_current_factors: [1.0, 1.0, 1.0],
            },
            rated_voltage: 230.0,
            rated_current: 40.0,
            rated_frequency: 50.0,
            power_factor: 0.9,
            meter_constant: 800,
            demand_period_minutes: 10,
            time_scale: 6.0,
        };
        assert!(handle
            .send_admin_command(AdminCommand::ApplySimulationConfig { config: simulation })
            .await
            .is_ok());

        let snapshot = handle
            .send_admin_command(AdminCommand::GetSnapshot)
            .await
            .expect("snapshot request must succeed");
        let json: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(json["simulation"]["load_profile"], "Fixed");
        assert_eq!(json["simulation"]["rated_voltage_v"], 230.0);
        assert_eq!(json["simulation"]["rated_current_a"], 40.0);
        assert_eq!(json["simulation"]["time_scale"], 6.0);

        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_basic() {
        // 创建 tick 广播通道
        let (tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address: config.address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, config.address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：获取地址
        let result = handle.send_admin_command(AdminCommand::GetAddress).await;
        assert!(result.is_ok());
        println!("Address: {}", result.unwrap());

        // 发送几个 tick
        for _ in 0..5 {
            tick_tx
                .send(TickMsg {
                    wall_elapsed: Duration::from_secs(1),
                    time_scale: 1.0,
                })
                .unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        // 测试：获取快照
        let result = handle.send_admin_command(AdminCommand::GetSnapshot).await;
        assert!(result.is_ok());
        println!("Snapshot: {}", result.unwrap());

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_read_voltage() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：读取A相电压 (DI = 02-01-01-00)
        let di = [0x00, 0x01, 0x01, 0x02];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读电压命令: {:02X?}", frame_bytes);

        let response_bytes = handle.send_protocol_command(frame_bytes).await;
        assert!(response_bytes.is_ok());

        let response = response_bytes.unwrap();
        println!("收到响应: {:02X?}", response);

        // 解码响应帧
        let response_frame = decode_frame(&response);
        assert!(response_frame.is_ok());

        let resp = response_frame.unwrap();
        println!("响应控制码: 0x{:02X}", resp.control);
        println!("响应数据长度: {}", resp.data.len());

        // 验证响应：控制码应该是 0x91（正常应答）
        assert_eq!(resp.control, 0x91, "应该收到正常应答");

        // 验证数据：DI(4字节) + 电压数据(2字节BCD)
        assert!(resp.data.len() >= 6, "响应数据应包含DI+电压值");

        // 验证DI回显
        assert_eq!(&resp.data[0..4], &di, "响应应回显请求的DI");

        // 解析电压值 (BCD格式，2字节，单位0.1V)
        let voltage_bytes = &resp.data[4..6];
        let voltage_bcd = u16::from_le_bytes([voltage_bytes[0], voltage_bytes[1]]);
        println!("电压BCD值: 0x{:04X}", voltage_bcd);

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_read_energy() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：读取正向有功总电能 (DI = 00-01-00-00)
        let di = [0x00, 0x00, 0x01, 0x00];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读电能命令: {:02X?}", frame_bytes);

        let response_bytes = handle.send_protocol_command(frame_bytes).await;
        assert!(response_bytes.is_ok());

        let response = response_bytes.unwrap();
        println!("收到响应: {:02X?}", response);

        // 解码响应帧
        let response_frame = decode_frame(&response);
        assert!(response_frame.is_ok());

        let resp = response_frame.unwrap();
        println!("响应控制码: 0x{:02X}", resp.control);
        println!("响应数据长度: {}", resp.data.len());

        // 验证响应
        assert_eq!(resp.control, 0x91, "应该收到正常应答");
        assert!(resp.data.len() >= 8, "响应数据应包含DI+电能值");

        // 验证DI回显
        assert_eq!(&resp.data[0..4], &di, "响应应回显请求的DI");

        // 解析电能值 (BCD格式，4字节，单位0.01kWh)
        let energy_bytes = &resp.data[4..8];
        println!("电能数据: {:02X?}", energy_bytes);

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_read_invalid_di() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：读取不存在的DI (FF-FF-FF-FF)
        let di = [0xFF, 0xFF, 0xFF, 0xFF];
        let read_frame = Frame::read(address, di);
        let frame_bytes = encode_frame(&read_frame);

        println!("发送读取无效DI命令: {:02X?}", frame_bytes);

        let response_bytes = handle.send_protocol_command(frame_bytes).await;
        assert!(response_bytes.is_ok());

        let response = response_bytes.unwrap();
        println!("收到响应: {:02X?}", response);

        // 解码响应帧
        let response_frame = decode_frame(&response);
        assert!(response_frame.is_ok());

        let resp = response_frame.unwrap();
        println!("响应控制码: 0x{:02X}", resp.control);

        // 验证响应：应该是异常应答 (0xD1)
        assert_eq!(resp.control, 0xD1, "应该收到异常应答");

        // 验证错误码
        assert_eq!(resp.data.len(), 1, "异常应答应包含1字节错误码");
        let error_code = resp.data[0];
        println!("错误码: 0x{:02X}", error_code);

        // 错误码应该是 NO_DATA (0x02)
        assert_eq!(error_code, 0x02, "错误码应该是NO_DATA");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_broadcast_time_sync() {
        use crate::protocol::{encode_frame, format::u64_to_bcd};
        use chrono::Timelike;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：广播校时到 2024-06-15 10:30:45
        // DATA格式：ss mm hh DD MM YY（BCD，低字节先传）
        let mut data = Vec::new();
        data.extend_from_slice(&u64_to_bcd(45, 1)); // ss = 45秒
        data.extend_from_slice(&u64_to_bcd(30, 1)); // mm = 30分
        data.extend_from_slice(&u64_to_bcd(10, 1)); // hh = 10时
        data.extend_from_slice(&u64_to_bcd(15, 1)); // DD = 15日
        data.extend_from_slice(&u64_to_bcd(6, 1)); // MM = 6月
        data.extend_from_slice(&u64_to_bcd(24, 1)); // YY = 24年（2024）

        // 构造广播校时帧（控制码08H）
        let frame = Frame {
            address,
            control: 0x08,
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送广播校时命令: {:02X?}", frame_bytes);

        // 发送命令（使用内部方法，因为广播命令无应答）
        use crate::protocol::decode_frame;
        let decoded = decode_frame(&frame_bytes).unwrap();

        let (reply_tx, _reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待处理完成
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // 验证：通过GetSnapshot查看虚拟时间是否更新
        let result = handle.send_admin_command(AdminCommand::GetSnapshot).await;
        assert!(result.is_ok());

        let snapshot = result.unwrap();
        println!("Snapshot: {}", snapshot);

        // 解析JSON验证时间
        let json: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let vtime = json["virtual_time"].as_str().unwrap();
        println!("Virtual time after sync: {}", vtime);

        // 验证时间包含正确的日期和时间
        assert!(vtime.contains("2024-06-15"));
        assert!(vtime.contains("10:30:45"));

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_write_command() {
        use crate::protocol::{encode_frame, format::u64_to_bcd};

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        //  测试：写入波特率
        // DI码：04-00-07-03 表示通信口1通信速率特征字
        // 在DL/T645中，DI码表示为 DI3-DI2-DI1-DI0
        // 04-00-07-03 = DI3=04, DI2=00, DI1=07, DI0=03
        // 传输时按小端序：[DI0, DI1, DI2, DI3]
        let di = [0x03, 0x07, 0x00, 0x04]; // 04-00-07-03 通信口1速率特征字
        let password = [0x00, 0x00, 0x00, 0x00]; // PA0=00(0级权限) + 3字节密码=000000
        let operator_code = [0x01, 0x02, 0x03, 0x04]; // 操作者代码（BCD）
        let write_data = [0x04]; // 波特率=04（2400bps）

        let mut data = Vec::new();
        data.extend_from_slice(&di);
        data.extend_from_slice(&password);
        data.extend_from_slice(&operator_code);
        data.extend_from_slice(&write_data);

        let frame = Frame {
            address,
            control: 0x14, // 写数据
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送写命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=94H（写数据正常应答）
        assert_eq!(response_frame.control, 0x94, "控制码应该是0x94");

        // 验证数据长度为0（写命令正常应答无数据）
        assert_eq!(response_frame.data.len(), 0, "写命令正常应答应该无数据");

        // 验证状态已更新
        let result = handle.send_admin_command(AdminCommand::GetSnapshot).await;
        assert!(result.is_ok());

        let snapshot = result.unwrap();
        println!("Snapshot after write: {}", snapshot);

        // 解析JSON验证波特率已更新
        let json: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let baudrate = json["baudrate"].as_u64().unwrap();
        println!("Baudrate after write: {}", baudrate);
        assert_eq!(baudrate, 0x04, "波特率应该已更新为0x04");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_write_command_password_error() {
        use crate::protocol::{encode_frame, ErrorInfoWord};

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：写入波特率，但密码错误
        let di = [0x01, 0x01, 0x04, 0x04]; // 04-04-01-01 波特率
        let password = [0x00, 0x11, 0x22, 0x33]; // PA0=00(0级权限) + 错误密码=112233
        let operator_code = [0x01, 0x02, 0x03, 0x04];
        let write_data = [0x04];

        let mut data = Vec::new();
        data.extend_from_slice(&di);
        data.extend_from_slice(&password);
        data.extend_from_slice(&operator_code);
        data.extend_from_slice(&write_data);

        let frame = Frame {
            address,
            control: 0x14,
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送写命令（密码错误）: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应（密码错误）: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=D4H（写数据异常应答）
        assert_eq!(response_frame.control, 0xD4, "控制码应该是0xD4（异常应答）");

        // 验证错误信息字包含密码错误标志
        assert!(response_frame.data.len() > 0, "异常应答应该有错误信息");
        let error_word = response_frame.data[0];
        println!("Error word: 0x{:02X}", error_word);
        assert_ne!(
            error_word & ErrorInfoWord::PASSWORD_ERR,
            0,
            "应该包含密码错误标志"
        );

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_freeze_command() {
        use crate::protocol::{encode_frame, format::u64_to_bcd};

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：瞬时冻结命令（99 99 99 99）
        let mut data = Vec::new();
        data.extend_from_slice(&u64_to_bcd(99, 1)); // mm = 99
        data.extend_from_slice(&u64_to_bcd(99, 1)); // hh = 99
        data.extend_from_slice(&u64_to_bcd(99, 1)); // DD = 99
        data.extend_from_slice(&u64_to_bcd(99, 1)); // MM = 99

        let frame = Frame {
            address,
            control: 0x16, // 冻结命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送冻结命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=96H（冻结命令正常应答）
        assert_eq!(response_frame.control, 0x96, "控制码应该是0x96");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "冻结命令正常应答应该无数据");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_change_baudrate_command() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：更改波特率到2400bps（代码0x04）
        let data = vec![0x04];

        let frame = Frame {
            address,
            control: 0x17, // 更改通信速率命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送更改波特率命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=97H（更改通信速率正常应答）
        assert_eq!(response_frame.control, 0x97, "控制码应该是0x97");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "更改速率正常应答应该无数据");

        // 验证状态已更新（通过GetSnapshot）
        let result = handle.send_admin_command(AdminCommand::GetSnapshot).await;
        assert!(result.is_ok());
        let snapshot = result.unwrap();
        let json: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let baudrate = json["baudrate"].as_u64().unwrap();
        assert_eq!(baudrate, 0x04, "波特率应该已更新为0x04");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_change_password_command() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：修改0级密码
        // DI = 04-00-02-01（密码等级1，对应索引0）
        let di = [0x01, 0x02, 0x00, 0x04]; // DI0=01, DI1=02, DI2=00, DI3=04
        let old_password = [0x00, 0x00, 0x00, 0x00]; // PA0=00(0级权限) + 旧密码=000000（默认）
        let new_password = [0x00, 0x11, 0x22, 0x33]; // PA0=00(0级权限) + 新密码=112233

        let mut data = Vec::new();
        data.extend_from_slice(&di);
        data.extend_from_slice(&old_password);
        data.extend_from_slice(&new_password);

        let frame = Frame {
            address,
            control: 0x18, // 修改密码命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送修改密码命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=98H（修改密码正常应答）
        assert_eq!(response_frame.control, 0x98, "控制码应该是0x98");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "修改密码正常应答应该无数据");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_demand_clear_command() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：需量清零（需要04级权限）
        let di = [0x00, 0x00, 0x00, 0x00]; // DI（可以是任意值）
        let password = [0x00, 0x00, 0x00, 0x00]; // PA0=00(0级权限) + 密码=000000
        let operator_code = [0x01, 0x02, 0x03, 0x04];

        let mut data = Vec::new();
        data.extend_from_slice(&di);
        data.extend_from_slice(&password);
        data.extend_from_slice(&operator_code);

        let frame = Frame {
            address,
            control: 0x19, // 需量清零命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送需量清零命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=99H（需量清零正常应答）
        assert_eq!(response_frame.control, 0x99, "控制码应该是0x99");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "需量清零正常应答应该无数据");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_meter_clear_command() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：电表清零（需要02级权限）
        let password = [0x00, 0x00, 0x00, 0x00]; // PA0=00(0级权限) + 密码=000000
        let operator_code = [0x01, 0x02, 0x03, 0x04];

        let mut data = Vec::new();
        data.extend_from_slice(&password);
        data.extend_from_slice(&operator_code);

        let frame = Frame {
            address,
            control: 0x1A, // 电表清零命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送电表清零命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=9AH（电表清零正常应答）
        assert_eq!(response_frame.control, 0x9A, "控制码应该是0x9A");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "电表清零正常应答应该无数据");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn test_meter_actor_event_clear_command() {
        use crate::protocol::encode_frame;

        // 创建 tick 广播通道
        let (_tick_tx, tick_rx) = broadcast::channel(16);

        // 创建命令通道
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        // 创建 VirtualMeter
        let config = VirtualMeterConfig::default();
        let address = config.address;
        let meter = VirtualMeter::new(config.clone());

        // 创建 MeterActor
        let actor_config = MeterActorConfig {
            address,
            ..Default::default()
        };
        let actor = MeterActor::new(meter, tick_rx, cmd_rx, actor_config);

        // 启动 Actor
        let handle = MeterActorHandle::new(cmd_tx, address);
        tokio::spawn(async move {
            actor.run().await;
        });

        // 测试：事件清零（需要02级权限）
        let password = [0x00, 0x00, 0x00, 0x00]; // PA0=00(0级权限) + 密码=000000
        let operator_code = [0x01, 0x02, 0x03, 0x04];

        let mut data = Vec::new();
        data.extend_from_slice(&password);
        data.extend_from_slice(&operator_code);

        let frame = Frame {
            address,
            control: 0x1B, // 事件清零命令
            data,
        };

        let frame_bytes = encode_frame(&frame);
        println!("发送事件清零命令: {:02X?}", frame_bytes);

        // 发送命令
        let decoded = crate::protocol::decode_frame(&frame_bytes).unwrap();

        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let msg = EngineMsg::ProtocolCommand {
            conn_id: 0,
            frame: decoded,
            reply_tx,
        };

        handle.cmd_tx.send(msg).await.unwrap();

        // 等待响应
        let response = reply_rx.recv().await.unwrap();
        println!("收到响应: {:02X?}", response);

        // 验证响应帧
        let response_frame = crate::protocol::decode_frame(&response).unwrap();

        // 验证控制码=9BH（事件清零正常应答）
        assert_eq!(response_frame.control, 0x9B, "控制码应该是0x9B");

        // 验证数据长度为0
        assert_eq!(response_frame.data.len(), 0, "事件清零正常应答应该无数据");

        // 关闭
        let _ = handle.send_admin_command(AdminCommand::Shutdown).await;
    }
}