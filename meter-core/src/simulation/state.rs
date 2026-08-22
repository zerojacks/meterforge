// 电表状态 - 包含所有需要读写的数据

use chrono::{DateTime, Utc, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::VecDeque;

/// 能量类型标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnergyType {
    ForwardActive = 1,     // 正向有功 (00-01-xx-00)
    ReverseActive = 2,     // 反向有功 (00-02-xx-00)
    ForwardReactive = 3,   // 正向无功，协议为组合无功1 (00-03-xx-00)
    ReverseReactive = 4,   // 反向无功，协议为组合无功2 (00-04-xx-00)
    CombinedActive = 5,    // 组合有功 (00-00-xx-00)，读出时由正/反向有功合成
    Quadrant1Reactive = 6, // 第一象限无功 (00-05-xx-00)
    Quadrant2Reactive = 7, // 第二象限无功 (00-06-xx-00)
    Quadrant3Reactive = 8, // 第三象限无功 (00-07-xx-00)
    Quadrant4Reactive = 9, // 第四象限无功 (00-08-xx-00)
    ForwardApparent = 10,  // 正向视在 (00-09-xx-00)
    ReverseApparent = 11,  // 反向视在 (00-0A-xx-00)
}

/// 单个需量寄存值（值 + 发生时间）
#[derive(Debug, Clone, Copy)]
pub struct DemandValue {
    pub value: f64,
    pub time: DateTime<Utc>,
}

/// 事件类型（对应DI2）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    VoltageFault = 0x01,           // 失压事件
    UnderVoltage = 0x02,           // 欠压事件
    OverVoltage = 0x03,            // 过压事件
    PhaseLoss = 0x04,              // 断相事件
    TotalVoltageLoss = 0x05,       // 全失压事件
    AuxiliaryPowerLoss = 0x06,     // 辅助电源失电事件
    VoltageReverseSequence = 0x07, // 电压逆相序事件
    CurrentReverseSequence = 0x08, // 电流逆相序事件
    VoltageImbalance = 0x09,       // 电压不平衡事件
    CurrentImbalance = 0x0A,       // 电流不平衡事件
    CurrentFault = 0x0B,           // 失流事件
    OverCurrent = 0x0E,            // 过流事件
    PowerDown = 0x0F,              // 掉电事件
    ProgrammingRecord = 0x30,      // 编程记录
    TimeSyncRecord = 0x31,         // 校时记录
    ClearRecord = 0x32,            // 清零记录（电表清零/需量清零按 DI1 区分）
}

/// 事件记录
#[derive(Debug, Clone)]
pub struct EventRecord {
    /// 事件类型（DI2）
    pub event_type: u8,

    /// 事件子类型（DI1）
    pub sub_type: u8,

    /// 事件开始时间
    pub start_time: DateTime<Utc>,

    /// 事件结束时间（None表示事件仍在进行中）
    pub end_time: Option<DateTime<Utc>>,

    /// 事件数据（根据事件类型不同，存储不同的数据）
    pub data: Vec<u8>,
}

impl EventRecord {
    /// 创建新的事件记录
    pub fn new(event_type: u8, sub_type: u8, start_time: DateTime<Utc>, data: Vec<u8>) -> Self {
        Self {
            event_type,
            sub_type,
            start_time,
            end_time: None,
            data,
        }
    }

    /// 结束事件
    pub fn end_event(&mut self, end_time: DateTime<Utc>) {
        self.end_time = Some(end_time);
    }

    /// 获取事件持续时间（分钟）
    pub fn duration_minutes(&self) -> u32 {
        match self.end_time {
            Some(end) => {
                let duration = end.signed_duration_since(self.start_time);
                (duration.num_minutes().max(0)) as u32
            }
            None => {
                // 事件仍在进行中，计算到当前时间
                let duration = Utc::now().signed_duration_since(self.start_time);
                (duration.num_minutes().max(0)) as u32
            }
        }
    }
}

/// 事件统计信息
#[derive(Debug, Clone)]
pub struct EventSummary {
    /// 总次数
    pub total_count: u32,

    /// 总时长（分钟）
    pub total_duration_minutes: u32,
}

impl EventSummary {
    pub fn new() -> Self {
        Self {
            total_count: 0,
            total_duration_minutes: 0,
        }
    }

    /// 添加事件记录到统计
    pub fn add_event(&mut self, duration_minutes: u32) {
        self.total_count += 1;
        self.total_duration_minutes += duration_minutes;
    }
}

/// 事件环形缓冲区
#[derive(Debug, Clone)]
pub struct EventRingBuffer {
    /// 最大容量
    capacity: usize,

    /// 事件记录（最新的在队尾）
    records: VecDeque<EventRecord>,
}

impl EventRingBuffer {
    /// 创建新的事件环形缓冲区
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            records: VecDeque::with_capacity(capacity),
        }
    }

    /// 添加事件记录
    pub fn push(&mut self, record: EventRecord) {
        // 如果已满，移除最旧的记录
        if self.records.len() >= self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    /// 获取指定索引的事件记录（1-based，1表示最新）
    pub fn get(&self, occurrence_index: u8) -> Option<&EventRecord> {
        if occurrence_index == 0 || occurrence_index as usize > self.records.len() {
            return None;
        }
        // occurrence_index=1 对应最新记录（队尾）
        let index = self.records.len() - occurrence_index as usize;
        self.records.get(index)
    }

    /// 获取所有事件记录
    pub fn get_all(&self) -> Vec<&EventRecord> {
        self.records.iter().collect()
    }

    /// 获取记录数量
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 清空所有记录
    pub fn clear(&mut self) {
        self.records.clear();
    }
}

/// 时段定义（时段表中的一个时段）
#[derive(Debug, Clone)]
pub struct TimeSlot {
    pub start_hour: u8,   // 起始小时 (0-23)
    pub start_minute: u8, // 起始分钟 (0-59)
    pub rate_number: u8,  // 费率号 (1-63)
}

/// 时段表配置（一套日时段表）
#[derive(Debug, Clone)]
pub struct TimeSlotTable {
    pub slots: Vec<TimeSlot>, // 时段列表，最多14个时段
}

impl TimeSlotTable {
    /// 根据当前时间查询所属费率
    pub fn get_rate_at_time(&self, time: &DateTime<Utc>) -> u8 {
        let hour = time.hour() as u8;
        let minute = time.minute() as u8;
        let current_minutes = hour as u16 * 60 + minute as u16;

        // 找到当前时间所在的时段
        let mut active_rate = 1; // 默认费率1
        for slot in &self.slots {
            let slot_minutes = slot.start_hour as u16 * 60 + slot.start_minute as u16;
            if current_minutes >= slot_minutes {
                active_rate = slot.rate_number;
            } else {
                break;
            }
        }

        active_rate
    }
}

impl Default for TimeSlotTable {
    fn default() -> Self {
        // 默认4费率时段表示例：
        // 00:00-08:00 费率4（谷）
        // 08:00-12:00 费率2（峰）
        // 12:00-18:00 费率3（平）
        // 18:00-22:00 费率1（尖）
        // 22:00-24:00 费率4（谷）
        Self {
            slots: vec![
                TimeSlot {
                    start_hour: 0,
                    start_minute: 0,
                    rate_number: 4,
                },
                TimeSlot {
                    start_hour: 8,
                    start_minute: 0,
                    rate_number: 2,
                },
                TimeSlot {
                    start_hour: 12,
                    start_minute: 0,
                    rate_number: 3,
                },
                TimeSlot {
                    start_hour: 18,
                    start_minute: 0,
                    rate_number: 1,
                },
                TimeSlot {
                    start_hour: 22,
                    start_minute: 0,
                    rate_number: 4,
                },
            ],
        }
    }
}

/// 时区表（一套）
#[derive(Debug, Clone)]
pub struct TimeZoneTable {
    pub zones: Vec<TimeZone>, // 时区列表，最多14个时区
}

/// 时区定义
#[derive(Debug, Clone)]
pub struct TimeZone {
    pub start_month: u8,  // 起始月份 (1-12)
    pub start_day: u8,    // 起始日期 (1-31)
    pub day_table_id: u8, // 使用第几套日时段表 (1-8)
}

impl Default for TimeZoneTable {
    fn default() -> Self {
        // 默认全年使用第一套日时段表
        Self {
            zones: vec![TimeZone {
                start_month: 1,
                start_day: 1,
                day_table_id: 1,
            }],
        }
    }
}

/// 时段表配置（包含两套，支持切换）
#[derive(Debug, Clone)]
pub struct TouConfig {
    pub time_zone_table_1: TimeZoneTable, // 第一套时区表（04-00-03-04）
    pub time_zone_table_2: TimeZoneTable, // 第二套时区表（04-00-03-05）
    pub day_table_1: TimeSlotTable,       // 第一套日时段表（04-00-02-01）
    pub day_table_2: TimeSlotTable,       // 第二套日时段表（04-00-02-02）
    pub time_zone_switch_datetime: Option<DateTime<Utc>>, // 时区表切换时间（04-00-01-06）
    pub day_table_switch_datetime: Option<DateTime<Utc>>, // 日时段表切换时间（04-00-01-07）
}

impl Default for TouConfig {
    fn default() -> Self {
        Self {
            time_zone_table_1: TimeZoneTable::default(),
            time_zone_table_2: TimeZoneTable::default(),
            day_table_1: TimeSlotTable::default(),
            day_table_2: TimeSlotTable::default(),
            time_zone_switch_datetime: None,
            day_table_switch_datetime: None,
        }
    }
}

/// 环形缓冲区（固定容量，循环覆盖）
///
/// 用于存储冻结快照，按照协议规范：
/// - 定时冻结：最近12次（DI0=01-0C）
/// - 瞬时冻结：最近3次（DI0=01-03）
/// - 时区表/日时段表切换：最近3次
///
/// 索引规则（DI0）：
/// - 00H = 当前值（不在环形缓冲中，单独存储）
/// - 01H = 最近一次（最新）
/// - 02H = 上1次
/// - 03H = 上2次
/// - ...
/// - 0CH = 上11次（定时冻结）
#[derive(Debug, Clone)]
pub struct FreezeRingBuffer {
    /// 最大容量
    capacity: usize,
    /// 存储的快照（按时间顺序，索引0=最新）
    snapshots: VecDeque<FreezeSnapshot>,
}

