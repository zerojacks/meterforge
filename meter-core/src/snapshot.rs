// 跨层快照类型
//
// MeterActor 在 tick 后与 admin 命令成功后推送快照，UI 侧 Entity 被动接收并渲染。
// 单位以 MeterState 内部一致为准：active_power_total / max_demand 已为 kW。

use serde::{Deserialize, Serialize};

use crate::protocol::format::format_address;
use crate::simulation::state::{MeterState, LoadRecordData};
use crate::simulation::{EnergyType, LoadModelConfig, LoadProfile};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulationSnapshot {
    pub load_profile: String,
    pub fixed_load_factor: Option<f64>,
    pub rated_voltage_v: f64,
    pub rated_current_a: f64,
    pub rated_frequency_hz: f64,
    pub power_factor: f64,
    pub meter_constant: u32,
    pub demand_period_minutes: u16,
    pub time_scale: f64,
    pub voltage_noise_v: f64,
    pub frequency_noise_hz: f64,
    pub power_factor_noise: f64,
    pub power_factor_min: f64,
    pub power_factor_max: f64,
    pub phase_current_factors: [f64; 3],
    /// 冻结配置（04-00-09-xx / 04-00-12-xx），时间为十进制分量
    pub freeze: FreezeConfigSnapshot,
    /// 结算日 DD 与 hh（04-00-0B-01~03，0=未设置）
    pub settlement_days: [u8; 3],
    pub settlement_hours: [u8; 3],
    /// 负荷记录配置（04-00-09-01 / 04-00-0A-xx）
    pub load_record_mode_word: u8,
    pub load_record_start_time: [u8; 4], // 十进制 [MM,DD,hh,mm]
    pub load_record_intervals: [u16; 8],
}

/// 冻结配置快照，时间字段为十进制分量（非 BCD）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreezeConfigSnapshot {
    /// 0=关 1=月 2=日 3=时
    pub timed_mode: u8,
    pub instant_mode: u8,
    pub appointment_mode: u8,
    pub hourly_mode: u8,
    pub daily_mode: u8,
    /// [hh, mm]
    pub daily_time: [u8; 2],
    /// [yy, mm, dd, hh, minute]
    pub hourly_start: [u8; 5],
    pub hourly_interval_min: u8,
    /// [yy, mm, dd, hh, minute]
    pub appointment_time: [u8; 5],
}

impl Default for FreezeConfigSnapshot {
    fn default() -> Self {
        Self {
            timed_mode: 0,
            instant_mode: 0,
            appointment_mode: 0,
            hourly_mode: 0,
            daily_mode: 0,
            daily_time: [0; 2],
            hourly_start: [0; 5],
            hourly_interval_min: 0,
            appointment_time: [0; 5],
        }
    }
}

/// BCD 字节转十进制
fn bcd_to_dec(bcd: u8) -> u8 {
    ((bcd >> 4) & 0x0F) * 10 + (bcd & 0x0F)
}

/// 为展示层准备的事件记录；避免 UI 直接依赖仿真状态的内部类型。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventSnapshot {
    pub event_type: u8,
    pub sub_type: u8,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub data_hex: String,
}

/// 负荷记录快照（统一的负荷记录类型）
///
/// 既用于实时快照推送（MeterSnapshot.load_records），也用于数据库历史查询。
/// 包含完整的原始数据字段，UI层按需格式化展示。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadRecordSnapshot {
    pub class_id: u8,
    pub sample_time_ms: i64,
    pub voltage_a: Option<f32>,
    pub voltage_b: Option<f32>,
    pub voltage_c: Option<f32>,
    pub current_a: Option<f32>,
    pub current_b: Option<f32>,
    pub current_c: Option<f32>,
    pub active_power_kw: Option<f32>,
    pub reactive_power_kvar: Option<f32>,
    pub power_factor: Option<f32>,
    pub energy_forward_active_kwh: Option<f64>,
    pub energy_reverse_active_kwh: Option<f64>,
}

