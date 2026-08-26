// 持久化请求类型定义

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::simulation::SimulationConfig;

/// 从 `meters` 表恢复的一表完整配置（协议参变量 + 仿真自定义项）
#[derive(Debug, Clone)]
pub struct PersistedMeterSettings {
    pub simulation: SimulationConfig,
    pub timed_freeze_mode: u8,
    pub instant_freeze_mode: u8,
    pub appointment_freeze_mode: u8,
    pub hourly_freeze_mode: u8,
    pub daily_freeze_mode: u8,
    pub daily_freeze_time: [u8; 2],
    pub hourly_freeze_start: [u8; 5],
    pub hourly_freeze_interval_min: u8,
    pub appointment_freeze_time: [u8; 5],
    pub settlement_days: [u8; 3],
    pub settlement_hours: [u8; 3],
    pub load_record_mode_word: u8,
    pub load_record_start_time: [u8; 4],
    pub load_record_intervals: [u16; 8],
    /// 通信速率（`comm_baud_json`）。老库从未写过时为 `None`，保持默认。
    pub baudrate: Option<u8>,
    /// 10 级密码（`passwords_json`）。老库从未写过时为 `None`，保持默认。
    pub passwords: Option<[[u8; 4]; 10]>,
    /// 费率时段表（`tou_config_json["tou"]`）。老库从未写过时为 `None`，保持默认。
    pub tou_time_slots: Option<Vec<(u8, u8, u8)>>,
}

/// 从数据库恢复出的虚拟时钟状态：virtual_time 本身 + 落盘时对应的本地
/// （真实）时间锚点。
///
/// `synced_at_ms` 在老数据库升级上来、还没写过新列（`virtual_time_synced_at_ms`）
/// 的情况下会是 `None`——调用方此时不知道 virtual_time 是多久之前保存的，
/// 应该跳过补时、按老逻辑原样使用。
#[derive(Debug, Clone, Copy)]
pub struct RestoredVirtualTime {
    pub virtual_time: DateTime<Utc>,
    pub synced_at_ms: Option<i64>,
}

/// 持久化请求枚举（`Barrier` 携带一次性的 oneshot ack，不可克隆）
#[derive(Debug)]
pub enum PersistRequest {
    /// 写入冻结快照
    WriteFreezeSnapshot(FreezeSnapshotRow),

    /// 写入事件记录
    WriteEventRecord(EventRecordRow),

    /// 写入电能寄存器（单条记录）
    WriteEnergyRegister(EnergyRegisterRow),

    /// 更新最大需量
    UpdateMaxDemand(MaxDemandRow),

    /// 写入负荷记录（load_profile_records 表，JSON payload）
    WriteLoadRecord(LoadRecordRow),

    /// 批量写入结算日历史电能数据（settlement_day=1~24）
    WriteSettlementEnergies(SettlementEnergiesRow),

    /// 保存虚拟时间和配置（用于定期持久化，防止异常退出丢失）
    ///
    /// `synced_at_ms` 是 `virtual_time` 快照对应的本地真实时间（落盘锚点），
    /// 必须与读取 `virtual_time` 同一时刻采集，供下次启动时计算停机补时
    /// （见 `RestoredVirtualTime`）。
    SaveVirtualTime {
        address: String,
        virtual_time: DateTime<Utc>,
        synced_at_ms: i64,
        time_scale: f64,
        simulation_config: SimulationConfig,
    },

    /// 排空屏障：本身不写任何数据，只用于确认排在它之前的请求都已经落库
    /// （worker 在批量事务提交之后才 ack）。删除电表时先用它等掉 Shutdown
    /// 产生的最终 flush，再清理数据库，避免迟到的批量写入把刚删掉的行复活。
    Barrier {
        ack: tokio::sync::oneshot::Sender<()>,
    },
}

/// 冻结快照数据行（对应 freeze_snapshots 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreezeSnapshotRow {
    pub meter_address: String,
    pub trigger_type: u8,   // DI2
    pub category: u8,       // DI1
    pub occurrence_idx: u8, // DI0
    pub snapshot_time: DateTime<Utc>,
    pub payload: serde_json::Value, // 快照数据（JSON）
}

/// 事件记录数据行（对应 event_records 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecordRow {
    pub meter_address: String,
    pub event_kind: u8,     // DI2
    pub sub_kind: u8,       // DI1
    pub occurrence_idx: u8, // DI0
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub payload: serde_json::Value,
}

/// 电能寄存器数据行（对应 energy_registers 表）
///
/// 设计说明：
/// - 按设计方案 4.7 节，批量存储所有电能寄存器
/// - 为简化实现，每次flush写入一条汇总记录（energy_kind=01, rate_index=00, settlement_day=00）
/// - 或者拆分成多条记录分别写入（见数据库schema）
#[derive(Debug, Clone)]
pub struct EnergyRegisterRow {
    pub meter_address: String,
    pub timestamp: DateTime<Utc>,

    // 组合有功电能
    pub combined_active_positive: f64,
    pub combined_active_negative: f64,

    // 组合无功电能
    pub combined_reactive_positive: f64,
    pub combined_reactive_negative: f64,

    // 各费率有功电能（正向）
    pub rate1_active_positive: f64,
    pub rate2_active_positive: f64,
    pub rate3_active_positive: f64,
    pub rate4_active_positive: f64,
}

/// 最大需量数据行（对应 max_demand 表）
#[derive(Debug, Clone)]
pub struct MaxDemandRow {
    pub meter_address: String,
    pub demand_kind: u8,
    pub rate_index: u8,
    pub value_fp: i64,
    pub occurred_at: DateTime<Utc>,
}