impl FreezeRingBuffer {
    /// 创建指定容量的环形缓冲区
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            snapshots: VecDeque::with_capacity(capacity),
        }
    }

    /// 添加新快照（自动循环覆盖）
    ///
    /// 新快照插入到队首并重排 occurrence_index（队首=1，DI0 语义），
    /// 超过容量的自动丢弃。返回最新快照的序号。
    pub fn push(&mut self, mut snapshot: FreezeSnapshot) -> u8 {
        // 插入到队首
        snapshot.occurrence_index = 1;
        self.snapshots.push_front(snapshot);

        // 如果超过容量，移除最旧的
        if self.snapshots.len() > self.capacity {
            self.snapshots.pop_back();
        }

        // 重排序号：队首 = 1（上1次），往后递增（上2次…）
        for (index, snapshot) in self.snapshots.iter_mut().enumerate() {
            snapshot.occurrence_index = (index + 1) as u8;
        }
        1
    }

    /// 获取指定索引的快照（DI0: 01-0C）
    ///
    /// - index=1: 最近一次（snapshots[0]）
    /// - index=2: 上1次（snapshots[1]）
    /// - index=N: 上N-1次（snapshots[N-1]）
    pub fn get(&self, index: u8) -> Option<&FreezeSnapshot> {
        if index == 0 || index > self.capacity as u8 {
            return None;
        }

        let array_index = (index - 1) as usize;
        self.snapshots.get(array_index)
    }

    /// 获取所有快照（按时间从新到旧）
    pub fn get_all(&self) -> Vec<&FreezeSnapshot> {
        self.snapshots.iter().collect()
    }

    /// 获取快照数量
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// 清空所有快照
    pub fn clear(&mut self) {
        self.snapshots.clear();
    }
}

impl Default for FreezeRingBuffer {
    fn default() -> Self {
        Self::new(12) // 默认定时冻结容量
    }
}

/// 冻结触发类型（对应 DI2，设计方案 4.6.3 节 + 协议 DI3=05）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreezeTrigger {
    Timed,          // DI2=00，定时冻结（月/日/时周期）
    Instant,        // DI2=01，瞬时冻结
    TimeZoneSwitch, // DI2=02，时区表切换冻结
    DayTableSwitch, // DI2=03，日时段表切换冻结
    Hourly,         // DI2=04，整点冻结
    Daily,          // DI2=06，日冻结
    LadderSwitch,   // DI2=07，阶梯切换冻结
}

impl FreezeTrigger {
    /// 从 DI2 字节转换
    pub fn from_di2(di2: u8) -> Option<Self> {
        match di2 {
            0x00 => Some(FreezeTrigger::Timed),
            0x01 => Some(FreezeTrigger::Instant),
            0x02 => Some(FreezeTrigger::TimeZoneSwitch),
            0x03 => Some(FreezeTrigger::DayTableSwitch),
            0x04 => Some(FreezeTrigger::Hourly),
            0x06 => Some(FreezeTrigger::Daily),
            0x07 => Some(FreezeTrigger::LadderSwitch),
            _ => None,
        }
    }

    /// 转换为 DI2 字节
    pub fn to_di2(&self) -> u8 {
        match self {
            FreezeTrigger::Timed => 0x00,
            FreezeTrigger::Instant => 0x01,
            FreezeTrigger::TimeZoneSwitch => 0x02,
            FreezeTrigger::DayTableSwitch => 0x03,
            FreezeTrigger::Hourly => 0x04,
            FreezeTrigger::Daily => 0x06,
            FreezeTrigger::LadderSwitch => 0x07,
        }
    }

    /// 获取此类型冻结的最大历史次数
    pub fn max_history_count(&self) -> u8 {
        match self {
            FreezeTrigger::Timed => 12,         // 0x0C，最近12次
            FreezeTrigger::Instant => 3,        // 0x03，最近3次
            FreezeTrigger::TimeZoneSwitch => 3, // 0x03，最近3次
            FreezeTrigger::DayTableSwitch => 3, // 0x03，最近3次
            FreezeTrigger::Hourly => 62,        // 0x3E，最近62次
            FreezeTrigger::Daily => 62,         // 0x3E，最近62次
            FreezeTrigger::LadderSwitch => 3,   // 0x03，最近3次
        }
    }
}

/// 冻结数据类别（对应 DI1）
///
/// 根据协议，冻结数据的 DI1 对应于电能量/最大需量/变量的数据项编码
/// 例如：
/// - DI1=00: 冻结时间（YYMMDDWW hh:mm:ss）
/// - DI1=01: 正向有功电能
/// - DI1=02: 反向有功电能
/// - DI1=03-0A: 组合无功1/2，第1-4象限无功
/// - DI1=15-1D: A/B/C相电压/电流/瞬时功率等
/// - DI1=FF: 数据块（包含所有相关项）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FreezeDataCategory(pub u8);

impl FreezeDataCategory {
    /// 冻结时间
    pub const TIME: Self = Self(0x00);
    /// 正向有功电能
    pub const FORWARD_ACTIVE: Self = Self(0x01);
    /// 反向有功电能
    pub const REVERSE_ACTIVE: Self = Self(0x02);
    /// 正向无功电能
    pub const FORWARD_REACTIVE: Self = Self(0x03);
    /// 反向无功电能
    pub const REVERSE_REACTIVE: Self = Self(0x04);
    /// 数据块（全部）
    pub const BLOCK_ALL: Self = Self(0xFF);
}

/// 冻结快照数据结构（设计方案 4.6 节）
///
/// 包含某次冻结触发时的完整电表状态快照
#[derive(Debug, Clone)]
pub struct FreezeSnapshot {
    /// 快照时间（虚拟时钟）
    pub snapshot_time: DateTime<Utc>,

    /// 触发类型（DI2）
    pub trigger_type: FreezeTrigger,

    /// 快照序号（DI0: 00=当前，01-0C=最近N次）
    pub occurrence_index: u8,

    /// 快照数据内容（按 DI1 分类存储）
    pub data: FreezeData,
}

/// 冻结快照数据内容
///
/// 根据协议 DI3=05，冻结数据包括：
/// - 电能量（正向有功/反向有功/正向无功/反向无功，总及各费率）
/// - 最大需量（有功/无功，总及各费率）
/// - 瞬时量（电压/电流/功率/功率因数）- 可选
///
/// serde default 兼容旧持久化行缺失的新增字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FreezeData {
    // ─────────────────────────────────────────────────────────────
    // 电能量（总+各费率）
    // ─────────────────────────────────────────────────────────────
    /// 正向有功总电能 (kWh)
    pub forward_active_total: f64,
    /// 正向有功分费率电能（费率号1-N）
    pub forward_active_rates: Vec<f64>,

    /// 反向有功总电能 (kWh)
    pub reverse_active_total: f64,
    /// 反向有功分费率电能
    pub reverse_active_rates: Vec<f64>,

    /// 正向无功总电能 (kvarh)
    pub forward_reactive_total: f64,
    /// 正向无功分费率电能
    pub forward_reactive_rates: Vec<f64>,

    /// 反向无功总电能 (kvarh)
    pub reverse_reactive_total: f64,
    /// 反向无功分费率电能
    pub reverse_reactive_rates: Vec<f64>,

    /// 第一~四象限无功总电能 (kvarh)
    pub quadrant1_reactive_total: f64,
    pub quadrant1_reactive_rates: Vec<f64>,
    pub quadrant2_reactive_total: f64,
    pub quadrant2_reactive_rates: Vec<f64>,
    pub quadrant3_reactive_total: f64,
    pub quadrant3_reactive_rates: Vec<f64>,
    pub quadrant4_reactive_total: f64,
    pub quadrant4_reactive_rates: Vec<f64>,

    // ─────────────────────────────────────────────────────────────
    // 最大需量
    // ─────────────────────────────────────────────────────────────
    /// 正向有功最大需量 (kW)
    pub max_demand_active: f64,
    /// 正向有功最大需量发生时间
    pub max_demand_active_time: DateTime<Utc>,

    /// 正向有功最大需量分费率（值 + 发生时间）
    pub max_demand_active_rates: Vec<(f64, DateTime<Utc>)>,

    /// 正向无功最大需量 (kvar)
    pub max_demand_reactive: f64,
    /// 正向无功最大需量发生时间
    pub max_demand_reactive_time: DateTime<Utc>,

    /// 正向无功最大需量分费率（值 + 发生时间）
    pub max_demand_reactive_rates: Vec<(f64, DateTime<Utc>)>,

    // ─────────────────────────────────────────────────────────────
    // 瞬时量（可选，部分冻结类型不包含）
    // ─────────────────────────────────────────────────────────────
    /// A/B/C 相电压 (V)
    pub voltages: Option<[f64; 3]>,
    /// A/B/C 相电流 (A)
    pub currents: Option<[f64; 3]>,
    /// 总有功功率 (kW)
    pub active_power: Option<f64>,
    /// 总无功功率 (kvar)
    pub reactive_power: Option<f64>,
    /// 功率因数
    pub power_factor: Option<f64>,
    /// 频率 (Hz)
    pub frequency: Option<f64>,
}

impl Default for FreezeData {
    fn default() -> Self {
        Self {
            forward_active_total: 0.0,
            forward_active_rates: Vec::new(),
            reverse_active_total: 0.0,
            reverse_active_rates: Vec::new(),
            forward_reactive_total: 0.0,
            forward_reactive_rates: Vec::new(),
            reverse_reactive_total: 0.0,
            reverse_reactive_rates: Vec::new(),
            quadrant1_reactive_total: 0.0,
            quadrant1_reactive_rates: Vec::new(),
            quadrant2_reactive_total: 0.0,
            quadrant2_reactive_rates: Vec::new(),
            quadrant3_reactive_total: 0.0,
            quadrant3_reactive_rates: Vec::new(),
            quadrant4_reactive_total: 0.0,
            quadrant4_reactive_rates: Vec::new(),
            max_demand_active: 0.0,
            max_demand_active_time: Utc::now(),
            max_demand_active_rates: Vec::new(),
            max_demand_reactive: 0.0,
            max_demand_reactive_time: Utc::now(),
            max_demand_reactive_rates: Vec::new(),
            voltages: None,
            currents: None,
            active_power: None,
            reactive_power: None,
            power_factor: None,
            frequency: None,
        }
    }
}