impl LoadRecordSnapshot {
    /// 从 LoadRecordData 构建快照
    pub fn from_load_record_data(
        class_id: u8,
        sample_time_ms: i64,
        data: &LoadRecordData,
    ) -> Self {
        Self {
            class_id,
            sample_time_ms,
            voltage_a: data.vif.as_ref().map(|v| v.voltage_a as f32),
            voltage_b: data.vif.as_ref().map(|v| v.voltage_b as f32),
            voltage_c: data.vif.as_ref().map(|v| v.voltage_c as f32),
            current_a: data.vif.as_ref().map(|v| v.current_a as f32),
            current_b: data.vif.as_ref().map(|v| v.current_b as f32),
            current_c: data.vif.as_ref().map(|v| v.current_c as f32),
            active_power_kw: data.pq.as_ref().map(|v| v.active_total as f32),
            reactive_power_kvar: data.pq.as_ref().map(|v| v.reactive_total as f32),
            power_factor: data.pf.as_ref().map(|v| v.total as f32),
            energy_forward_active_kwh: data.energy.as_ref().map(|v| v.forward_active),
            energy_reverse_active_kwh: data.energy.as_ref().map(|v| v.reverse_active),
        }
    }
    
    /// 生成类别标签（UI辅助方法）
    pub fn class_label(&self) -> String {
        format!("第{}类负荷记录", self.class_id)
    }
    
    /// 生成数据块摘要（UI辅助方法）
    pub fn blocks_summary(&self) -> String {
        let mut blocks = Vec::new();
        if self.voltage_a.is_some() || self.voltage_b.is_some() || self.voltage_c.is_some() {
            blocks.push("电压");
        }
        if self.current_a.is_some() || self.current_b.is_some() || self.current_c.is_some() {
            blocks.push("电流");
        }
        if self.active_power_kw.is_some() || self.reactive_power_kvar.is_some() {
            blocks.push("功率");
        }
        if self.power_factor.is_some() {
            blocks.push("功率因数");
        }
        if self.energy_forward_active_kwh.is_some() || self.energy_reverse_active_kwh.is_some() {
            blocks.push("电能");
        }
        if blocks.is_empty() {
            "无数据".to_string()
        } else {
            blocks.join("·")
        }
    }
}

/// 为展示层准备的冻结快照摘要。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreezeSnapshotSummary {
    pub trigger: String,
    pub occurrence_index: u8,
    pub snapshot_time_ms: i64,
    pub forward_active_kwh: f64,
    pub max_demand_kw: f64,
    pub voltage_a: Option<f64>,
    pub voltage_b: Option<f64>,
    pub voltage_c: Option<f64>,
}

/// 单表实时快照（轻量级，适合频繁传输）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeterSnapshot {
    pub address: String,
    pub virtual_time_ms: i64,
    pub voltage_a: f32,
    pub voltage_b: f32,
    pub voltage_c: f32,
    pub current_a: f32,
    pub current_b: f32,
    pub current_c: f32,
    pub active_power_kw: f32,
    pub reactive_power_kvar: f32,
    pub power_factor: f32,
    pub energy_total_kwh: f64,
    pub max_demand_kw: f32,
    pub is_online: bool,
    pub recent_event_count: u16,
    pub events: Vec<EventSnapshot>,
    pub freezes: Vec<FreezeSnapshotSummary>,
    pub load_records: Vec<LoadRecordSnapshot>,
    pub simulation: SimulationSnapshot,
}

impl FreezeSnapshotSummary {
    /// 从冻结快照数据构建展示摘要。
    ///
    /// `occurrence_index` 语义遵循协议 A.6：01=最近一次，02=上一次……由调用方
    /// 按实际来源计算（内存环形缓冲用位置，数据库行用挪号后的 occurrence_idx）。
    pub fn from_freeze_data(
        trigger_label: String,
        occurrence_index: u8,
        snapshot_time_ms: i64,
        data: &crate::simulation::FreezeData,
    ) -> Self {
        Self {
            trigger: trigger_label,
            occurrence_index,
            snapshot_time_ms,
            forward_active_kwh: data.forward_active_total,
            max_demand_kw: data.max_demand_active,
            voltage_a: data.voltages.map(|values| values[0]),
            voltage_b: data.voltages.map(|values| values[1]),
            voltage_c: data.voltages.map(|values| values[2]),
        }
    }
}

