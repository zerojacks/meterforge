// Actor 消息定义

use crate::protocol::Frame;
use crate::simulation::SimulationConfig;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// 电表引擎消息（主要消息类型）
#[derive(Debug)]
pub enum EngineMsg {
    /// 协议命令（来自 Transport -> Router -> Actor）
    ProtocolCommand {
        /// 来源连接 ID（用于 12H 续传缓冲按连接隔离）
        conn_id: u64,
        frame: Frame,
        /// 回复通道（mpsc：支持通配查询多表应答与续传多片段）
        reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    },

    /// 管理命令（直接操作状态，绕过协议权限）
    AdminCommand {
        cmd: AdminCommand,
        reply_tx: oneshot::Sender<Result<String, String>>,
    },
}

/// 协议命令封装
#[derive(Debug, Clone)]
pub struct ProtocolCommand {
    pub frame: Vec<u8>,
}

/// 注册表联动消息（用于 15H 写通信地址后更新路由表）
#[derive(Debug, Clone)]
pub enum RegistryMsg {
    UpdateAddress { old: [u8; 6], new: [u8; 6] },
}

/// 管理命令（用于测试和调试）
#[derive(Debug, Clone)]
pub enum AdminCommand {
    /// 获取表状态快照（JSON）
    GetSnapshot,

    /// 设置虚拟时间
    SetVirtualTime {
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    },

    /// 设置电能值
    SetEnergy {
        energy_type: u8,  // 1=正向有功, 2=反向有功, 3=正向无功, 4=反向无功
        rate: Option<u8>, // None=总, Some(1-4)=费率1-4
        value: f64,       // kWh
    },

    /// 设置负荷模型参数
    SetLoadModel {
        voltage: f64, // V
        current: f64, // A
        power_factor: f64,
    },

    /// Atomically replace every simulation input for one meter.
    ApplySimulationConfig { config: SimulationConfig },

    /// 触发冻结（用于测试）
    TriggerFreeze {
        freeze_type: u8, // 0=定时, 1=瞬时
    },

    /// 修改密码（admin 通道绕过协议权限校验，直接设置）
    ChangePassword { level: u8, new_password: [u8; 4] },

    /// 设置通信速率（波特率编码，仿真模式仅更新状态）
    SetBaudrate { baudrate: u8 },

    /// 最大需量清零
    ClearMaxDemand,

    /// 电表清零（重置电能/需量，保留事件/冻结/参数）
    ClearMeter,

    /// 设置费率时段表（时段列表：起始时/起始分/费率号）
    SetTouConfig { time_slots: Vec<(u8, u8, u8)> },

    /// 设置冻结配置（对应 04-00-09-xx / 04-00-12-xx 参数）
    ApplyFreezeConfig {
        /// 04-00-09-02 定时冻结模式字（0=关 1=月 2=日 3=时）
        timed_mode: u8,
        /// 04-00-09-03 瞬时冻结模式字
        instant_mode: u8,
        /// 04-00-09-04 约定冻结模式字
        appointment_mode: u8,
        /// 04-00-09-05 整点冻结模式字
        hourly_mode: u8,
        /// 04-00-09-06 日冻结模式字
        daily_mode: u8,
        /// 04-00-12-03 日冻结时间 [hh, mm] (BCD)
        daily_time: [u8; 2],
        /// 04-00-12-01 整点冻结起始时间 YYMMDDhhmm (BCD, 5字节)
        hourly_start: [u8; 5],
        /// 04-00-12-02 整点冻结间隔（分钟）
        hourly_interval_min: u8,
        /// 约定冻结触发时间 YYMMDDhhmm (BCD, 5字节)
        appointment_time: [u8; 5],
    },

    /// 设置结算日（04-00-0B-01~03 的 DDhh，DD=0 表示不启用）
    ApplySettlementDays { days: [u8; 3], hours: [u8; 3] },

    /// 设置负荷记录配置
    /// （04-00-09-01 模式字 / 04-00-0A-01 起始时间 MMDDhhmm BCD / 04-00-0A-02~07 六类间隔）
    ApplyLoadRecordConfig {
        mode_word: u8,
        start_time: [u8; 4],
        intervals: [u16; 6],
    },

    /// 故障注入（事件生成测试）：强制产生/解除指定故障事件
    ///
    /// 相别故障 event_type: 01=失压 02=欠压 03=过压 04=断相 0B=失流 0C=过流，phase: 1=A 2=B 3=C；
    /// 系统级故障 event_type: 05=全失压 06=辅助电源失电 07=电压逆相序 08=电流逆相序
    ///                       09=电压不平衡 0A=电流不平衡 0F=掉电，phase 必须为 0；
    /// 记录类 event_type: 30=编程 31=校时 32=清零，phase 必须为 0。
    InjectFault {
        event_type: u8,
        phase: u8,
        active: bool,
    },

    /// 强制flush电能寄存器
    ForceFlushEnergy,

    /// 获取地址
    GetAddress,

    /// 优雅关闭
    Shutdown,

    /// 保存状态到数据库（触发最终flush）
    SaveState,

    /// 加载冻结历史（合并内存环形缓冲 + 数据库历史，去重后按时间倒序）
    ///
    /// 返回值为 JSON 字符串，反序列化为 `Vec<crate::snapshot::FreezeSnapshotSummary>`。
    /// 用于 UI 切换到"冻结数据"标签页时按需加载，避免启动时全量读库。
    LoadFreezeHistory,
}

/// Tick 消息（全局时钟广播）
#[derive(Debug, Clone, Copy)]
pub struct TickMsg {
    /// 经过的墙上时间
    pub wall_elapsed: Duration,

    /// 时间倍率（用于加速仿真）
    pub time_scale: f64,
}