impl FreezeData {
    /// 从 MeterState 创建冻结快照数据
    pub fn from_meter_state(state: &MeterState) -> Self {
        // 收集各费率的电能
        let mut forward_active_rates = Vec::new();
        let mut reverse_active_rates = Vec::new();
        let mut forward_reactive_rates = Vec::new();
        let mut reverse_reactive_rates = Vec::new();
        let mut quadrant_rates: [Vec<f64>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let quadrant_types = [
            EnergyType::Quadrant1Reactive,
            EnergyType::Quadrant2Reactive,
            EnergyType::Quadrant3Reactive,
            EnergyType::Quadrant4Reactive,
        ];

        for rate in 1..=state.num_rates {
            forward_active_rates.push(state.get_energy(EnergyType::ForwardActive, Some(rate)));
            reverse_active_rates.push(state.get_energy(EnergyType::ReverseActive, Some(rate)));
            forward_reactive_rates.push(state.get_energy(EnergyType::ForwardReactive, Some(rate)));
            reverse_reactive_rates.push(state.get_energy(EnergyType::ReverseReactive, Some(rate)));
            for (idx, energy_type) in quadrant_types.iter().enumerate() {
                quadrant_rates[idx].push(state.get_energy(*energy_type, Some(rate)));
            }
        }

        // 分费率需量（值 + 发生时间）
        let demand_rates = |energy_type: EnergyType| -> Vec<(f64, DateTime<Utc>)> {
            (1..=state.num_rates)
                .map(|rate| {
                    let demand = state.get_demand(0, 0, energy_type, Some(rate));
                    (demand.value, demand.time)
                })
                .collect()
        };

        let reactive_total_demand = state.get_demand(0, 0, EnergyType::ForwardReactive, None);

        Self {
            // 电能量
            forward_active_total: state.get_energy(EnergyType::ForwardActive, None),
            forward_active_rates,
            reverse_active_total: state.get_energy(EnergyType::ReverseActive, None),
            reverse_active_rates,
            forward_reactive_total: state.get_energy(EnergyType::ForwardReactive, None),
            forward_reactive_rates,
            reverse_reactive_total: state.get_energy(EnergyType::ReverseReactive, None),
            reverse_reactive_rates,
            quadrant1_reactive_total: state.get_energy(EnergyType::Quadrant1Reactive, None),
            quadrant1_reactive_rates: quadrant_rates[0].clone(),
            quadrant2_reactive_total: state.get_energy(EnergyType::Quadrant2Reactive, None),
            quadrant2_reactive_rates: quadrant_rates[1].clone(),
            quadrant3_reactive_total: state.get_energy(EnergyType::Quadrant3Reactive, None),
            quadrant3_reactive_rates: quadrant_rates[2].clone(),
            quadrant4_reactive_total: state.get_energy(EnergyType::Quadrant4Reactive, None),
            quadrant4_reactive_rates: quadrant_rates[3].clone(),

            // 最大需量
            max_demand_active: state.max_demand,
            max_demand_active_time: state.max_demand_time,
            max_demand_active_rates: demand_rates(EnergyType::ForwardActive),
            max_demand_reactive: reactive_total_demand.value,
            max_demand_reactive_time: reactive_total_demand.time,
            max_demand_reactive_rates: demand_rates(EnergyType::ForwardReactive),

            // 瞬时量
            voltages: Some([state.voltage_a, state.voltage_b, state.voltage_c]),
            currents: Some([state.current_a, state.current_b, state.current_c]),
            active_power: Some(state.active_power_total),
            reactive_power: Some(state.reactive_power_total),
            power_factor: Some(state.power_factor),
            frequency: Some(state.frequency),
        }
    }
}

/// 冻结类型标识（设计方案 4.6.3 节，待处理的冻结触发）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeType {
    Timed,       // 定时冻结（整点/日/月周期）
    Instant(u8), // 瞬时冻结（编号1-63）
    Appointment, // 约定冻结
    Hourly,      // 整点冻结（04-00-09-05 / 04-00-12-01/02 参数驱动）
    Daily,       // 日冻结（04-00-09-06 / 04-00-12-03 参数驱动）
    Event(u8),   // 事件触发冻结（事件类型）
}

/// 冻结模式配置
/// 铭牌与厂家参数（04-00-04-xx，ASCII/BCD）
#[derive(Debug, Clone)]
pub struct NameplateConfig {
    pub meter_no: [u8; 6],                // 04-00-04-02 表号
    pub asset_code: [u8; 32],             // 04-00-04-03 资产管理编码
    pub rated_voltage_ascii: [u8; 6],     // 04-00-04-04 额定电压
    pub rated_current_ascii: [u8; 6],     // 04-00-04-05 额定(基本)电流
    pub max_current_ascii: [u8; 6],       // 04-00-04-06 最大电流
    pub active_accuracy: [u8; 4],         // 04-00-04-07 有功准确度等级
    pub reactive_accuracy: [u8; 4],       // 04-00-04-08 无功准确度等级
    pub reactive_meter_constant: u32,     // 04-00-04-0A 无功常数 imp/kvarh
    pub meter_model_ascii: [u8; 10],      // 04-00-04-0B 电表型号
    pub production_date_ascii: [u8; 10],  // 04-00-04-0C 生产日期
    pub protocol_version_ascii: [u8; 16], // 04-00-04-0D 协议版本号
    pub customer_no: [u8; 6],             // 04-00-04-0E 客户编号
}

impl Default for NameplateConfig {
    fn default() -> Self {
        Self {
            meter_no: *b"000000",
            asset_code: [0x31; 32],
            rated_voltage_ascii: *b"220V  ",
            rated_current_ascii: *b"10(60)",
            max_current_ascii: *b"60A   ",
            active_accuracy: *b"1.0 ",
            reactive_accuracy: *b"2.0 ",
            reactive_meter_constant: 1600,
            meter_model_ascii: *b"DTSD1234  ",
            production_date_ascii: *b"2025-01-01",
            protocol_version_ascii: *b"DL/T645-2007    ",
            customer_no: *b"000000",
        }
    }
}

/// 显示与互感器参数（04-00-03-xx）
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub cycle_screen_count: u8,    // 04-00-03-01 自动循环显示屏数
    pub screen_period_seconds: u8, // 04-00-03-02 每屏显示时间
    pub energy_decimals: u8,       // 04-00-03-03 电能小数位数
    pub demand_decimals: u8,       // 04-00-03-04 功率(需量)小数位数
    pub key_screen_count: u8,      // 04-00-03-05 按键循环显示屏数
    pub ct_ratio: u32,             // 04-00-03-06 电流互感器变比
    pub pt_ratio: u32,             // 04-00-03-07 电压互感器变比
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            cycle_screen_count: 5,
            screen_period_seconds: 5,
            energy_decimals: 2,
            demand_decimals: 4,
            key_screen_count: 3,
            ct_ratio: 1,
            pt_ratio: 1,
        }
    }
}

/// 限制值与报警限值（04-00-0E ~ 04-00-10）
#[derive(Debug, Clone)]
pub struct LimitConfig {
    // 04-00-0D-xx 相网络系数（电导/电纳/电阻/电抗，A/B/C 各4项）
    pub network_coefficients: [[f64; 4]; 3],
    // 04-00-0E-xx 功率与电压限值
    pub forward_active_power_limit: f64,
    pub reverse_active_power_limit: f64,
    pub voltage_upper: f64,
    pub voltage_lower: f64,
    // 04-00-0F-xx 电量限值
    pub alarm_energy_1: f64,
    pub alarm_energy_2: f64,
    pub hoard_energy: f64,
    pub overdraft_energy: f64,
    // 04-00-10-xx 金额限值
    pub alarm_amount_1: f64,
    pub alarm_amount_2: f64,
    pub overdraft_amount: f64,
    pub hoard_amount: f64,
    pub close_allow_amount: f64,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            network_coefficients: [[0.0; 4]; 3],
            forward_active_power_limit: 0.0,
            reverse_active_power_limit: 0.0,
            voltage_upper: 253.0,
            voltage_lower: 187.0,
            alarm_energy_1: 0.0,
            alarm_energy_2: 0.0,
            hoard_energy: 0.0,
            overdraft_energy: 0.0,
            alarm_amount_1: 0.0,
            alarm_amount_2: 0.0,
            overdraft_amount: 0.0,
            hoard_amount: 0.0,
            close_allow_amount: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FreezeConfig {
    pub timed_freeze_mode: u8,       // 04-00-09-02 定时冻结数据模式字
    pub instant_freeze_mode: u8,     // 04-00-09-03 瞬时冻结数据模式字
    pub appointment_freeze_mode: u8, // 04-00-09-04 约定冻结数据模式字
}

impl Default for FreezeConfig {
    fn default() -> Self {
        Self {
            timed_freeze_mode: 0,
            instant_freeze_mode: 0,
            appointment_freeze_mode: 0,
        }
    }
}

/// 负荷记录配置
#[derive(Debug, Clone)]
pub struct LoadRecordConfig {
    pub mode_word: u8,       // 04-00-09-01 负荷记录模式字（位图）
    pub intervals: [u16; 8], // 04-00-0A-01~06 第1-6类负荷记录间隔（分钟）
}

impl Default for LoadRecordConfig {
    fn default() -> Self {
        Self {
            mode_word: 0,
            intervals: [0; 8],
        }
    }
}

/// 负荷记录数据内容（附录B，模式字 bit0~bit5 与六个块一一对应）
///
/// 与 FreezeData 平铺字段不同，负荷记录按附录B的六个数据块组织，且使用
/// 块级 Option：模式字的选通单位是"块"（选了电压电流频率就是 17 字节整体，
/// 不存在只记电压）；同时要区分"没记录这个块"（None，序列化后 JSON 键
/// 直接缺省，历史行自描述）和"记录了但值为 0"——日后改模式字不影响旧行解码。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct LoadRecordData {
    pub vif: Option<VifBlock>, // bit0 电压、电流、频率（B.2.1，17字节）
    pub pq: Option<PqBlock>,   // bit1 有、无功功率（B.2.2，24字节）
    pub pf: Option<PfBlock>,   // bit2 功率因数（B.2.3，8字节）
    pub energy: Option<EnergyBlock>, // bit3 有、无功总电能（B.2.4，16字节）
    pub quadrant: Option<QuadrantBlock>, // bit4 四象限无功（B.2.5，16字节）
    pub demand: Option<DemandBlock>,    // bit5 当前需量（B.2.6，6字节）
}

/// B.2.1 电压、电流、频率
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct VifBlock {
    pub voltage_a: f64, // V
    pub voltage_b: f64,
    pub voltage_c: f64,
    pub current_a: f64, // A
    pub current_b: f64,
    pub current_c: f64,
    pub frequency: f64, // Hz
}

/// B.2.2 有、无功功率
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PqBlock {
    pub active_total: f64, // kW
    pub active_a: f64,
    pub active_b: f64,
    pub active_c: f64,
    pub reactive_total: f64, // kvar
    pub reactive_a: f64,
    pub reactive_b: f64,
    pub reactive_c: f64,
}

/// B.2.3 功率因数（分相简化使用总值，与瞬时变量读路径一致）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PfBlock {
    pub total: f64,
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

/// B.2.4 有、无功总电能
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct EnergyBlock {
    pub forward_active: f64,     // 正向有功总电能 (kWh)
    pub reverse_active: f64,     // 反向有功总电能 (kWh)
    pub combined_reactive1: f64, // 组合无功1总电能 (kvarh)
    pub combined_reactive2: f64, // 组合无功2总电能 (kvarh)
}

/// B.2.5 四象限无功总电能
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct QuadrantBlock {
    pub q1: f64, // kvarh
    pub q2: f64,
    pub q3: f64,
    pub q4: f64,
}

/// B.2.6 当前需量
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DemandBlock {
    pub active: f64,    // 当前有功需量 (kW)
    pub reactive: f64,  // 当前无功需量 (kvar)
}