impl MeterSnapshot {
    /// 构建默认快照（全零，仅带地址，用于 UI Entity 初始化）
    pub fn default_with_address(address: String) -> Self {
        Self {
            address,
            virtual_time_ms: 0,
            voltage_a: 0.0,
            voltage_b: 0.0,
            voltage_c: 0.0,
            current_a: 0.0,
            current_b: 0.0,
            current_c: 0.0,
            active_power_kw: 0.0,
            reactive_power_kvar: 0.0,
            power_factor: 0.0,
            energy_total_kwh: 0.0,
            max_demand_kw: 0.0,
            is_online: false,
            recent_event_count: 0,
            events: Vec::new(),
            freezes: Vec::new(),
            load_records: Vec::new(),
            simulation: SimulationSnapshot {
                load_profile: "Residential".into(),
                fixed_load_factor: None,
                rated_voltage_v: 220.0,
                rated_current_a: 60.0,
                rated_frequency_hz: 50.0,
                power_factor: 0.95,
                meter_constant: 1600,
                demand_period_minutes: 15,
                time_scale: 1.0,
                voltage_noise_v: 2.0,
                frequency_noise_hz: 0.05,
                power_factor_noise: 0.002,
                power_factor_min: 0.85,
                power_factor_max: 0.99,
                phase_current_factors: [1.0, 0.98, 1.02],
                freeze: FreezeConfigSnapshot::default(),
                settlement_days: [1, 0, 0],
                settlement_hours: [0; 3],
                load_record_mode_word: 0,
                load_record_start_time: [0; 4],
                load_record_intervals: [0; 8],
            },
        }
    }