/// 结算日历史电能数据（批量写入 energy_registers 表，settlement_day=1~24）
///
/// 设计说明：
/// - 结算日转存后，需要批量保存所有结算日槽位的历史电能数据
/// - Key = (settlement_day, energy_kind, rate_index)
/// - settlement_day: 1~24 表示上1~24个结算日，对应DI0=01~18H
/// - energy_kind: 对应DI2编码（01=正向有功, 02=反向有功, 03=组合无功1, 04=组合无功2等）
/// - rate_index: 对应DI1编码（00=总, 01~3F=费率1~63）
#[derive(Debug, Clone)]
pub struct SettlementEnergiesRow {
    pub meter_address: String,
    /// 结算日电能数据：Key = (settlement_day, energy_kind, rate_index), Value = 电能值(kWh/kvarh)
    pub energies: std::collections::HashMap<(u8, u8, u8), f64>,
}

/// 结算日历史电能查询结果（单条数据库记录，供 UI 历史加载使用）
///
/// 与 `SettlementEnergiesRow`（写入用，一次批量提交多条）不同，这是
/// `query_settlement_energy_history` 的读取结果，一行对应一条数据库记录。
#[derive(Debug, Clone)]
pub struct SettlementEnergyDbRow {
    /// 结算日序号（1~12，对应协议 DI0=01~0C）
    pub settlement_day: u8,
    /// 对应DI2编码（01=正向有功, 02=反向有功, 03=组合无功1, 04=组合无功2等）
    pub energy_kind: u8,
    /// 对应DI1编码（00=总, 01~3F=费率1~63）
    pub rate_index: u8,
    /// 电能值（kWh/kvarh）
    pub value: f64,
    /// 落库时间（毫秒时间戳）
    pub updated_at_ms: i64,
}

/// 负荷记录采样数据行（查询结果）
///
/// 用于从数据库查询负荷记录采样数据
#[derive(Debug, Clone)]
pub struct LoadProfileSampleRow {
    pub meter_address: String,
    pub sample_time: DateTime<Utc>,
    pub data_type: u8, // 数据类型（01=电压，02=电流等）
    pub channel: u8,   // 通道（00=总，01=A相，02=B相，03=C相）
    pub value: f64,    // 采样值
}

// ═════════════════════════════════════════════════════════════════════════════
// 负荷记录数据结构（附录B，JSON存储，对齐冻结数据模式）
// ═════════════════════════════════════════════════════════════════════════════

/// 负荷记录数据内容（附录B，模式字 bit0~bit5 一一对应）
/// 
/// 块级 Option 设计原因：
/// 1. 模式字的选通单位是"块"（选了电压电流频率就是17字节整体）
/// 2. 区分"没记录这个块"和"记录了但值为0"
/// 3. None 块序列化后 JSON 键直接缺省，日后改模式字不影响旧行解码
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LoadRecordData {
    /// bit0: 电压电流频率（B.2.1，17字节）
    pub vif: Option<VifBlock>,
    /// bit1: 有无功功率（B.2.2，24字节）
    pub pq: Option<PqBlock>,
    /// bit2: 功率因数（B.2.3，8字节）
    pub pf: Option<PfBlock>,
    /// bit3: 有无功总电能（B.2.4，16字节）
    pub energy: Option<EnergyBlock>,
    /// bit4: 四象限无功（B.2.5，16字节）
    pub quadrant: Option<QuadrantBlock>,
    /// bit5: 当前需量（B.2.6，6字节）
    pub demand: Option<DemandBlock>,
}

/// B.2.1 电压、电流、频率块（17字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VifBlock {
    /// A/B/C 相电压 (V)
    pub voltage_a: f64,
    pub voltage_b: f64,
    pub voltage_c: f64,
    /// A/B/C 相电流 (A)
    pub current_a: f64,
    pub current_b: f64,
    pub current_c: f64,
    /// 频率 (Hz)
    pub frequency: f64,
}

/// B.2.2 有、无功功率块（24字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PqBlock {
    /// 总及A/B/C相有功功率 (kW)
    pub active_total: f64,
    pub active_a: f64,
    pub active_b: f64,
    pub active_c: f64,
    /// 总及A/B/C相无功功率 (kvar)
    pub reactive_total: f64,
    pub reactive_a: f64,
    pub reactive_b: f64,
    pub reactive_c: f64,
}

/// B.2.3 功率因数块（8字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PfBlock {
    /// 总及A/B/C相功率因数
    pub total: f64,
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

/// B.2.4 有、无功总电能块（16字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyBlock {
    /// 正向有功总电能 (kWh)
    pub forward_active: f64,
    /// 反向有功总电能 (kWh)
    pub reverse_active: f64,
    /// 组合无功1总电能 (kvarh)
    pub combined_reactive1: f64,
    /// 组合无功2总电能 (kvarh)
    pub combined_reactive2: f64,
}

/// B.2.5 四象限无功总电能块（16字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuadrantBlock {
    /// 第一~四象限无功总电能 (kvarh)
    pub q1: f64,
    pub q2: f64,
    pub q3: f64,
    pub q4: f64,
}

/// B.2.6 当前需量块（6字节）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandBlock {
    /// 当前有功需量 (kW)
    pub active: f64,
    /// 当前无功需量 (kvar)
    pub reactive: f64,
}

/// 负荷记录数据行（load_profile_records 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadRecordRow {
    pub meter_address: String,
    pub class_id: u8,            // 第1~6类负荷记录（1-6）
    pub sample_time: DateTime<Utc>,
    pub payload: serde_json::Value, // LoadRecordData 序列化
}