impl LoadRecordData {
    /// 按负荷记录模式字（附录C：bit0~bit5 对应六个数据块）从当前状态采集
    ///
    /// 取值口径与 FreezeData::from_meter_state 一致：电能/需量读寄存器，
    /// 瞬时量读物理引擎最新输出；数值存 SI 浮点（kW、kvarh…），协议定点/
    /// BCD 编码由读取侧的 encode_* 完成。
    pub fn from_meter_state(state: &MeterState, mode_word: u8) -> Self {
        Self {
            vif: (mode_word & 0x01 != 0).then(|| VifBlock {
                voltage_a: state.voltage_a,
                voltage_b: state.voltage_b,
                voltage_c: state.voltage_c,
                current_a: state.current_a,
                current_b: state.current_b,
                current_c: state.current_c,
                frequency: state.frequency,
            }),
            pq: (mode_word & 0x02 != 0).then(|| PqBlock {
                active_total: state.active_power_total,
                active_a: state.active_power_a,
                active_b: state.active_power_b,
                active_c: state.active_power_c,
                reactive_total: state.reactive_power_total,
                reactive_a: state.reactive_power_a,
                reactive_b: state.reactive_power_b,
                reactive_c: state.reactive_power_c,
            }),
            pf: (mode_word & 0x04 != 0).then(|| PfBlock {
                total: state.power_factor,
                // 分相功率因数未单独建模，与瞬时变量读路径一致使用总值
                a: state.power_factor,
                b: state.power_factor,
                c: state.power_factor,
            }),
            energy: (mode_word & 0x08 != 0).then(|| {
                EnergyBlock {
                    forward_active: state.get_energy(EnergyType::ForwardActive, None),
                    reverse_active: state.get_energy(EnergyType::ReverseActive, None),
                    combined_reactive1: state.get_energy(EnergyType::ForwardReactive, None),
                    combined_reactive2: state.get_energy(EnergyType::ReverseReactive, None),
                }
            }),
            quadrant: (mode_word & 0x10 != 0).then(|| QuadrantBlock {
                q1: state.get_energy(EnergyType::Quadrant1Reactive, None),
                q2: state.get_energy(EnergyType::Quadrant2Reactive, None),
                q3: state.get_energy(EnergyType::Quadrant3Reactive, None),
                q4: state.get_energy(EnergyType::Quadrant4Reactive, None),
            }),
            demand: (mode_word & 0x20 != 0).then(|| DemandBlock {
                // 当前结算周期需量寄存值（正向有功回退到 max_demand）
                active: state.get_demand(0, 0, EnergyType::ForwardActive, None).value,
                reactive: state.get_demand(0, 0, EnergyType::ForwardReactive, None).value,
            }),
        }
    }
}

/// 负荷记录采样行（某类负荷某节拍的完整快照，用于数据库持久化）
#[derive(Debug, Clone)]
pub struct LoadRecordSample {
    /// 第几类负荷记录（1~6，对应 04-00-0A-01~06）
    pub class_id: u8,
    /// 采样时刻（虚拟时钟）
    pub sample_time: DateTime<Utc>,
    /// 该节拍全部选通块的快照
    pub data: LoadRecordData,
}

/// 负荷记录采样状态（跟踪各类上次采样时间）
///
/// 采样节拍按"类"驱动：第1~6类各自有独立间隔（04-00-0A-01~06），每个节拍
/// 一次性采集模式字选通的全部数据块、整行落库——时间对齐与块原子性由此保证。
#[derive(Debug, Clone, Default)]
pub struct LoadProfileSamplingState {
    pub last_sample_times: [Option<DateTime<Utc>>; 6], // 索引 = 类号-1
}

impl LoadProfileSamplingState {
    /// 从数据库恢复的最后采样时间初始化状态
    pub fn from_last_sample_times(times: [Option<DateTime<Utc>>; 6]) -> Self {
        Self {
            last_sample_times: times,
        }
    }

    /// 检查某类是否到达采样节拍
    ///
    /// 对齐点用"UTC 纪元整分钟数 / 周期 * 周期"计算（见
    /// [`Self::aligned_slot`]），而不是只取当前时间的 `hour*60+minute`：
    /// - 纪元 1970-01-01 00:00:00 本身就是整点/整天对齐的，所以按周期
    ///   取模后仍然精确落在标准分钟点上（周期15分钟 → :00/:15/:30/:45，
    ///   周期60分钟 → 整点），不会因为换用绝对时间而算错刻度。
    /// - 对齐点包含日期信息，天然连续跨天：不会在跨午夜时把"昨天
    ///   23:45"和"今天00:00"错误地判定成同一或反向的对齐点。
    /// - 校时把虚拟时钟往回拨之后，`aligned(now)` 会正确地小于等于
    ///   `aligned(last)`，从而正确地判定"还没到下一个采样点"，而不是
    ///   永久性地卡死（旧实现只比较一天以内的分钟数，回拨后可能再也追不上
    ///   之前记录的、数值更大的"上次对齐点"）。
    pub fn should_sample(
        &mut self,
        class_idx: usize,
        current_time: &DateTime<Utc>,
        interval_minutes: u16,
    ) -> bool {
        if interval_minutes == 0 {
            return false; // 间隔为0表示不采样
        }
        if class_idx >= 6 {
            return false;
        }

        match self.last_sample_times[class_idx] {
            // 从未采样过：以当前对齐点为虚拟的"上次采样时间"，
            // 只有恰好落在对齐点上才立即采样，其余时刻等到跨越下一个对齐点
            None => {
                let time_stmp = chrono::DateTime::timestamp(current_time);
                time_stmp % (interval_minutes as i64 * 60) == 0
            }
            Some(last_time) => {
                // 检测时间倒退：如果当前时间早于上次采样时间，说明发生了校时回拨
                if *current_time < last_time {
                    self.last_sample_times[class_idx] = None;

                    // 时间倒退后不立即采样，等待跨越到下一个对齐点
                    return false;
                }

                // 跨入新的对齐点即采样：节拍由对齐点（而非整点秒）判定，
                // tick 落在对齐点之后（如 12:15:03）同样代表跨过了 12:15
                // 节拍，不应错过；同一对齐点内（如 12:15:05）不重复采样
                Self::aligned_slot(current_time, interval_minutes)
                    > Self::aligned_slot(&last_time, interval_minutes)
            }
        }
    }

    /// 时间所属的采样对齐点（周期起点，epoch 秒向下取整对齐）
    fn aligned_slot(t: &DateTime<Utc>, interval_minutes: u16) -> i64 {
        let interval_secs = interval_minutes as i64 * 60;
        chrono::DateTime::timestamp(t).div_euclid(interval_secs) * interval_secs
    }

    /// 更新某类的采样时间
    pub fn update_sample_time(&mut self, class_idx: usize, sample_time: DateTime<Utc>) {
        if class_idx < 6 {
            self.last_sample_times[class_idx] = Some(sample_time);
        }
    }
}

/// 密码配置（10级密码）
#[derive(Debug, Clone)]
pub struct PasswordConfig {
    pub passwords: [[u8; 4]; 10], // 04-00-0C-01~0A 10级密码
}

impl Default for PasswordConfig {
    fn default() -> Self {
        // 默认密码全为0（无密码保护）
        Self {
            passwords: [[0x00; 4]; 10],
        }
    }
}

impl PasswordConfig {
    /// 验证密码
    ///
    /// 参数：
    /// - level: 权限等级（0-9，0最高权限）
    /// - password: 3字节密码值
    ///
    /// 返回：密码是否正确
    pub fn verify(&self, level: u8, password: &[u8; 3]) -> bool {
        if level > 9 {
            return false;
        }

        // 获取对应等级的密码
        let stored = &self.passwords[level as usize];

        // 比较密码（只比较低3字节）
        stored[1] == password[0] && stored[2] == password[1] && stored[3] == password[2]
    }

    /// 设置密码
    ///
    /// 参数：
    /// - level: 权限等级（0-9）
    /// - password: 3字节密码值
    pub fn set_password(&mut self, level: u8, password: &[u8; 3]) {
        if level > 9 {
            return;
        }

        // 设置密码（第一个字节为权限等级）
        self.passwords[level as usize] = [
            level,       // PA0高半字节=等级
            password[0], // P00
            password[1], // P10
            password[2], // P20
        ];
    }
}

/// 派生状态字（设计方案 4.6.1）
#[derive(Debug, Clone, Default)]
pub struct DerivedStatusWords {
    pub status_word_1: u16, // 04-00-05-01 运行状态字1
    pub status_word_2: u16, // 04-00-05-02 运行状态字2
    pub status_word_3: u16, // 04-00-05-03 运行状态字3
    pub status_word_4: u16, // 04-00-05-04 运行状态字4（A相）
    pub status_word_5: u16, // 04-00-05-05 运行状态字5（B相）
    pub status_word_6: u16, // 04-00-05-06 运行状态字6（C相）
    pub status_word_7: u16, // 04-00-05-07 运行状态字7（合相）
    pub status_key: u32,  // 密钥状态字 4字节
    pub meter_status: u16,  // 电表自检状态字 2字节
}

/// 电表完整状态（设计方案 4.6 节）
///
/// 这是电表的"数据库"，包含所有可被读取/写入的数据项。
/// PhysicsEngine 负责更新这些字段，DIHandler 负责读取并编码这些字段。
#[derive(Debug, Clone)]
pub struct MeterState {
    // ─────────────────────────────────────────────────────────────
    // 身份与参量（可被写命令修改）
    // ─────────────────────────────────────────────────────────────
    pub address: [u8; 6], // 电表地址

    // ─────────────────────────────────────────────────────────────
    // 04-00-04-xx 电表运行参数
    // ─────────────────────────────────────────────────────────────
    pub meter_constant: u32,             // 04-00-04-02 电表常数 (imp/kWh)
    pub baudrate: u8,                    // 04-00-04-01 通信波特率
    pub rated_voltage: u32,              // 04-00-04-03 额定电压（毫伏）
    pub rated_current: u32,              // 04-00-04-04 额定电流（毫安）
    pub rated_frequency: u8,             // 04-00-04-05 额定频率（50Hz/60Hz）
    pub demand_period_minutes: u16,      // 04-00-04-06 需量周期（分钟）
    pub sliding_window_minutes: u16,     // 04-00-04-07 滑差时间（分钟）
    pub calibration_pulse_constant: u32, // 04-00-04-08 校表脉冲常数
    pub comm_speed_feature: [u8; 5],     // 04-00-04-0A 通信速率特征字（5个通信口）

    // ─────────────────────────────────────────────────────────────
    // 04-00-02-xx & 04-00-03-xx 费率时段参数
    // ─────────────────────────────────────────────────────────────
    pub num_rates: u8,            // 04-00-02-04 费率数 k≤63
    pub num_time_slots: u8,       // 04-00-02-03 日时段数 m≤14
    pub settlement_days: [u8; 3], // 04-00-0B-01~03 结算日 DD（0=未设置，1~28）
    /// 结算日 hh（04-00-0B-01~03 的 DDhh 中小时部分，0~23）
    pub settlement_hours: [u8; 3],
    pub tou_config: TouConfig,    // 时区表+日时段表配置（两套）
    pub weekly_rest_day_word: u8, // 04-00-08-01 周休日特征字

    // ─────────────────────────────────────────────────────────────
    // 04-00-06-xx 能量组合方式
    // ─────────────────────────────────────────────────────────────
    pub active_combination_word: u8, // 04-00-06-01 有功组合方式特征字
    pub reactive_combination_1: u8,  // 04-00-06-02 无功组合方式1特征字
    pub reactive_combination_2: u8,  // 04-00-06-03 无功组合方式2特征字

    // ─────────────────────────────────────────────────────────────
    // 04-00-09-xx & 04-00-0A-xx 冻结与负荷记录
    // ─────────────────────────────────────────────────────────────
    pub freeze_config: FreezeConfig,          // 冻结模式配置
    pub load_record_config: LoadRecordConfig, // 负荷记录配置