    /// 从 MeterState 构建快照（address 为 human-readable 12 位字符串）
    pub fn from_state(state: &MeterState, load_model: &LoadModelConfig, is_online: bool) -> Self {
        let energy_total_kwh = state.get_energy(EnergyType::ForwardActive, None);
        let recent_event_count = state
            .event_records
            .values()
            .map(|buf| buf.len())
            .sum::<usize>()
            .min(u16::MAX as usize) as u16;
        let mut events: Vec<_> = state
            .event_records
            .values()
            .flat_map(|records| records.get_all())
            .map(|record| EventSnapshot {
                event_type: record.event_type,
                sub_type: record.sub_type,
                start_time_ms: record.start_time.timestamp_millis(),
                end_time_ms: record.end_time.map(|time| time.timestamp_millis()),
                data_hex: record
                    .data
                    .iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect();
        events.sort_by_key(|record| std::cmp::Reverse(record.start_time_ms));
        // 注意：FreezeSnapshot.occurrence_index 字段本身在创建时恒为 1（见
        // state.rs 的 create_freeze_snapshot 系列方法），真正的"第几次"由
        // 环形缓冲区内的位置决定（get_all() 按新到旧排列），这里按位置重新
        // 计算，使其与协议 DI0（01=最近一次）语义一致，也便于跟数据库历史合并。
        let mut freezes: Vec<_> = state
            .freeze_snapshots
            .iter()
            .flat_map(|(trigger, records)| {
                let trigger_label = format!("{:?}", trigger);
                records
                    .get_all()
                    .into_iter()
                    .enumerate()
                    .map(move |(position, freeze)| {
                        FreezeSnapshotSummary::from_freeze_data(
                            trigger_label.clone(),
                            (position + 1) as u8,
                            freeze.snapshot_time.timestamp_millis(),
                            &freeze.data,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        freezes.sort_by_key(|record| std::cmp::Reverse(record.snapshot_time_ms));

        // 收集最近的负荷记录（跨所有类别，最新的在前）
        let mut load_records: Vec<_> = state
            .recent_load_records
            .iter()
            .flat_map(|(class_id, records)| {
                records.iter().map(|sample| LoadRecordSnapshot::from_load_record_data(
                    *class_id,
                    sample.sample_time.timestamp_millis(),
                    &sample.data,
                ))
            })
            .collect();
        load_records.sort_by_key(|record| std::cmp::Reverse(record.sample_time_ms));

        Self {
            address: format_address(&state.address),
            virtual_time_ms: state.virtual_time.timestamp_millis(),
            voltage_a: state.voltage_a as f32,
            voltage_b: state.voltage_b as f32,
            voltage_c: state.voltage_c as f32,
            current_a: state.current_a as f32,
            current_b: state.current_b as f32,
            current_c: state.current_c as f32,
            // active_power_total / max_demand 单位已是 kW，不再二次换算
            active_power_kw: state.active_power_total as f32,
            reactive_power_kvar: state.reactive_power_total as f32,
            power_factor: state.power_factor as f32,
            energy_total_kwh,
            max_demand_kw: state.max_demand as f32,
            is_online,
            recent_event_count,
            events,
            freezes,
            load_records,
            simulation: SimulationSnapshot {
                load_profile: match load_model.profile {
                    LoadProfile::Residential => "Residential".into(),
                    LoadProfile::Industrial => "Industrial".into(),
                    LoadProfile::Commercial => "Commercial".into(),
                    LoadProfile::Fixed(_) => "Fixed".into(),
                },
                fixed_load_factor: match load_model.profile {
                    LoadProfile::Fixed(value) => Some(value),
                    _ => None,
                },
                rated_voltage_v: state.rated_voltage as f64 / 1000.0,
                rated_current_a: state.rated_current as f64 / 1000.0,
                rated_frequency_hz: state.rated_frequency as f64,
                power_factor: state.power_factor,
                meter_constant: state.meter_constant,
                demand_period_minutes: state.demand_period_minutes,
                time_scale: state.simulation_time_scale,
                voltage_noise_v: load_model.voltage_noise_v,
                frequency_noise_hz: load_model.frequency_noise_hz,
                power_factor_noise: load_model.power_factor_noise,
                power_factor_min: load_model.power_factor_min,
                power_factor_max: load_model.power_factor_max,
                phase_current_factors: load_model.phase_current_factors,
                freeze: FreezeConfigSnapshot {
                    timed_mode: state.freeze_config.timed_freeze_mode,
                    instant_mode: state.freeze_config.instant_freeze_mode,
                    appointment_mode: state.freeze_config.appointment_freeze_mode,
                    hourly_mode: state.hourly_freeze_mode,
                    daily_mode: state.daily_freeze_mode,
                    daily_time: [
                        bcd_to_dec(state.daily_freeze_time[0]),
                        bcd_to_dec(state.daily_freeze_time[1]),
                    ],
                    hourly_start: state.hourly_freeze_start.map(bcd_to_dec),
                    hourly_interval_min: state.hourly_freeze_interval_min,
                    appointment_time: state.appointment_freeze_time.map(bcd_to_dec),
                },
                settlement_days: state.settlement_days,
                settlement_hours: state.settlement_hours,
                load_record_mode_word: state.load_record_config.mode_word,
                load_record_start_time: state.load_record_start_time.map(bcd_to_dec),
                load_record_intervals: state.load_record_config.intervals,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::LoadProfile;

    #[test]
    fn snapshot_includes_complete_simulation_configuration() {
        let mut state = MeterState::default();
        state.rated_voltage = 230_000;
        state.rated_current = 40_000;
        state.simulation_time_scale = 12.0;
        let load_model = LoadModelConfig {
            profile: LoadProfile::Fixed(0.4),
            voltage_noise_v: 1.5,
            frequency_noise_hz: 0.02,
            power_factor_noise: 0.001,
            power_factor_min: 0.7,
            power_factor_max: 0.98,
            phase_current_factors: [1.0, 0.9, 1.1],
        };

        let snapshot = MeterSnapshot::from_state(&state, &load_model, true);

        assert_eq!(snapshot.simulation.load_profile, "Fixed");
        assert_eq!(snapshot.simulation.fixed_load_factor, Some(0.4));
        assert_eq!(snapshot.simulation.rated_voltage_v, 230.0);
        assert_eq!(snapshot.simulation.rated_current_a, 40.0);
        assert_eq!(snapshot.simulation.time_scale, 12.0);
        assert_eq!(snapshot.simulation.power_factor_min, 0.7);
        assert_eq!(snapshot.simulation.phase_current_factors, [1.0, 0.9, 1.1]);
    }
}