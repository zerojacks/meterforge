// 持久化请求类型定义

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// 持久化请求枚举
#[derive(Debug, Clone)]
pub enum PersistRequest {
    /// 写入冻结快照
    WriteFreezeSnapshot(FreezeSnapshotRow),

    /// 写入事件记录
    WriteEventRecord(EventRecordRow),

    /// 写入负荷记录
    WriteLoadProfileRecord(LoadProfileRecordRow),

    /// 写入电能寄存器（单条记录）
    WriteEnergyRegister(EnergyRegisterRow),

    /// 更新最大需量
    UpdateMaxDemand(MaxDemandRow),

    /// 写入负荷记录采样（load_profile_samples 表，按时间序列追加）
    WriteLoadProfileSample {
        address: String,
        sample: crate::simulation::state::LoadProfileSample,
    },
}

/// 冻结快照数据行（对应 freeze_snapshots 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreezeSnapshotRow {
    pub meter_address: String,
    pub trigger_type: u8,   // DI2
    pub category: u8,       // DI1
    pub occurrence_idx: u8, // DI0
    pub snapshot_time: DateTime<Local>,
    pub payload: serde_json::Value, // 快照数据（JSON）
}

/// 事件记录数据行（对应 event_records 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecordRow {
    pub meter_address: String,
    pub event_kind: u8,     // DI2
    pub sub_kind: u8,       // DI1
    pub occurrence_idx: u8, // DI0
    pub start_time: DateTime<Local>,
    pub end_time: Option<DateTime<Local>>,
    pub payload: serde_json::Value,
}

/// 负荷记录数据行（对应 load_profile_records 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadProfileRecordRow {
    pub meter_address: String,
    pub channel: u8,
    pub data_type: u8,
    pub recorded_at: DateTime<Local>,
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
    pub timestamp: DateTime<Local>,

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
    pub occurred_at: DateTime<Local>,
}

/// 负荷记录采样数据行（查询结果）
///
/// 用于从数据库查询负荷记录采样数据
#[derive(Debug, Clone)]
pub struct LoadProfileSampleRow {
    pub meter_address: String,
    pub sample_time: DateTime<Local>,
    pub data_type: u8, // 数据类型（01=电压，02=电流等）
    pub channel: u8,   // 通道（00=总，01=A相，02=B相，03=C相）
    pub value: f64,    // 采样值
}