    // ─────────────────────────────────────────────────────────────
    // 04-00-0C-xx 密码
    // ─────────────────────────────────────────────────────────────
    pub password_config: PasswordConfig, // 10级密码
    pub operator_code: [u8; 4],          // 操作者代码

    // ─────────────────────────────────────────────────────────────
    // 04-00-05-xx 派生状态字（运行时计算，不可直接写入）
    // ─────────────────────────────────────────────────────────────
    pub derived_status: DerivedStatusWords,

    // ─────────────────────────────────────────────────────────────
    // 虚拟时钟
    // ─────────────────────────────────────────────────────────────
    pub virtual_time: DateTime<Utc>,

    // ─────────────────────────────────────────────────────────────
    // 仿真量：电能寄存器
    // ─────────────────────────────────────────────────────────────
    // Key = (能量类型, 费率号), 费率号 0 = 总
    pub energy_registers: HashMap<(EnergyType, u8), f64>,

    // ─────────────────────────────────────────────────────────────
    // 结算日电能与分相电能
    // ─────────────────────────────────────────────────────────────
    /// 结算日电能，Key = (结算日序号 1~12, 能量类型, 费率号 0=总)
    pub settlement_energies: HashMap<(u8, EnergyType, u8), f64>,
    /// 分相正向有功电能 [A, B, C] (00-15/29/3D-00-00)
    pub phase_forward_active: [f64; 3],
    /// 结算日分相正向有功电能，Key = (结算日 1~12, 相 0=A 1=B 2=C)
    pub settlement_phase_energies: HashMap<(u8, u8), f64>,
    /// 需量寄存器，Key = (结算日 0=当前 1~12, 相 0=总 1~3=A/B/C, 能量类型, 费率 0=总)
    /// 查无值时回退到 max_demand/max_demand_time
    pub demand_registers: HashMap<(u8, u8, EnergyType, u8), DemandValue>,

    // ─────────────────────────────────────────────────────────────
    // 仿真量：瞬时变量
    // ─────────────────────────────────────────────────────────────
    pub voltage_a: f64,
    pub voltage_b: f64,
    pub voltage_c: f64,
    pub current_a: f64,
    pub current_b: f64,
    pub current_c: f64,
    pub power_factor: f64,
    pub frequency: f64, // 频率（Hz）

    // 派生量（从上面计算得出）
    pub active_power_total: f64,
    pub active_power_a: f64,
    pub active_power_b: f64,
    pub active_power_c: f64,
    pub reactive_power_total: f64,
    pub reactive_power_a: f64,
    pub reactive_power_b: f64,
    pub reactive_power_c: f64,
    pub apparent_power_total: f64,

    // ─────────────────────────────────────────────────────────────
    // 电能质量变量
    // ─────────────────────────────────────────────────────────────
    /// 电压波形失真度 (%) [A, B, C]
    pub voltage_thd: [f64; 3],
    /// 电流波形失真度 (%) [A, B, C]
    pub current_thd: [f64; 3],
    /// 电压谐波含量 (%) [相][1~21次]，下标 0 未用
    pub voltage_harmonics: [[f64; 22]; 3],
    /// 电流谐波含量 (%) [相][1~21次]，下标 0 未用
    pub current_harmonics: [[f64; 22]; 3],
    /// 零线电流 (A)
    pub neutral_current: f64,
    /// 表内温度 (℃)
    pub meter_temperature: f64,
    /// 时钟电池电压 (V)
    pub clock_battery_voltage: f64,
    /// 停电抄表电池电压 (V)
    pub outage_battery_voltage: f64,
    /// 内部电池工作时间 (分钟)
    pub battery_work_minutes: u32,
    /// 当前阶梯电价 (元/kWh)
    pub current_step_price: f64,
    /// 剩余电量 (kWh, 费控 00-90-01-00)
    pub remaining_energy: f64,
    /// 剩余金额 (元, 费控 00-90-02-00)
    pub remaining_amount: f64,
    /// 负荷记录起始时间 (MMDDhhmm BCD 字节, 04-00-0A-01)
    pub load_record_start_time: [u8; 4],

    // ─────────────────────────────────────────────────────────────
    // 最大需量
    // ─────────────────────────────────────────────────────────────
    pub max_demand: f64,
    pub max_demand_time: DateTime<Utc>,

    // ─────────────────────────────────────────────────────────────
    // A.5 参变量扩展
    // ─────────────────────────────────────────────────────────────
    // 04-00-01-xx 时段切换时间 (YYMMDDhhmm BCD)
    pub calibration_pulse_width_ms: u16, // 04-00-01-05 校表脉冲宽度
    pub timezone_switch_time: [u8; 5],   // 04-00-01-06 两套时区表切换时间
    pub daytable_switch_time: [u8; 5],   // 04-00-01-07 两套日时段表切换时间
    pub price_switch_time: [u8; 5],      // 04-00-01-08 两套费率电价切换时间
    pub ladder_switch_time: [u8; 5],     // 04-00-01-09 两套梯度切换时间
    // 04-00-02-xx 费率时段参数
    pub num_time_zones: u8,           // 04-00-02-01 年时区数 p≤14
    pub num_day_tables: u8,           // 04-00-02-02 日时段表数 q≤8
    pub num_public_holidays: u16,     // 04-00-02-05 公共假日数 n≤254
    pub harmonic_analysis_orders: u8, // 04-00-02-06 谐波分析次数
    pub num_ladders: u8,              // 04-00-02-07 梯度数
    pub display_config: DisplayConfig,
    pub nameplate: NameplateConfig,
    // 04-00-08-02 周休日采用的日时段表号
    pub rest_day_table_no: u8,
    // 04-00-09-05/06 整点/日冻结模式字
    pub hourly_freeze_mode: u8,
    pub daily_freeze_mode: u8,
    /// 约定冻结时间（BCD YYMMDDhhmm，04-00-09-04 约定冻结的触发时刻）
    pub appointment_freeze_time: [u8; 5],
    /// 约定冻结是否已触发（一次性）
    pub appointment_freeze_fired: bool,
    /// 上次结算日转存的虚拟时间
    pub last_settlement_rollover: Option<DateTime<Utc>>,
    // 04-00-11-01 电表运行特征字1
    pub operation_feature_word_1: u16,
    // 04-00-11-04 主动上报模式字
    pub active_report_mode: [u8; 8],

    // 04-00-12-xx 整点/日冻结时间
    pub hourly_freeze_start: [u8; 5], // YYMMDDhhmm
    pub hourly_freeze_interval_min: u8,
    pub daily_freeze_time: [u8; 2], // hhmm
    // 04-00-13-01 无线通信在线及信号强弱指示
    pub wireless_signal: u8,
    pub limits: LimitConfig,

    // ─────────────────────────────────────────────────────────────
    // 冻结数据存储（按触发类型分别维护环形缓冲）
    // ─────────────────────────────────────────────────────────────
    /// 冻结快照历史（按触发类型存储）
    ///
    /// Key = FreezeTrigger (定时/瞬时/时区切换/日时段切换)
    /// Value = FreezeRingBuffer (环形缓冲区，容量按触发类型决定)
    pub freeze_snapshots: HashMap<FreezeTrigger, FreezeRingBuffer>,

    // ─────────────────────────────────────────────────────────────
    // 事件记录（环形缓冲）
    // ─────────────────────────────────────────────────────────────
    /// 事件记录明细（按事件类型存储）
    ///
    /// Key = (event_type=DI2, sub_type=DI1)
    /// 例如：(0x01, 0x01) = A相失压事件
    ///      (0x30, 0x0F) = 费率参数表编程记录
    /// Value = EventRingBuffer（环形缓冲，最多10次）
    pub event_records: HashMap<(u8, u8), EventRingBuffer>,

    /// 事件统计信息（总次数+总时长）
    ///
    /// Key = (event_type=DI2, sub_type=DI1)
    /// Value = EventSummary（总次数、总时长）
    pub event_summary: HashMap<(u8, u8), EventSummary>,

    // ─────────────────────────────────────────────────────────────
    // 负荷记录采样状态
    // ─────────────────────────────────────────────────────────────
    /// 负荷记录采样状态（跟踪上次采样时间）
    pub load_profile_state: LoadProfileSamplingState,

    /// 最近的负荷记录快照（每个类别保存最近5次）
    /// Key = class_id (1-6)，Value = 环形缓冲区（最多5条，最新的在前）
    pub recent_load_records: HashMap<u8, VecDeque<LoadRecordSample>>,

    /// 当前冻结数据（DI0=00，不在环形缓冲中，实时查询）
    ///
    /// 注意：根据协议，DI0=00 表示"当前"数据，不是历史快照
    /// 读取 05-xx-yy-00 时，直接从 MeterState 当前状态生成，不从快照读取

    // ─────────────────────────────────────────────────────────────
    // 冻结触发标志（tick检测到冻结点时设置，由MeterActor处理）
    // ─────────────────────────────────────────────────────────────
    /// 待处理的冻结触发类型列表（空表示无待处理冻结）
    ///
    /// 用 Vec 而不是单个 Option，是因为同一个虚拟时刻可能同时命中多种
    /// 冻结条件（比如 Timed 配的是"按日周期"、日冻结时间恰好也是
    /// 00:00，两者会在同一秒都满足触发条件），这时必须把它们都记下来，
    /// 不能因为先命中了一个就把另一个漏掉。
    ///
    /// 工作流程：
    /// 1. PhysicsEngine::tick() 检测到冻结触发点时把命中的类型 push 进来
    /// 2. MeterActor 在 tick 后检查此字段
    /// 3. MeterActor 依次处理每一个，然后清空此字段
    ///
    /// 性能考虑：
    /// - tick() 中只做轻量级检测（<1μs）
    /// - 快照生成异步化，不阻塞实时性
    pub pending_freeze_triggers: Vec<FreezeType>,

    /// Per-meter multiplier applied to the global simulation clock.
    pub simulation_time_scale: f64,
}

/// 取"现在"作为虚拟时钟值：数字与本地墙钟一致，类型用 UTC 承载。
///
/// 虚拟时钟是电表的"表面时间"——DL/T645 读日期/时间（04-00-01-01/02）
/// 直接把这些数字编成 BCD，UI 也按原数字显示，全链路不做时区换算。
/// 因此把本地当前时间写入虚拟时钟时，必须保持数字不变、仅贴上 UTC
/// 标签（naive_local + and_utc），而不是做真正的时区换算——后者会把
/// 数字拨慢/拨快时区偏移（如 UTC+8 少 8 小时），导致表显与本地钟不一致。
pub fn local_now_as_utc() -> DateTime<Utc> {
    chrono::Local::now().naive_local().and_utc()
}

