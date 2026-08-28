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

    /// 清除冻结历史数据（内存环形缓冲 + 数据库 `freeze_snapshots` 表），
    /// 保留冻结相关配置（触发模式/结算日等），仅清空已产生的历史快照。
    ClearFreezeHistory,

    /// 清除负荷记录历史数据（内存缓冲 + 数据库 `load_profile_records` 表），
    /// 保留负荷记录配置（模式字/起始时间/各类间隔），仅清空已产生的历史采样。
    ClearLoadProfileHistory,

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
        intervals: [u16; 8],
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

    /// 修改表地址（管理通道，绕过协议密码校验；由 UI"修改地址"入口使用）
    ///
    /// 处理时同步更新仿真状态与 actor 配置里的地址，并立即推送一次快照。
    /// 调用方负责在收到回执后完成路由表/句柄表/数据库的 re-key。
    SetAddress { address: [u8; 6] },

    /// 优雅关闭
    Shutdown,

    /// 保存状态到数据库（触发最终flush）
    SaveState,

    /// 加载冻结历史（合并内存环形缓冲 + 数据库历史，去重后按时间倒序）
    ///
    /// 返回值为 JSON 字符串，反序列化为 `Vec<crate::snapshot::FreezeSnapshotSummary>`。
    /// 用于 UI 切换到"冻结数据"标签页时按需加载，避免启动时全量读库。
    LoadFreezeHistory,

    /// 加载最近的负荷记录（`load_profile_records` 表，跨全部类别，按时间倒序）
    ///
    /// 与冻结历史不同，负荷记录落库后不维护内存历史（每类各自独立采样间隔，
    /// 靠数据库查询而非环形缓冲），因此这里只查数据库，不需要合并去重。
    /// 返回值为 JSON 字符串，反序列化为 `Vec<crate::snapshot::LoadRecordSummary>`。
    /// 用于 UI 切换到"负荷记录"标签页时按需加载。
    LoadLoadProfileHistory { max_records: u32 },

    /// 读取协议参数（虚拟时间 / 10 级密码 / 通信速率 / 费率时段表）
    ///
    /// 返回值为 JSON 字符串，供 UI 在"一键同步参数到所有表"时读取源表
    /// 当前已生效的参数，再通过 `ApplyProtocolParameters` 下发给其他表。
    GetProtocolParameters,

    /// 原子应用一组协议参数（虚拟时间 / 密码 / 通信速率 / 费率时段表）
    ///
    /// 用于把源表的参数同步到目标表：直接写内存状态并落库
    /// （`comm_baud_json` / `passwords_json` / `tou_config_json["tou"]` /
    /// 虚拟时间），绕过协议层的逐项写入流程。为了让批量同步后的
    /// 虚拟时间与源表保持一致，发送端会携带下发时刻，接收端按
    /// `接收时刻 - 下发时刻` 补偿传输耗时。
    ApplyProtocolParameters {
        /// 虚拟时间（Unix 毫秒时间戳）
        virtual_time_ms: i64,
        /// 该命令下发给当前目标表时的本地真实时间（Unix 毫秒时间戳）
        sent_at_ms: i64,
        /// 通信速率编码（04-00-07-03）
        baudrate: u8,
        /// 10 级密码（04-00-0C-01~0A）
        passwords: [[u8; 4]; 10],
        /// 费率时段表（起始时/起始分/费率号，04-00-02-xx / 04-00-03-xx）
        time_slots: Vec<(u8, u8, u8)>,
    },

    // ========================================
    // 数据项自定义
    // ========================================
    /// 设置自定义数据项开关（0=优先使用自定义数据项 1=完全使用自定义数据项 2=使用模拟数据）
    SetCustomDataMode { mode: u8 },

    /// 新增/覆盖一条自定义数据项：DI 精确匹配，data 为按人类正常顺序配置的
    /// 应答内容（不做协议转换，命中时回复前会整体逆序，与645协议"低字节在前"
    /// 的传输约定一致，其余不做任何编解码处理）
    SetCustomDataItem { di: [u8; 4], data: Vec<u8> },

    /// 删除一条自定义数据项
    RemoveCustomDataItem { di: [u8; 4] },

    /// 清空该表所有自定义数据项（内存 + 数据库 `custom_data_items` 表）
    ClearCustomDataItems,

    /// 获取自定义数据项开关 + 全部自定义数据项列表（JSON字符串）
    ///
    /// 返回值反序列化为 `{ "mode": u8, "items": [{"di": "DDDDDDDD", "data": "HEXHEX"}, ...] }`
    GetCustomDataItems,
}

/// Tick 消息（全局时钟广播）
#[derive(Debug, Clone, Copy)]
pub struct TickMsg {
    /// 经过的墙上时间
    pub wall_elapsed: Duration,

    /// 时间倍率（用于加速仿真）
    pub time_scale: f64,
}