impl Default for MeterState {
    fn default() -> Self {
        let mut state = Self {
            address: [0x12, 0x34, 0x56, 0x78, 0x90, 0x12],

            // 04-00-04-xx 电表运行参数
            meter_constant: 1600,
            baudrate: 0x10,        // 9600 bps（01/02/04/08/10/20 = 600/1200/2400/4800/9600/19200，见 SetBaudrate 校验表）
            rated_voltage: 220000, // 220V (毫伏)
            rated_current: 60000,  // 60A (毫安)
            rated_frequency: 50,   // 50Hz
            demand_period_minutes: 15,
            sliding_window_minutes: 1,
            calibration_pulse_constant: 3200,
            comm_speed_feature: [0x10; 5], // 默认全部 9600bps

            // 费率时段参数
            num_rates: 4,
            num_time_slots: 5,          // 根据默认时段表的实际时段数
            settlement_days: [1, 0, 0], // 第1日为结算日，其余未设置
            settlement_hours: [0; 3],   // 结算日 0 点转存
            tou_config: TouConfig::default(),
            weekly_rest_day_word: 0x07, // 周日为休息日

            // 能量组合方式
            active_combination_word: 0x00,
            reactive_combination_1: 0x00,
            reactive_combination_2: 0x00,

            // 冻结与负荷记录
            freeze_config: FreezeConfig::default(),
            load_record_config: LoadRecordConfig::default(),

            // 密码
            password_config: PasswordConfig::default(),
            operator_code: [0x00, 0x00, 0x00, 0x00],

            // 派生状态字
            derived_status: DerivedStatusWords::default(),

            // 虚拟时钟（表面时间：数字与本地墙钟一致，类型用 UTC 承载）
            virtual_time: local_now_as_utc(),

            // 电能寄存器
            energy_registers: HashMap::new(),
            settlement_energies: HashMap::new(),
            phase_forward_active: [0.0; 3],
            settlement_phase_energies: HashMap::new(),
            demand_registers: HashMap::new(),

            // 电能质量变量
            voltage_thd: [0.0; 3],
            current_thd: [0.0; 3],
            voltage_harmonics: [[0.0; 22]; 3],
            current_harmonics: [[0.0; 22]; 3],
            neutral_current: 0.0,
            meter_temperature: 25.0,
            clock_battery_voltage: 3.6,
            outage_battery_voltage: 3.6,
            battery_work_minutes: 0,
            current_step_price: 0.0,
            remaining_energy: 0.0,
            remaining_amount: 0.0,
            load_record_start_time: [0; 4],

            // 瞬时变量
            voltage_a: 220.0,
            voltage_b: 220.0,
            voltage_c: 220.0,
            current_a: 10.0,
            current_b: 10.0,
            current_c: 10.0,
            power_factor: 0.95,
            frequency: 50.0,

            // 派生量
            active_power_total: 0.0,
            active_power_a: 0.0,
            active_power_b: 0.0,
            active_power_c: 0.0,
            reactive_power_total: 0.0,
            reactive_power_a: 0.0,
            reactive_power_b: 0.0,
            reactive_power_c: 0.0,
            apparent_power_total: 0.0,

            // 最大需量
            max_demand: 0.0,
            max_demand_time: Utc::now(),

            // A.5 参变量扩展
            calibration_pulse_width_ms: 80,
            timezone_switch_time: [0; 5],
            daytable_switch_time: [0; 5],
            price_switch_time: [0; 5],
            ladder_switch_time: [0; 5],
            num_time_zones: 1,
            num_day_tables: 1,
            num_public_holidays: 0,
            harmonic_analysis_orders: 21,
            num_ladders: 0,
            display_config: DisplayConfig::default(),
            nameplate: NameplateConfig::default(),
            rest_day_table_no: 1,
            hourly_freeze_mode: 0,
            daily_freeze_mode: 0,
            appointment_freeze_time: [0; 5],
            appointment_freeze_fired: false,
            last_settlement_rollover: None,
            operation_feature_word_1: 0,
            active_report_mode: [0; 8],
            hourly_freeze_start: [0; 5],
            hourly_freeze_interval_min: 60,
            daily_freeze_time: [0; 2],
            wireless_signal: 0,
            limits: LimitConfig::default(),

            // 冻结数据存储
            freeze_snapshots: {
                let mut map = HashMap::new();
                // 定时冻结：最多12次
                map.insert(FreezeTrigger::Timed, FreezeRingBuffer::new(12));
                // 瞬时冻结：最多3次
                map.insert(FreezeTrigger::Instant, FreezeRingBuffer::new(3));
                // 时区表切换冻结：最多3次
                map.insert(FreezeTrigger::TimeZoneSwitch, FreezeRingBuffer::new(3));
                // 日时段表切换冻结：最多3次
                map.insert(FreezeTrigger::DayTableSwitch, FreezeRingBuffer::new(3));
                // 整点冻结：最多62次
                map.insert(FreezeTrigger::Hourly, FreezeRingBuffer::new(62));
                // 日冻结：最多62次
                map.insert(FreezeTrigger::Daily, FreezeRingBuffer::new(62));
                // 阶梯切换冻结：最多3次
                map.insert(FreezeTrigger::LadderSwitch, FreezeRingBuffer::new(3));
                map
            },

            // 事件记录存储
            event_records: HashMap::new(),
            event_summary: HashMap::new(),

            // 负荷记录采样状态
            load_profile_state: LoadProfileSamplingState::default(),
            recent_load_records: HashMap::new(),

            // 冻结触发标志
            pending_freeze_triggers: Vec::new(),
            simulation_time_scale: 1.0,
        };

        // 初始化电能寄存器
        state
            .energy_registers
            .insert((EnergyType::ForwardActive, 0), 0.0); // 总
        for rate in 1..=state.num_rates {
            state
                .energy_registers
                .insert((EnergyType::ForwardActive, rate), 0.0);
        }

        state
    }
}

impl MeterState {
    /// 获取指定能量类型和费率的电能值
    pub fn get_energy(&self, energy_type: EnergyType, rate: Option<u8>) -> f64 {
        let rate_num = rate.unwrap_or(0);
        *self
            .energy_registers
            .get(&(energy_type, rate_num))
            .unwrap_or(&0.0)
    }

    /// 设置指定能量类型和费率的电能值
    pub fn set_energy(&mut self, energy_type: EnergyType, rate: Option<u8>, value: f64) {
        let rate_num = rate.unwrap_or(0);
        self.energy_registers.insert((energy_type, rate_num), value);
    }

    /// 增加电能（供 PhysicsEngine 调用）
    pub fn add_energy(&mut self, energy_type: EnergyType, rate: Option<u8>, delta: f64) {
        let rate_num = rate.unwrap_or(0);
        let current = self.get_energy(energy_type, Some(rate_num));
        self.set_energy(energy_type, Some(rate_num), current + delta);
    }

    /// 读取电能量（含结算日维度）
    ///
    /// - settlement=0 表示当前结算周期，直接读 energy_registers
    /// - settlement=1~12 读 settlement_energies
    pub fn get_settlement_energy(
        &self,
        settlement: u8,
        energy_type: EnergyType,
        rate: Option<u8>,
    ) -> f64 {
        if energy_type == EnergyType::CombinedActive {
            // 组合有功按"正向有功 - 反向有功"动态合成
            return self.get_settlement_energy(settlement, EnergyType::ForwardActive, rate)
                - self.get_settlement_energy(settlement, EnergyType::ReverseActive, rate);
        }
        let rate_num = rate.unwrap_or(0);
        if settlement == 0 {
            self.get_energy(energy_type, rate)
        } else {
            *self
                .settlement_energies
                .get(&(settlement, energy_type, rate_num))
                .unwrap_or(&0.0)
        }
    }

    /// 判断结算日电能是否已在内存中（供协议读取按需回退数据库判断使用）
    ///
    /// - settlement=0（当前结算周期）恒为 true：直接从 energy_registers 实时读取
    /// - settlement=1~12：只有在内存 settlement_energies 里有对应槽位时才为 true；
    ///   缺失时（如进程刚启动、数据库恢复尚未完成或从未恢复过）调用方应改为
    ///   查询数据库（见 di_handler/energy.rs 的 handle_energy_read_async）
    pub fn has_settlement_energy(
        &self,
        settlement: u8,
        energy_type: EnergyType,
        rate: Option<u8>,
    ) -> bool {
        if settlement == 0 {
            return true;
        }
        let rate_num = rate.unwrap_or(0);
        self.settlement_energies
            .contains_key(&(settlement, energy_type, rate_num))
    }

    /// 设置结算日电能（settlement=1~12）
    pub fn set_settlement_energy(
        &mut self,
        settlement: u8,
        energy_type: EnergyType,
        rate: Option<u8>,
        value: f64,
    ) {
        let rate_num = rate.unwrap_or(0);
        self.settlement_energies
            .insert((settlement, energy_type, rate_num), value);
    }

    /// 读取需量寄存值。
    ///
    /// 正向有功总需量回退到 max_demand（历史兼容）；其余类型/费率/分相
    /// 未寄存时返回 0（由物理引擎按需量周期真实寄存）。
    pub fn get_demand(
        &self,
        settlement: u8,
        phase: u8,
        energy_type: EnergyType,
        rate: Option<u8>,
    ) -> DemandValue {
        let rate_num = rate.unwrap_or(0);
        if let Some(value) = self
            .demand_registers
            .get(&(settlement, phase, energy_type, rate_num))
        {
            return *value;
        }
        if energy_type == EnergyType::ForwardActive {
            DemandValue {
                value: self.max_demand,
                time: self.max_demand_time,
            }
        } else {
            DemandValue {
                value: 0.0,
                time: self.virtual_time,
            }
        }
    }

    /// 写入需量寄存值
    pub fn set_demand(
        &mut self,
        settlement: u8,
        phase: u8,
        energy_type: EnergyType,
        rate: Option<u8>,
        value: DemandValue,
    ) {
        let rate_num = rate.unwrap_or(0);
        self.demand_registers
            .insert((settlement, phase, energy_type, rate_num), value);
    }

    /// 读取分相正向有功电能；结算日槽位由结算日转存填充，未转存时为 0
    pub fn get_phase_energy(&self, settlement: u8, phase: u8) -> f64 {
        if settlement == 0 {
            self.phase_forward_active[phase as usize]
        } else {
            *self
                .settlement_phase_energies
                .get(&(settlement, phase))
                .unwrap_or(&0.0)
        }
    }

    /// 结算日转存：当虚拟时钟越过任一配置的结算日（DD 日 hh 时）时执行。
    ///
    /// 转存内容（当前结算周期 → 上1结算日，原有槽位顺移 1→2 … 11→12，12 丢弃）：
    /// - 各能量类型的总/费率电能
    /// - 分相正向有功电能
    /// - 各类/费率/分相最大需量（正向有功总需量取 max_demand）
    ///
    /// 转存后需量按结算周期归零（max_demand 清零、当前需量寄存器清除），
    /// 电能寄存器按协议继续累计。
    pub fn settlement_rollover_if_due(&mut self) -> bool {
        use chrono::{Datelike, TimeZone};

        let now = self.virtual_time;
        let last = self.last_settlement_rollover.unwrap_or(now);

        // 是否在 (last, now] 内跨过了某个配置结算日的 DD 日 hh:00
        let crossed = |day: u32, hour: u32| -> bool {
            if day == 0 || day > 31 || hour > 23 {
                return false;
            }
            // 逐月枚举 last 与 now 之间（含 now 当月）的结算日边界
            let mut cursor = chrono::Utc
                .with_ymd_and_hms(last.year(), last.month(), 1, 0, 0, 0)
                .single()
                .unwrap_or(last);
            loop {
                if let Some(boundary) = chrono::Utc
                    .with_ymd_and_hms(cursor.year(), cursor.month(), day, hour, 0, 0)
                    .single()
                {
                    if boundary > last && boundary <= now {
                        return true;
                    }
                }
                // 下一个月
                let next = chrono::Utc
                    .with_ymd_and_hms(cursor.year(), cursor.month(), 28, 23, 59, 59)
                    .single()
                    .unwrap_or(cursor)
                    + chrono::Duration::days(4);
                cursor = chrono::Utc
                    .with_ymd_and_hms(next.year(), next.month(), 1, 0, 0, 0)
                    .single()
                    .unwrap_or(next);
                if cursor > now {
                    return false;
                }
            }
        };

        let due = self
            .settlement_days
            .iter()
            .zip(self.settlement_hours.iter())
            .any(|(&d, &h)| crossed(d as u32, h as u32));
        self.last_settlement_rollover = Some(now);
        if !due {
            return false; // 未到结算日，不执行转存
        }

        // 1. 结算日电能：槽位顺移 11→12 … 1→2
        let mut shifted = HashMap::new();
        for ((settlement, energy_type, rate), value) in self.settlement_energies.drain() {
            if settlement < 12 {
                shifted.insert((settlement + 1, energy_type, rate), value);
            }
        }
        // 当前电能 → 槽位1
        for ((energy_type, rate), value) in &self.energy_registers {
            shifted.insert((1u8, *energy_type, *rate), *value);
        }
        self.settlement_energies = shifted;

        // 2. 结算日分相电能
        let mut shifted_phase = HashMap::new();
        for ((settlement, phase), value) in self.settlement_phase_energies.drain() {
            if settlement < 12 {
                shifted_phase.insert((settlement + 1, phase), value);
            }
        }
        for phase in 0..3u8 {
            shifted_phase.insert((1u8, phase), self.phase_forward_active[phase as usize]);
        }
        self.settlement_phase_energies = shifted_phase;

        // 3. 结算日需量
        let mut shifted_demand = HashMap::new();
        for ((settlement, phase, energy_type, rate), value) in self.demand_registers.drain() {
            if settlement == 0 {
                // 当前需量 → 槽位1（正向有功总需量取 max_demand）
                shifted_demand.insert((1u8, phase, energy_type, rate), value);
            } else if settlement < 12 {
                shifted_demand.insert((settlement + 1, phase, energy_type, rate), value);
            }
        }
        shifted_demand.insert(
            (1u8, 0u8, EnergyType::ForwardActive, 0u8),
            DemandValue {
                value: self.max_demand,
                time: self.max_demand_time,
            },
        );
        self.demand_registers = shifted_demand;

        // 4. 需量按结算周期归零，电能继续累计
        self.max_demand = 0.0;
        self.max_demand_time = now;

        true // 成功执行了结算日转存
    }

    /// 相角（U/I 夹角，单位：度），由功率因数反推
    pub fn phase_angle_degrees(&self) -> f64 {
        self.power_factor.clamp(-1.0, 1.0).acos().to_degrees()
    }

    // ═══════════════════════════════════════════════════════════════
    // 冻结快照管理方法
    // ═══════════════════════════════════════════════════════════════

    /// 生成并存储冻结快照
    ///
    /// 参数：
    /// - trigger: 触发类型（定时/瞬时/切换）
    ///
    /// 返回：新快照的序号（DI0: 01-0C）
    pub fn create_freeze_snapshot(&mut self, trigger: FreezeTrigger) -> u8 {
        // 生成快照数据
        let snapshot_data = FreezeData::from_meter_state(self);

        // 确定序号（新快照总是 01，旧快照后移）
        let occurrence_index = 1u8;

        // 创建快照
        let snapshot = FreezeSnapshot {
            snapshot_time: self.virtual_time,
            trigger_type: trigger,
            occurrence_index,
            data: snapshot_data,
        };

        // 存储到对应的环形缓冲区
        if let Some(buffer) = self.freeze_snapshots.get_mut(&trigger) {
            buffer.push(snapshot);
        }

        occurrence_index
    }

    /// 生成冻结快照并返回数据库行（用于持久化）
    ///
    /// 此方法生成快照并立即返回用于数据库写入的行数据
    /// 调用方需要自行将其提交到 PersistenceWorker。
    ///
    /// occurrence_idx 使用环形缓冲分配的真实序号（队首=1），
    /// payload 为 FreezeData 的 serde 序列化（读取侧兼容旧平铺格式）。
    pub fn create_freeze_snapshot_with_persist(
        &mut self,
        trigger: FreezeTrigger,
    ) -> (u8, crate::persistence::FreezeSnapshotRow) {
        use crate::persistence::FreezeSnapshotRow;

        // 生成快照数据
        let snapshot_data = FreezeData::from_meter_state(self);

        // 创建快照
        let snapshot = FreezeSnapshot {
            snapshot_time: self.virtual_time,
            trigger_type: trigger,
            occurrence_index: 1,
            data: snapshot_data.clone(),
        };

        // 存储到环形缓冲区并取回真实序号
        let occurrence_index = self
            .freeze_snapshots
            .get_mut(&trigger)
            .map(|buffer| buffer.push(snapshot))
            .unwrap_or(1);

        // 生成数据库行：payload 为 FreezeData 全量 serde JSON（category=0xFF 完整快照）
        let payload = serde_json::to_value(&snapshot_data).unwrap_or_else(|_| {
            serde_json::json!({
                "forward_active_total": snapshot_data.forward_active_total,
                "reverse_active_total": snapshot_data.reverse_active_total,
            })
        });
        let row = FreezeSnapshotRow {
            meter_address: String::new(), // 由VirtualMeter填充
            trigger_type: trigger.to_di2(), // 使用协议值（DI2）
            category: 0xFF, // 0xFF表示完整快照（包含所有类别）
            occurrence_idx: occurrence_index,
            snapshot_time: self.virtual_time.with_timezone(&chrono::Utc),
            payload,
        };

        (occurrence_index, row)
    }

    /// 获取冻结快照
    ///
    /// 参数：
    /// - trigger: 触发类型（DI2）
    /// - occurrence_index: 快照序号（DI0: 00=当前，01-0C=历史）
    ///
    /// 返回：快照引用
    pub fn get_freeze_snapshot(
        &self,
        trigger: FreezeTrigger,
        occurrence_index: u8,
    ) -> Option<&FreezeSnapshot> {
        // DI0=00 表示当前值，不从快照读取
        if occurrence_index == 0 {
            return None;
        }

        // 从环形缓冲区读取
        self.freeze_snapshots
            .get(&trigger)
            .and_then(|buffer| buffer.get(occurrence_index))
    }

    /// 获取所有冻结快照（指定触发类型）
    pub fn get_all_freeze_snapshots(&self, trigger: FreezeTrigger) -> Vec<&FreezeSnapshot> {
        self.freeze_snapshots
            .get(&trigger)
            .map(|buffer| buffer.get_all())
            .unwrap_or_default()
    }

    /// 清空冻结快照（指定触发类型）
    pub fn clear_freeze_snapshots(&mut self, trigger: FreezeTrigger) {
        if let Some(buffer) = self.freeze_snapshots.get_mut(&trigger) {
            buffer.clear();
        }
    }

    /// 添加事件记录
    ///
    /// 参数：
    /// - event_type: 事件类型（DI2）
    /// - sub_type: 事件子类型（DI1）
    /// - start_time: 事件开始时间
    /// - data: 事件数据
    pub fn add_event_record(
        &mut self,
        event_type: u8,
        sub_type: u8,
        start_time: DateTime<Utc>,
        data: Vec<u8>,
    ) {
        let key = (event_type, sub_type);

        // 创建事件记录
        let record = EventRecord::new(event_type, sub_type, start_time, data);

        // 添加到环形缓冲区（如果不存在则创建）
        self.event_records
            .entry(key)
            .or_insert_with(|| EventRingBuffer::new(10))
            .push(record.clone());

        // 更新事件统计（如果是已结束的事件）
        if let Some(_) = record.end_time {
            self.event_summary
                .entry(key)
                .or_insert_with(|| EventSummary::new())
                .add_event(record.duration_minutes());
        }
    }

    /// 结束事件记录（用于故障类事件）
    ///
    /// 参数：
    /// - event_type: 事件类型（DI2）
    /// - sub_type: 事件子类型（DI1）
    /// - end_time: 事件结束时间
    /// 结束故障类事件并按附录A.4回填数据尾部：
    /// data[0..16] = 正向有功/反向有功/组合无功1/组合无功2 总电能增量（各4字节BCD）
    /// 发生/结束时刻由读取侧拼装，data 即协议尾段（119字节）。
    pub fn finalize_fault_event(
        &mut self,
        event_type: u8,
        sub_type: u8,
        end_time: DateTime<Utc>,
        energy_increments: [f64; 4],
    ) {
        let key = (event_type, sub_type);
        let encode = |v: f64| -> Vec<u8> {
            let scaled = (v * 100.0).round() as u64;
            let mut out = vec![0u8; 4];
            let mut t = scaled;
            for b in out.iter_mut() {
                *b = (((t % 10) << 4) | ((t / 10) % 10)) as u8;
                t /= 100;
            }
            out
        };
        if let Some(buffer) = self.event_records.get_mut(&key) {
            for record in buffer.records.iter_mut().rev() {
                if record.end_time.is_none() {
                    if record.data.len() < 16 {
                        record.data.resize(16, 0);
                    }
                    for (i, inc) in energy_increments.iter().enumerate() {
                        let bytes = encode(*inc);
                        record.data[i * 4..i * 4 + 4].copy_from_slice(&bytes);
                    }
                    break;
                }
            }
        }
        // 复用结束时间与汇总统计路径
        self.end_event_record(event_type, sub_type, end_time);
    }

    pub fn end_event_record(&mut self, event_type: u8, sub_type: u8, end_time: DateTime<Utc>) {
        let key = (event_type, sub_type);

        // 查找最新的未结束事件记录
        if let Some(buffer) = self.event_records.get_mut(&key) {
            // 从最新记录开始查找
            for i in (0..buffer.len()).rev() {
                if let Some(record) = buffer.records.get_mut(i) {
                    if record.end_time.is_none() {
                        // 找到未结束的事件，设置结束时间
                        record.end_event(end_time);

                        // 更新事件统计
                        self.event_summary
                            .entry(key)
                            .or_insert_with(|| EventSummary::new())
                            .add_event(record.duration_minutes());

                        break;
                    }
                }
            }
        }
    }

    /// 获取事件记录
    ///
    /// 参数：
    /// - event_type: 事件类型（DI2）
    /// - sub_type: 事件子类型（DI1）
    /// - occurrence_index: 发生次数索引（1表示最新）
    ///
    /// 返回：事件记录的引用
    pub fn get_event_record(
        &self,
        event_type: u8,
        sub_type: u8,
        occurrence_index: u8,
    ) -> Option<&EventRecord> {
        let key = (event_type, sub_type);
        self.event_records
            .get(&key)
            .and_then(|buffer| buffer.get(occurrence_index))
    }

    /// 获取所有事件记录
    pub fn get_all_event_records(&self, event_type: u8, sub_type: u8) -> Vec<&EventRecord> {
        let key = (event_type, sub_type);
        self.event_records
            .get(&key)
            .map(|buffer| buffer.get_all())
            .unwrap_or_default()
    }

    /// 获取事件统计信息
    pub fn get_event_summary(&self, event_type: u8, sub_type: u8) -> Option<&EventSummary> {
        let key = (event_type, sub_type);
        self.event_summary.get(&key)
    }

    /// 清空所有事件记录
    pub fn clear_all_events(&mut self) {
        self.event_records.clear();
        self.event_summary.clear();
    }

    // ═══════════════════════════════════════════════════════════════
    // 启动恢复方法（任务#4）
    // ═══════════════════════════════════════════════════════════════

    /// 从数据库恢复电能寄存器
    ///
    /// 参数：
    /// - registers: 从数据库加载的电能寄存器HashMap (energy_kind, rate_index) -> value
    ///
    /// 此方法将数据库中的电能寄存器值加载到MeterState中
    pub fn restore_energy_registers(
        &mut self,
        registers: std::collections::HashMap<(u8, u8), f64>,
    ) {
        // 清空现有寄存器
        self.energy_registers.clear();

        // 加载数据库值
        for ((energy_kind, rate_index), value) in registers {
            // 映射数据库编码到EnergyType
            // DI2编码：01=正向有功, 02=反向有功, 03=正向无功, 04=反向无功
            let energy_type = match energy_kind {
                0x01 => EnergyType::ForwardActive,
                0x02 => EnergyType::ReverseActive,
                0x03 => EnergyType::ForwardReactive,
                0x04 => EnergyType::ReverseReactive,
                _ => continue, // 跳过未知类型
            };

            self.energy_registers
                .insert((energy_type, rate_index), value);
        }
    }

    /// 恢复虚拟时钟
    ///
    /// 参数：
    /// - virtual_time: 从数据库加载的虚拟时钟
    pub fn restore_virtual_time(&mut self, virtual_time: DateTime<Utc>) {
        self.virtual_time = virtual_time;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_meter_state_creation() {
        let state = MeterState::default();

        // 验证基本字段
        assert_eq!(state.address, [0x12, 0x34, 0x56, 0x78, 0x90, 0x12]);
        assert_eq!(state.meter_constant, 1600);
        assert_eq!(state.baudrate, 0x10);

        // 验证事件记录初始化
        assert_eq!(state.event_records.len(), 0);
        assert_eq!(state.event_summary.len(), 0);
    }

    #[test]
    fn test_event_limit() {
        let mut state = MeterState::default();

        // 测试环形缓冲区限制（最多10条）
        for i in 1..=15 {
            state.add_event_record(0x01, 0x01, Utc::now(), vec![i as u8]);
        }

        // 应该只保留最新的10条
        let records = state.get_all_event_records(0x01, 0x01);
        assert_eq!(records.len(), 10);

        // 最新的记录应该是15
        let latest = state.get_event_record(0x01, 0x01, 1).unwrap();
        assert_eq!(latest.data[0], 15);

        // 第10条记录应该是6（15-9=6）
        let tenth = state.get_event_record(0x01, 0x01, 10).unwrap();
        assert_eq!(tenth.data[0], 6);
    }

    #[test]
    fn test_event_record_operations() {
        let mut state = MeterState::default();
        let now = Utc::now();

        // 测试1：添加编程记录事件
        state.add_event_record(
            0x30, // 编程记录
            0x0F, // 费率参数表编程
            now,
            vec![0x01, 0x02, 0x03, 0x04], // 操作者代码
        );

        // 验证事件记录
        let record = state.get_event_record(0x30, 0x0F, 1).unwrap();
        assert_eq!(record.event_type, 0x30);
        assert_eq!(record.sub_type, 0x0F);
        assert_eq!(record.data, vec![0x01, 0x02, 0x03, 0x04]);

        // 测试2：添加故障事件并结束
        let fault_start = now - Duration::minutes(30);
        state.add_event_record(
            0x01, // 失压事件
            0x01, // A相失压
            fault_start,
            vec![],
        );

        // 结束事件
        let fault_end = now;
        state.end_event_record(0x01, 0x01, fault_end);

        // 验证事件持续时间
        let fault_record = state.get_event_record(0x01, 0x01, 1).unwrap();
        assert!(fault_record.end_time.is_some());
        assert_eq!(fault_record.duration_minutes(), 30);

        // 测试3：获取事件统计
        let summary = state.get_event_summary(0x01, 0x01).unwrap();
        assert_eq!(summary.total_count, 1);
        assert_eq!(summary.total_duration_minutes, 30);

        // 测试4：清空所有事件
        state.clear_all_events();
        assert_eq!(state.get_all_event_records(0x30, 0x0F).len(), 0);
        assert_eq!(state.get_all_event_records(0x01, 0x01).len(), 0);
    }

    #[test]
    fn test_event_ring_buffer() {
        let mut buffer = EventRingBuffer::new(3);

        // 添加3条记录
        for i in 1..=3 {
            buffer.push(EventRecord::new(0x30, 0x0F, Utc::now(), vec![i]));
        }

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get(1).unwrap().data[0], 3); // 最新
        assert_eq!(buffer.get(3).unwrap().data[0], 1); // 最旧

        // 添加第4条，应该挤掉第1条
        buffer.push(EventRecord::new(0x30, 0x0F, Utc::now(), vec![4]));

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get(1).unwrap().data[0], 4); // 最新
        assert_eq!(buffer.get(3).unwrap().data[0], 2); // 最旧（1被挤掉）

        // 清空
        buffer.clear();
        assert_eq!(buffer.len(), 0);
    }
}

    #[test]
    fn test_load_profile_sampling_alignment() {
        use chrono::TimeZone;

        let mut sampling_state = LoadProfileSamplingState::default();
        
        // 测试15分钟间隔，应该在 0, 15, 30, 45 分采样
        let interval = 15;
        
        // 12:00:00 - 应该采样
        let time_12_00 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_00, interval));
        sampling_state.update_sample_time(0, time_12_00);
        
        // 12:07:00 - 不应该采样（还在同一对齐点 12:00-12:14）
        let time_12_07 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 7, 0).unwrap();
        assert!(!sampling_state.should_sample(0, &time_12_07, interval));
        
        // 12:15:00 - 应该采样（新的对齐点）
        let time_12_15 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 15, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_15, interval));
        sampling_state.update_sample_time(0, time_12_15);
        
        // 12:30:00 - 应该采样
        let time_12_30 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_30, interval));
        sampling_state.update_sample_time(0, time_12_30);
        
        // 12:45:00 - 应该采样
        let time_12_45 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 45, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_45, interval));
        sampling_state.update_sample_time(0, time_12_45);
        
        // 13:00:00 - 应该采样（下一个小时的整点）
        let time_13_00 = Utc.with_ymd_and_hms(2025, 1, 1, 13, 0, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_13_00, interval));
    }

    #[test]
    fn test_load_profile_sampling_skip_alignment_point() {
        use chrono::TimeZone;

        let mut sampling_state = LoadProfileSamplingState::default();
        let interval = 15;
        
        // 12:00:00 - 第一次采样
        let time_12_00 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_00, interval));
        sampling_state.update_sample_time(0, time_12_00);
        
        // 关键测试：从 12:00 跳到 12:15:03（秒数不为0，跨过了采样点）
        // 应该仍然能够采样，因为已经跨过了一个对齐点
        let time_12_15_03 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 15, 3).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_15_03, interval), 
                "应该能够在 12:15:03 采样，即使秒数不为0");
        sampling_state.update_sample_time(0, time_12_15_03);
        
        // 12:15:05 - 同一个对齐点内（12:15-12:29），不应该重复采样
        let time_12_15_05 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 15, 5).unwrap();
        assert!(!sampling_state.should_sample(0, &time_12_15_05, interval),
                "同一对齐点内不应该重复采样");
        
        // 从 12:15 直接跳到 12:47（跨过了 12:30 和 12:45 两个采样点）
        // 应该在到达 12:47 时立即采样（代表 12:45 的对齐点）
        let time_12_47 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 47, 0).unwrap();
        assert!(sampling_state.should_sample(0, &time_12_47, interval),
                "跨过采样点后应该立即采样");
        sampling_state.update_sample_time(0, time_12_47);
        
        // 12:50 - 同一对齐点（12:45），不应该再采样
        let time_12_50 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 50, 0).unwrap();
        assert!(!sampling_state.should_sample(0, &time_12_50, interval));
    }

    #[test]
    fn test_load_profile_sampling_various_intervals() {
        use chrono::TimeZone;

        let mut sampling_state = LoadProfileSamplingState::default();
        
        // 测试30分钟间隔，应该在 0, 30 分采样
        let interval_30 = 30;
        
        let time_12_00 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        assert!(sampling_state.should_sample(1, &time_12_00, interval_30));
        sampling_state.update_sample_time(1, time_12_00);
        
        let time_12_15 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 15, 0).unwrap();
        assert!(!sampling_state.should_sample(1, &time_12_15, interval_30));
        
        let time_12_30 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 30, 0).unwrap();
        assert!(sampling_state.should_sample(1, &time_12_30, interval_30));
        sampling_state.update_sample_time(1, time_12_30);
        
        let time_13_00 = Utc.with_ymd_and_hms(2025, 1, 1, 13, 0, 0).unwrap();
        assert!(sampling_state.should_sample(1, &time_13_00, interval_30));
        
        // 测试60分钟间隔，应该只在整点采样
        let interval_60 = 60;
        
        let time_14_00 = Utc.with_ymd_and_hms(2025, 1, 1, 14, 0, 0).unwrap();
        assert!(sampling_state.should_sample(2, &time_14_00, interval_60));
        sampling_state.update_sample_time(2, time_14_00);
        
        let time_14_30 = Utc.with_ymd_and_hms(2025, 1, 1, 14, 30, 0).unwrap();
        assert!(!sampling_state.should_sample(2, &time_14_30, interval_60));
        
        let time_15_00 = Utc.with_ymd_and_hms(2025, 1, 1, 15, 0, 0).unwrap();
        assert!(sampling_state.should_sample(2, &time_15_00, interval_60));
    }

    #[test]
    fn test_load_profile_no_duplicate_sampling() {
        use chrono::TimeZone;

        let mut sampling_state = LoadProfileSamplingState::default();
        
        let interval = 15;
        let time_12_00 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
        
        // 第一次采样
        assert!(sampling_state.should_sample(0, &time_12_00, interval));
        sampling_state.update_sample_time(0, time_12_00);
        
        // 同一对齐点内的任何时间都不应该重复采样
        let time_12_05 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 5, 0).unwrap();
        assert!(!sampling_state.should_sample(0, &time_12_05, interval));
        
        let time_12_14 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 14, 59).unwrap();
        assert!(!sampling_state.should_sample(0, &time_12_14, interval));
    }