// 物理引擎 - 基于电气物理关系的虚拟电表核心
//
// 设计原则（按设计方案4.6节）：
// - PhysicsEngine **不持有状态**，只负责仿真逻辑
// - 所有电表数据存储在 MeterState 中
// - tick() 方法接收 &mut MeterState，读取参数并更新状态
// - **不创建 tick**：tick 由全局 broadcast channel 统一产出（设计方案 4.4 节）

use super::state::{DemandValue, EnergyType, FreezeType, MeterState};
use chrono::{DateTime, Utc, Timelike};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

/// 物理引擎 - 虚拟电表核心（无状态，纯计算）
///
/// 职责：
/// 1. 推进虚拟时钟
/// 2. 根据负荷模型更新瞬时量（电压/电流/功率）
/// 3. 脉冲累加计算电能（按费率分类）
/// 4. 更新最大需量（滑差窗口）
/// 5. 检测事件（失压/过流等）
/// 6. 触发冻结（定时/瞬时）
pub struct PhysicsEngine {
    /// 负荷模型配置
    load_model_config: LoadModelConfig,

    /// 脉冲累加器（中间态，未凑够一个完整脉冲的余量）
    pulse_accumulator: f64,

    /// 最大需量滑差窗口
    demand_window: VecDeque<PowerSample>,

    /// 上次需量计算时刻（滑差步进用）
    last_demand_calc: Option<DateTime<Utc>>,

    /// 当前活跃的故障集合，key = (事件类型 DI2, 相别 DI1)
    /// 用于跟踪故障的开始/结束跃迁，避免重复生成事件。
    active_faults: HashSet<(u8, u8)>,

    /// 故障开始时的四类总电能基线，用于结束时计算协议要求的电能增量
    /// [正向有功, 反向有功, 组合无功1, 组合无功2]
    fault_start_energies: HashMap<(u8, u8), [f64; 4]>,

    /// 手动注入的持续故障（事件生成测试），key = (事件类型 DI2, 相别 DI1)
    forced_faults: HashSet<(u8, u8)>,
}

struct PowerSample {
    timestamp: DateTime<Utc>,
    power: f64,
    /// 分相有功（A/B/C）与总无功，用于各类/分相需量寄存
    power_a: f64,
    power_b: f64,
    power_c: f64,
    reactive_total: f64,
}

impl PhysicsEngine {
    /// 创建新的物理引擎
    pub fn new(config: LoadModelConfig) -> Self {
        Self {
            load_model_config: config,
            pulse_accumulator: 0.0,
            demand_window: VecDeque::new(),
            last_demand_calc: None,
            fault_start_energies: HashMap::new(),
            forced_faults: HashSet::new(),
            active_faults: HashSet::new(),
        }
    }

    /// Replaces the active load curve for this meter. The actor owns this
    /// transition, so a simulation tick never observes a partial update.
    pub fn set_load_profile(&mut self, profile: LoadProfile) {
        self.load_model_config.profile = profile;
    }

    pub fn load_profile(&self) -> LoadProfile {
        self.load_model_config.profile
    }

    pub fn load_model_config(&self) -> &LoadModelConfig {
        &self.load_model_config
    }

    pub fn set_load_model_config(&mut self, config: LoadModelConfig) {
        self.load_model_config = config;
    }

    /// 推进仿真（供 MeterActor::on_tick 调用）
    ///
    /// 设计说明：
    /// - PhysicsEngine **不创建 tick**，只被动接收
    /// - tick 由全局 broadcast channel 统一产出（设计方案 4.4 节）
    /// - 2000 个 MeterActor 各自订阅同一个广播通道
    ///
    /// 参数：
    /// - state: 电表状态（读取参数、更新数据）
    /// - elapsed: 流逝时长（VirtualMeter 传入已含倍率的模拟时长并将
    ///   time_scale 置 1.0；独立驱动时传真实流逝时间配 time_scale）
    /// - time_scale: 时间加速倍率（默认 1.0）
    ///
    /// 返回值：
    /// - bool: 是否发生了结算日转存
    ///
    /// 执行步骤（设计方案 4.5.1 节）：
    /// 1. 推进虚拟时钟
    /// 2. 根据负荷模型更新瞬时量
    /// 3. 脉冲累加电能
    /// 4. 更新最大需量
    /// 5. 事件检测（TODO）
    /// 6. 结算日转存（TODO）
    /// 7. 冻结调度（TODO）
    pub fn tick(&mut self, state: &mut MeterState, elapsed: Duration, time_scale: f64) -> bool {
        // ─────────────────────────────────────────────────────────────
        // 步骤1: 推进虚拟时钟（应用时间倍率）
        // ─────────────────────────────────────────────────────────────
        let sim_seconds = elapsed.as_secs_f64() * time_scale;
        let sim_elapsed = Duration::from_secs_f64(sim_seconds);
        // 结算日转存锚点：首帧以推进前的时钟为基准，保证单帧大步进也能检测跨越
        if state.last_settlement_rollover.is_none() {
            state.last_settlement_rollover = Some(state.virtual_time);
        }
        state.virtual_time =
            state.virtual_time + chrono::Duration::milliseconds((sim_seconds * 1000.0) as i64);

        // ─────────────────────────────────────────────────────────────
        // 步骤2: 根据负荷模型更新瞬时量
        // ─────────────────────────────────────────────────────────────
        self.update_instantaneous_values(state);

        // ─────────────────────────────────────────────────────────────
        // 步骤3: 脉冲累加电能（使用模拟时长，与虚拟时钟一致）
        // ─────────────────────────────────────────────────────────────
        self.accumulate_energy(state, sim_elapsed);

        // ─────────────────────────────────────────────────────────────
        // 步骤4: 更新最大需量（滑差窗口）
        // ─────────────────────────────────────────────────────────────
        self.update_max_demand(state);

        // ─────────────────────────────────────────────────────────────
        // 步骤5: 事件检测
        // ─────────────────────────────────────────────────────────────
        self.detect_events(state);

        // ─────────────────────────────────────────────────────────────
        // 步骤6: 结算日转存（按结算日参数归档电能/需量）
        // ─────────────────────────────────────────────────────────────
        let settlement_rolled_over = state.settlement_rollover_if_due();

        // ─────────────────────────────────────────────────────────────
        // 步骤7: 冻结调度（轻量级检测，设置标志）
        // ─────────────────────────────────────────────────────────────
        self.check_freeze_schedule(state);

        // ─────────────────────────────────────────────────────────────
        // 步骤7: 负荷记录采样（返回需要持久化的采样记录）
        // ─────────────────────────────────────────────────────────────
        // 注意：采样记录由调用方（MeterActor）负责持久化到数据库
        // 这里只检测采样条件，不直接操作数据库

        // ─────────────────────────────────────────────────────────────
        // 步骤8: 派生状态字更新
        // ─────────────────────────────────────────────────────────────
        self.update_derived_status(state);

        settlement_rolled_over
    }

    /// 步骤2: 更新瞬时量（电压/电流/功率）
    fn update_instantaneous_values(&mut self, state: &mut MeterState) {
        // 根据负荷模型计算当前负荷系数
        let load_factor = self.load_model_config.get_load_factor(&state.virtual_time);

        // 更新三相电压（额定电压 ± 小波动）
        let voltage_base = state.rated_voltage as f64 / 1000.0; // 毫伏转伏特
        state.voltage_a =
            voltage_base + Self::small_noise() * self.load_model_config.voltage_noise_v;
        state.voltage_b =
            voltage_base + Self::small_noise() * self.load_model_config.voltage_noise_v;
        state.voltage_c =
            voltage_base + Self::small_noise() * self.load_model_config.voltage_noise_v;

        // 更新三相电流（基准电流 × 负荷系数）
        let current_base = (state.rated_current as f64 / 1000.0) * load_factor; // 毫安转安培
        state.current_a = current_base * self.load_model_config.phase_current_factors[0];
        state.current_b = current_base * self.load_model_config.phase_current_factors[1];
        state.current_c = current_base * self.load_model_config.phase_current_factors[2];

        // 更新功率因数（缓慢随机游走）
        state.power_factor = (state.power_factor
            + Self::small_noise() * self.load_model_config.power_factor_noise)
            .clamp(
                self.load_model_config.power_factor_min,
                self.load_model_config.power_factor_max,
            );

        // 更新频率
        state.frequency = state.rated_frequency as f64
            + Self::small_noise() * self.load_model_config.frequency_noise_hz;

        // 计算三相有功功率 P = U × I × cosφ
        let pf = state.power_factor;
        state.active_power_a = state.voltage_a * state.current_a * pf / 1000.0;
        state.active_power_b = state.voltage_b * state.current_b * pf / 1000.0;
        state.active_power_c = state.voltage_c * state.current_c * pf / 1000.0;
        state.active_power_total =
            state.active_power_a + state.active_power_b + state.active_power_c;

        // 计算三相无功功率 Q = U × I × sinφ
        let sin_phi = (1.0 - pf * pf).sqrt();
        state.reactive_power_a = state.voltage_a * state.current_a * sin_phi / 1000.0;
        state.reactive_power_b = state.voltage_b * state.current_b * sin_phi / 1000.0;
        state.reactive_power_c = state.voltage_c * state.current_c * sin_phi / 1000.0;
        state.reactive_power_total =
            state.reactive_power_a + state.reactive_power_b + state.reactive_power_c;

        // 计算视在功率 S = U × I
        state.apparent_power_total = state.voltage_a * state.current_a / 1000.0
            + state.voltage_b * state.current_b / 1000.0
            + state.voltage_c * state.current_c / 1000.0;
    }

    /// 步骤3: 脉冲累加电能（设计方案 simulation_algorithms.md 第1节）
    fn accumulate_energy(&mut self, state: &mut MeterState, elapsed: Duration) {
        // 时间增量（小时）
        let dt_hours = elapsed.as_secs_f64() / 3600.0;

        // 电能增量（kWh）: ΔE = P × Δt
        let delta_energy_kwh = state.active_power_total * dt_hours;

        // 转换为脉冲数: pulses = ΔE × 电表常数
        // 电表常数单位: imp/kWh
        let delta_pulses = delta_energy_kwh * state.meter_constant as f64;

        // 累加到脉冲计数器
        self.pulse_accumulator += delta_pulses;

        // 当累加的脉冲数 ≥ 1 时，才将完整脉冲计入电能寄存器
        while self.pulse_accumulator >= 1.0 {
            self.pulse_accumulator -= 1.0;

            // 一个脉冲对应的电能: 1 / 电表常数 (kWh)
            let pulse_energy_kwh = 1.0 / state.meter_constant as f64;

            // 查询当前虚拟时间所属的费率号
            let current_rate = state
                .tou_config
                .day_table_1
                .get_rate_at_time(&state.virtual_time);

            // 累加到总电能
            let total_key = (EnergyType::ForwardActive, 0);
            let current_total = state
                .energy_registers
                .get(&total_key)
                .copied()
                .unwrap_or(0.0);
            state
                .energy_registers
                .insert(total_key, current_total + pulse_energy_kwh);

            // 累加到对应费率的电能
            let rate_key = (EnergyType::ForwardActive, current_rate);
            let current_rate_energy = state
                .energy_registers
                .get(&rate_key)
                .copied()
                .unwrap_or(0.0);
            state
                .energy_registers
                .insert(rate_key, current_rate_energy + pulse_energy_kwh);
        }
    }

    /// 步骤4: 更新最大需量（滑差窗口法，设计方案 simulation_algorithms.md 第2节）
    ///
    /// 需量按协议维度真实寄存（当前结算周期）：
    /// - 正向有功：总 + 当前费率 + A/B/C 分相
    /// - 正向无功：总 + 当前费率
    ///
    /// 滑差窗口法：采样保留 需量周期（04-00-01-03）长度的窗口，
    /// 每 滑差时间（04-00-01-04）步进一次计算窗口平均并更新最大需量；
    /// 滑差时间为 0 时退化为逐帧计算。
    fn update_max_demand(&mut self, state: &mut MeterState) {
        let now = state.virtual_time;
        let window_duration = chrono::Duration::minutes(state.demand_period_minutes as i64);
        let step_duration = chrono::Duration::minutes(state.sliding_window_minutes.max(1) as i64);

        // 添加当前功率采样
        self.demand_window.push_back(PowerSample {
            timestamp: now,
            power: state.active_power_total,
            power_a: state.active_power_a,
            power_b: state.active_power_b,
            power_c: state.active_power_c,
            reactive_total: state.reactive_power_total,
        });

        // 移除窗口外的旧样本
        while let Some(sample) = self.demand_window.front() {
            if now.signed_duration_since(sample.timestamp) > window_duration {
                self.demand_window.pop_front();
            } else {
                break;
            }
        }

        // 滑差步进：未到步进点则只采样不计算
        if state.sliding_window_minutes > 0 {
            let due = match self.last_demand_calc {
                Some(last) => now.signed_duration_since(last) >= step_duration,
                None => true,
            };
            if !due {
                return;
            }
        }
        self.last_demand_calc = Some(now);

        if self.demand_window.is_empty() {
            return;
        }
        let count = self.demand_window.len() as f64;
        let avg = |f: fn(&PowerSample) -> f64| -> f64 {
            self.demand_window.iter().map(f).sum::<f64>() / count
        };
        let avg_demand = avg(|s| s.power);

        // 更新最大需量（正向有功总）
        if avg_demand > state.max_demand {
            state.max_demand = avg_demand;
            state.max_demand_time = now;
        }

        // 当前虚拟时间所属费率号
        let current_rate = state
            .tou_config
            .day_table_1
            .get_rate_at_time(&state.virtual_time);

        let mut update = |key: (u8, u8, EnergyType, u8), value: f64, time: DateTime<Utc>| {
            if let Some(existing) = state.demand_registers.get(&key) {
                if value > existing.value {
                    state
                        .demand_registers
                        .insert(key, DemandValue { value, time });
                }
            } else {
                state
                    .demand_registers
                    .insert(key, DemandValue { value, time });
            }
        };

        // 正向有功：总 + 费率 + 分相
        update((0, 0, EnergyType::ForwardActive, 0), avg_demand, now);
        update(
            (0, 0, EnergyType::ForwardActive, current_rate),
            avg_demand,
            now,
        );
        update(
            (0, 1, EnergyType::ForwardActive, 0),
            avg(|s| s.power_a),
            now,
        );
        update(
            (0, 2, EnergyType::ForwardActive, 0),
            avg(|s| s.power_b),
            now,
        );
        update(
            (0, 3, EnergyType::ForwardActive, 0),
            avg(|s| s.power_c),
            now,
        );

        // 正向无功：总 + 费率
        let avg_reactive = avg(|s| s.reactive_total);
        update((0, 0, EnergyType::ForwardReactive, 0), avg_reactive, now);
        update(
            (0, 0, EnergyType::ForwardReactive, current_rate),
            avg_reactive,
            now,
        );
    }

    /// 步骤8: 更新派生状态字（设计方案 4.6.1 节）
    ///
    /// 状态字由瞬时量/故障态等权威字段在每次 tick 后重新计算刷新。
    /// 状态字4/5/6 = A/B/C 相故障状态，状态字7 = 合相故障状态。
    /// 位定义（简化，精确位映射见附录 C）：bit0=失压 bit1=过压 bit2=失流 bit3=过流。
    fn update_derived_status(&self, state: &mut MeterState) {
        // 状态字1: 功率方向
        state.derived_status.status_word_1 = if state.active_power_total > 0.0 {
            0x0001
        } else {
            0x0002
        };

        let mut phase_flags = [0u16; 3]; // A, B, C
        for &(event_type, sub_type) in &self.active_faults {
            if !(0x01..=0x03).contains(&sub_type) {
                continue;
            }
            let idx = (sub_type - 1) as usize;
            let bit = match event_type {
                0x01 => 0, // 失压
                0x03 => 1, // 过压
                0x0B => 2, // 失流
                0x0C => 3, // 过流
                _ => continue,
            };
            phase_flags[idx] |= 1 << bit;
        }

        state.derived_status.status_word_4 = phase_flags[0];
        state.derived_status.status_word_5 = phase_flags[1];
        state.derived_status.status_word_6 = phase_flags[2];
        state.derived_status.status_word_7 = phase_flags[0] | phase_flags[1] | phase_flags[2];
    }

    /// 步骤5: 事件检测（生成/结束事件记录）
    ///
    /// 按 DL/T 645 附录 A.4 真实事件编码，事件 key = (DI2=事件大类, DI1=相别/0=系统级)。
    ///
    /// 阈值自动检测的相别事件：
    ///   01=失压(<70%Un)  02=欠压(70%~90%Un)  03=过压(>120%Un)  04=断相(<10%Un)
    ///   0B=失流(<5%Ib)   0C=过流(>120%Ib)；DI1：01=A相 02=B相 03=C相
    /// 阈值自动检测的系统级事件：
    ///   05=全失压（三相均失压）  0F=掉电（三相均断相）；DI1=00
    /// 仅允许注入的系统级事件：
    ///   06=辅助电源失电 07=电压逆相序 08=电流逆相序 09=电压不平衡 0A=电流不平衡 32=清零记录
    /// 30=编程记录 / 31=校时记录由写命令与校时命令自动生成。
    fn detect_events(&mut self, state: &mut MeterState) {
        let voltage_base = state.rated_voltage as f64 / 1000.0; // 伏特
        let current_base = state.rated_current as f64 / 1000.0; // 安培

        // 失压 70% / 欠压 90% / 断相 10%（simulation_algorithms §5.1），
        // 过压/过流 120%，失流 5%
        let loss_voltage_threshold = voltage_base * 0.7;
        let under_voltage_threshold = voltage_base * 0.9;
        let phase_loss_threshold = voltage_base * 0.1;
        let over_voltage_threshold = voltage_base * 1.2;
        let over_current_threshold = current_base * 1.2;
        let loss_current_threshold = current_base * 0.05;

        let phases = [
            (0x01u8, state.voltage_a, state.current_a), // A相
            (0x02u8, state.voltage_b, state.current_b), // B相
            (0x03u8, state.voltage_c, state.current_c), // C相
        ];

        let now = state.virtual_time;

        let mut loss_voltage_count = 0usize;
        let mut phase_loss_count = 0usize;
        for (phase, voltage, current) in phases {
            let loss_voltage = voltage < loss_voltage_threshold;
            let phase_loss = voltage < phase_loss_threshold;
            if loss_voltage {
                loss_voltage_count += 1;
            }
            if phase_loss {
                phase_loss_count += 1;
            }
            // 断相必然伴随失压
            self.transition_fault(state, now, 0x04, phase, phase_loss);
            self.transition_fault(state, now, 0x01, phase, loss_voltage);
            // 欠压：失压阈值之上、90% 额定之下
            self.transition_fault(
                state,
                now,
                0x02,
                phase,
                !loss_voltage && voltage < under_voltage_threshold,
            );
            self.transition_fault(state, now, 0x03, phase, voltage > over_voltage_threshold);
            self.transition_fault(state, now, 0x0C, phase, current > over_current_threshold);
            self.transition_fault(state, now, 0x0B, phase, current < loss_current_threshold);
        }

        // 系统级事件：全失压（三相均失压）、掉电（三相均断相）
        self.transition_fault(state, now, 0x05, 0x00, loss_voltage_count == 3);
        self.transition_fault(state, now, 0x0F, 0x00, phase_loss_count == 3);

        // 手动注入的故障每帧强制生效（transition_fault 幂等，不会重复生成事件）；
        // 解除注入后下一帧自动回到阈值判定。
        let forced: Vec<(u8, u8)> = self.forced_faults.iter().copied().collect();
        for (event_type, phase) in forced {
            self.transition_fault(state, now, event_type, phase, true);
        }
    }

    /// 故障注入开关（事件生成测试用）：active=true 强制指定故障持续生效，
    /// active=false 解除注入（下一帧回到阈值自动判定，故障自然结束并生成结束记录）。
    pub fn set_forced_fault(&mut self, event_type: u8, phase: u8, active: bool) {
        let key = (event_type, phase);
        if active {
            self.forced_faults.insert(key);
        } else {
            self.forced_faults.remove(&key);
        }
    }

    /// 根据故障状态跃迁，生成或结束事件记录
    ///
    /// 事件数据按附录A.4故障类记录的尾段（119字节）组织：
    /// - [0..16]  四类总电能增量（结束时回填）
    /// - [16..63] A/B/C 各相：16字节增量 + 13字节故障时刻量
    ///   （电压2 + 电流3 + 有功3 + 无功3 + 功率因数2，故障开始时填充本相数据）
    /// - [63..79] 失压/失流类的安时数（未建模，为0）
    fn transition_fault(
        &mut self,
        state: &mut MeterState,
        now: DateTime<Utc>,
        event_type: u8,
        sub_type: u8,
        active: bool,
    ) {
        let key = (event_type, sub_type);
        if active {
            if !self.active_faults.contains(&key) {
                self.active_faults.insert(key);

                // 故障时刻的本相瞬时量
                let (voltage, current, active_p, reactive_p, pf) = match sub_type {
                    0x01 => (
                        state.voltage_a,
                        state.current_a,
                        state.active_power_a,
                        state.reactive_power_a,
                        state.power_factor,
                    ),
                    0x02 => (
                        state.voltage_b,
                        state.current_b,
                        state.active_power_b,
                        state.reactive_power_b,
                        state.power_factor,
                    ),
                    _ => (
                        state.voltage_c,
                        state.current_c,
                        state.active_power_c,
                        state.reactive_power_c,
                        state.power_factor,
                    ),
                };

                let mut data = vec![0u8; 119];
                let phase_idx = (sub_type.saturating_sub(1).min(2)) as usize;
                let offset = 16 + phase_idx * 29 + 16;
                data[offset..offset + 2].copy_from_slice(&bcd_voltage(voltage));
                data[offset + 2..offset + 5].copy_from_slice(&bcd_current(current));
                data[offset + 5..offset + 8].copy_from_slice(&bcd_power(active_p));
                data[offset + 8..offset + 11].copy_from_slice(&bcd_power(reactive_p));
                data[offset + 11..offset + 13].copy_from_slice(&bcd_power_factor(pf));

                // 电能基线
                self.fault_start_energies.insert(
                    key,
                    [
                        state.get_energy(EnergyType::ForwardActive, None),
                        state.get_energy(EnergyType::ReverseActive, None),
                        state.get_energy(EnergyType::ForwardReactive, None),
                        state.get_energy(EnergyType::ReverseReactive, None),
                    ],
                );

                state.add_event_record(event_type, sub_type, now, data);
            }
        } else if self.active_faults.remove(&key) {
            let increments = self
                .fault_start_energies
                .remove(&key)
                .map(|base| {
                    [
                        state.get_energy(EnergyType::ForwardActive, None) - base[0],
                        state.get_energy(EnergyType::ReverseActive, None) - base[1],
                        state.get_energy(EnergyType::ForwardReactive, None) - base[2],
                        state.get_energy(EnergyType::ReverseReactive, None) - base[3],
                    ]
                })
                .unwrap_or([0.0; 4]);
            state.finalize_fault_event(event_type, sub_type, now, increments);
        }
    }

    /// 步骤6: 冻结调度检测（轻量级，只设置标志）
    ///
    /// 检测类型：
    /// 1. 定时冻结：每整点/日/月触发
    /// 2. 日冻结 / 整点冻结：各自独立的模式字+时间参数
    /// 3. 约定冻结：检查约定时间（一次性）
    ///
    /// 性能要求：只做时间比较，耗时 < 0.5μs
    ///
    /// 工作流程：
    /// - 检测到冻结点 → push 进 state.pending_freeze_triggers
    /// - MeterActor 检测到非空 → 依次异步生成快照
    /// - 全部处理完成 → 清空列表
    ///
    /// 注意：这里**不会**在命中一种冻结后提前返回。同一个虚拟时刻完全
    /// 可能同时满足多种冻结条件——最典型的就是"定时冻结=按日周期"
    /// （每天 00:00:00）和"日冻结时间=00:00"同时配置，两者会在同一秒
    /// 都成立。如果检测到一种就 return，后面的条件那一秒就永远没机会
    /// 被判断到，等于那种冻结被无声吞掉。所以每种冻结都要独立判断、
    /// 独立 push，谁也不抢谁的。
    fn check_freeze_schedule(&self, state: &mut MeterState) {
        let now = state.virtual_time;

        // ─────────────────────────────────────────────────────────────
        // 1. 定时冻结检测
        // ─────────────────────────────────────────────────────────────
        match state.freeze_config.timed_freeze_mode {
            3 => {
                // 按小时周期：每小时整点（XX:00:00）
                if now.minute() == 0 && now.second() == 0 {
                    state.pending_freeze_triggers.push(FreezeType::Timed);
                }
            }
            2 => {
                // 按日周期：每天00:00:00
                if now.hour() == 0 && now.minute() == 0 && now.second() == 0 {
                    state.pending_freeze_triggers.push(FreezeType::Timed);
                }
            }
            1 => {
                // 按月周期：每月1日00:00:00
                use chrono::Datelike;
                if now.day() == 1 && now.hour() == 0 && now.minute() == 0 && now.second() == 0 {
                    state.pending_freeze_triggers.push(FreezeType::Timed);
                }
            }
            _ => {}
        }

        // ─────────────────────────────────────────────────────────────
        // 2. 日冻结检测（04-00-09-06 模式字 + 04-00-12-03 日冻结时间 hhmm）
        // ─────────────────────────────────────────────────────────────
        if state.daily_freeze_mode != 0 {
            let hh = bcd_to_dec(state.daily_freeze_time[0]);
            let mm = bcd_to_dec(state.daily_freeze_time[1]);
            if now.hour() == hh && now.minute() == mm && now.second() == 0 {
                state.pending_freeze_triggers.push(FreezeType::Daily);
            }
        }

        // ─────────────────────────────────────────────────────────────
        // 3. 整点冻结检测（04-00-09-05 模式字 + 04-00-12-01 起始时间
        //    + 04-00-12-02 间隔分钟）
        //
        // 对齐方式：分钟数按"周期"对齐到标准刻度（60分钟→整点 :00；
        // 15分钟→ :00/:15/:30/:45……），而不是用 (now - start) 对 interval
        // 取模。取模方式在 start 本身没有落在整点/整刻度上时（比如
        // start 配的是 xx:05）会把所有触发点整体偏移成 :05/:20/:35/:50，
        // 偏离标准刻度；这里的 start 只作为"从什么时候开始生效"的闸门，
        // 不参与刻度计算，才能保证周期分钟数据永远是 0/15/30/45 这类值，
        // 秒永远是 0。
        // ─────────────────────────────────────────────────────────────
        if state.hourly_freeze_mode != 0 && state.hourly_freeze_interval_min > 0 {
            if let Some(start) = decode_bcd_datetime(&state.hourly_freeze_start) {
                let interval = state.hourly_freeze_interval_min as i64;
                if now >= start && now.second() == 0 {
                    let minute_of_day = now.hour() as i64 * 60 + now.minute() as i64;
                    if minute_of_day % interval == 0 {
                        state.pending_freeze_triggers.push(FreezeType::Hourly);
                    }
                }
            }
        }

        // ─────────────────────────────────────────────────────────────
        // 4. 约定冻结检测（04-00-09-04 模式字 + appointment_freeze_time，一次性）
        // ─────────────────────────────────────────────────────────────
        if state.freeze_config.appointment_freeze_mode != 0 && !state.appointment_freeze_fired {
            if let Some(target) = decode_bcd_datetime(&state.appointment_freeze_time) {
                if now >= target {
                    state.appointment_freeze_fired = true;
                    state.pending_freeze_triggers.push(FreezeType::Appointment);
                }
            }
        }

        // ─────────────────────────────────────────────────────────────
        // 5. 瞬时冻结（由外部命令触发，不在此检测）
        // ─────────────────────────────────────────────────────────────
    }

    /// 步骤7: 负荷记录采样检测
    ///
    /// 检查是否需要进行负荷记录采样，并返回待持久化的采样行
    ///
    /// 采样规则（附录B + 04-00-09-01/0A-xx）：
    /// - 起始时间（04-00-0A-01，MMDDhhmm BCD）未到不采样；全 0 视为立即启用
    /// - 采样节拍按"类"驱动：第1~6类各自有独立间隔（04-00-0A-02~07）
    /// - 模式字（04-00-09-01）选通的是附录B六个数据块（bit0~bit5），
    ///   不是采样类别；模式字为 0 时无任何数据块可记，直接不采样
    /// - 每个节拍一次性采集全部选通块、整行落库（时间对齐与块原子性由此保证）
    ///
    /// 返回：需要持久化的采样行列表（每类每节拍最多一行）
    pub fn check_load_record_sampling(
        &self,
        state: &mut MeterState,
    ) -> Vec<super::state::LoadRecordSample> {
        use super::state::LoadRecordData;

        let mut samples = Vec::new();
        let now = state.virtual_time;

        // 起始时间门控：配置了起始时间且未到达时不采样
        if let Some(start) = decode_bcd_start_time(&state.load_record_start_time, now) {
            if now < start {
                return samples;
            }
        }

        // 模式字未选通任何数据块：无负荷记录可写
        let mode_word = state.load_record_config.mode_word;
        if mode_word == 0 {
            return samples;
        }

        // 逐类检查采样节拍（class_idx = 类号-1）
        for class_idx in 0..6 {
            let interval_minutes = state.load_record_config.intervals[class_idx];
            if interval_minutes == 0 {
                continue; // 间隔为0表示该类不采样
            }

            if state
                .load_profile_state
                .should_sample(class_idx, &now, interval_minutes)
            {
                // 整行采集：一次抓全部选通块，时间戳取同一虚拟时钟
                let data = LoadRecordData::from_meter_state(state, mode_word);
                let sample = super::state::LoadRecordSample {
                    class_id: (class_idx + 1) as u8,
                    sample_time: now,
                    data: data.clone(),
                };
                
                samples.push(sample.clone());

                // 更新内存中的最近记录（保留最近5次）
                let class_id = (class_idx + 1) as u8;
                let recent_records = state.recent_load_records.entry(class_id).or_insert_with(VecDeque::new);
                recent_records.push_front(sample); // 最新的在前
                while recent_records.len() > 5 {
                    recent_records.pop_back(); // 移除最旧的
                }

                state
                    .load_profile_state
                    .update_sample_time(class_idx, now);
            }
        }

        samples
    }

    /// 生成小噪声 (-1.0 ~ 1.0)
    fn small_noise() -> f64 {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        rng.gen_range(-1.0..1.0)
    }
}

// ============================================
// 负荷模型配置
// ============================================

/// 负荷模型配置
#[derive(Debug, Clone)]
pub struct LoadModelConfig {
    pub profile: LoadProfile,
    pub voltage_noise_v: f64,
    pub frequency_noise_hz: f64,
    pub power_factor_noise: f64,
    pub power_factor_min: f64,
    pub power_factor_max: f64,
    pub phase_current_factors: [f64; 3],
}

impl Default for LoadModelConfig {
    fn default() -> Self {
        Self {
            profile: LoadProfile::Residential,
            voltage_noise_v: 2.0,
            frequency_noise_hz: 0.05,
            power_factor_noise: 0.002,
            power_factor_min: 0.85,
            power_factor_max: 0.99,
            phase_current_factors: [1.0, 0.98, 1.02],
        }
    }
}

impl LoadModelConfig {
    /// 根据当前时间获取负荷系数 (0.0 ~ 1.0)
    pub fn get_load_factor(&self, time: &DateTime<Utc>) -> f64 {
        self.profile.get_load_factor(time.hour())
    }
}

/// 负载类型（设计方案 simulation_algorithms.md 第4节）
#[derive(Debug, Clone, Copy)]
pub enum LoadProfile {
    Residential, // 居民用电
    Industrial,  // 工业用电
    Commercial,  // 商业用电
    Fixed(f64),  // 固定负荷（0.0-1.0）
}

impl LoadProfile {
    /// 获取指定时刻的负载系数 (0.0 ~ 1.0)
    fn get_load_factor(&self, hour: u32) -> f64 {
        match self {
            LoadProfile::Fixed(factor) => *factor,
            LoadProfile::Residential => match hour {
                0..=5 => 0.2,   // 深夜低谷
                6..=8 => 0.7,   // 早高峰
                9..=16 => 0.4,  // 白天
                17..=21 => 1.0, // 晚高峰
                22..=23 => 0.3, // 夜间
                _ => 0.3,
            },
            LoadProfile::Industrial => match hour {
                0..=5 => 0.3,   // 夜班
                6..=7 => 0.6,   // 交班
                8..=17 => 1.0,  // 白班高峰
                18..=21 => 0.7, // 晚班
                22..=23 => 0.4, // 夜班
                _ => 0.3,
            },
            LoadProfile::Commercial => match hour {
                0..=7 => 0.1,   // 休息
                8..=11 => 0.8,  // 上午营业
                12..=13 => 1.0, // 午餐高峰
                14..=17 => 0.7, // 下午
                18..=20 => 0.9, // 晚餐高峰
                21..=23 => 0.4, // 收尾
                _ => 0.1,
            },
        }
    }
}

// ============================================
// 物理引擎配置（供外部创建时使用）
// ============================================

/// 物理引擎配置
#[derive(Debug, Clone)]
pub struct PhysicsConfig {
    pub load_model: LoadModelConfig,
}

/// Complete, per-meter input contract for the simulation calculation.
/// All values are expressed in display units (V, A, Hz, kWh pulses).
#[derive(Debug, Clone)]
pub struct SimulationConfig {
    pub load_model: LoadModelConfig,
    pub rated_voltage: f64,
    pub rated_current: f64,
    pub rated_frequency: f64,
    pub power_factor: f64,
    pub meter_constant: u32,
    pub demand_period_minutes: u16,
    pub time_scale: f64,
}

impl SimulationConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(100.0..=1_000.0).contains(&self.rated_voltage) {
            return Err("rated voltage must be 100..1000 V".into());
        }
        if !(0.1..=10_000.0).contains(&self.rated_current) {
            return Err("rated current must be 0.1..10000 A".into());
        }
        if !(45.0..=65.0).contains(&self.rated_frequency) {
            return Err("rated frequency must be 45..65 Hz".into());
        }
        if !(0.0..=1.0).contains(&self.power_factor) {
            return Err("power factor must be 0..1".into());
        }
        if self.meter_constant == 0 {
            return Err("meter constant must be positive".into());
        }
        if !(1..=120).contains(&self.demand_period_minutes) {
            return Err("demand period must be 1..120 minutes".into());
        }
        if !(0.01..=10_000.0).contains(&self.time_scale) {
            return Err("time scale must be 0.01..10000".into());
        }
        if self.load_model.voltage_noise_v < 0.0
            || self.load_model.frequency_noise_hz < 0.0
            || self.load_model.power_factor_noise < 0.0
        {
            return Err("noise values cannot be negative".into());
        }
        if self
            .load_model
            .phase_current_factors
            .iter()
            .any(|value| !(0.0..=3.0).contains(value))
        {
            return Err("phase factors must be 0..3".into());
        }
        if !(0.0..=1.0).contains(&self.load_model.power_factor_min)
            || !(0.0..=1.0).contains(&self.load_model.power_factor_max)
            || self.load_model.power_factor_min > self.load_model.power_factor_max
        {
            return Err("power factor range must satisfy 0 <= min <= max <= 1".into());
        }
        if let LoadProfile::Fixed(factor) = self.load_model.profile {
            if !(0.0..=1.0).contains(&factor) {
                return Err("fixed load factor must be 0..1".into());
            }
        }
        Ok(())
    }
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            load_model: LoadModelConfig::default(),
            rated_voltage: 220.0,
            rated_current: 60.0,
            rated_frequency: 50.0,
            power_factor: 0.95,
            meter_constant: 1600,
            demand_period_minutes: 15,
            time_scale: 1.0,
        }
    }
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            load_model: LoadModelConfig::default(),
        }
    }
}

/// 事件数据尾段用的 BCD 编码（XXX.X 电压 / XXX.XXX 电流 / XX.XXXX 功率 / X.XXX 功率因数）
fn bcd_voltage(v: f64) -> Vec<u8> {
    bcd_fixed(v, 2, 1)
}

fn bcd_current(v: f64) -> Vec<u8> {
    bcd_fixed(v, 3, 3)
}

fn bcd_power(v: f64) -> Vec<u8> {
    bcd_fixed(v, 3, 4)
}

fn bcd_power_factor(v: f64) -> Vec<u8> {
    bcd_fixed(v, 2, 3)
}

fn bcd_fixed(value: f64, bytes: usize, decimals: usize) -> Vec<u8> {
    let scaled = (value * 10f64.powi(decimals as i32)).round() as u64;
    let mut out = vec![0u8; bytes];
    let mut t = scaled;
    for b in out.iter_mut() {
        *b = (((t % 10) << 4) | ((t / 10) % 10)) as u8;
        t /= 100;
    }
    out
}

/// 单字节 BCD 转十进制
fn bcd_to_dec(bcd: u8) -> u32 {
    ((bcd >> 4) & 0x0F) as u32 * 10 + (bcd & 0x0F) as u32
}

/// 解码负荷记录起始时间（MMDDhhmm BCD，4字节）。
/// 年份取参考时间所在年份；全 0 返回 None（表示立即启用）。
fn decode_bcd_start_time(bcd: &[u8; 4], reference: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use chrono::{Datelike, TimeZone};
    let (mm, dd, hh, mi) = (
        bcd_to_dec(bcd[0]),
        bcd_to_dec(bcd[1]),
        bcd_to_dec(bcd[2]),
        bcd_to_dec(bcd[3]),
    );
    if mm == 0 && dd == 0 && hh == 0 && mi == 0 {
        return None;
    }
    chrono::Utc
        .with_ymd_and_hms(reference.year(), mm, dd, hh, mi, 0)
        .single()
}

/// 解码 BCD 时间（YYMMDDhhmm, 5 字节）为本地时间
///
/// `pub(crate)`：virtual_meter.rs 在恢复约定冻结的 `appointment_freeze_fired`
/// 状态时也需要用同一份解码逻辑判断"约定时间是否已经过去"。
pub(crate) fn decode_bcd_datetime(bcd: &[u8; 5]) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;
    let yy = bcd_to_dec(bcd[0]);
    let mm = bcd_to_dec(bcd[1]);
    let dd = bcd_to_dec(bcd[2]);
    let hh = bcd_to_dec(bcd[3]);
    let mi = bcd_to_dec(bcd[4]);
    chrono::Utc
        .with_ymd_and_hms(2000 + yy as i32, mm, dd, hh, mi, 0)
        .single()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::state::MeterState;

    #[test]
    fn test_physics_tick() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        let initial_time = state.virtual_time;

        // 模拟 1秒（时间倍率 1.0）
        engine.tick(&mut state, Duration::from_secs(1), 1.0);

        // 验证虚拟时钟推进了 1 秒
        let time_diff = state.virtual_time.signed_duration_since(initial_time);
        assert_eq!(time_diff.num_seconds(), 1);

        // 验证瞬时量被更新
        assert!(state.voltage_a > 0.0);
        assert!(state.current_a > 0.0);
        assert!(state.active_power_total > 0.0);

        println!("虚拟时间: {}", state.virtual_time);
        println!("电压A: {:.1}V", state.voltage_a);
        println!("电流A: {:.3}A", state.current_a);
        println!("有功功率: {:.4}kW", state.active_power_total);
        println!("功率因数: {:.3}", state.power_factor);
    }

    #[test]
    fn test_time_scale() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        let initial_time = state.virtual_time;

        // 模拟 1 秒，但时间倍率 10.0（加速 10 倍）
        engine.tick(&mut state, Duration::from_secs(1), 10.0);

        // 验证虚拟时钟推进了 10 秒
        let time_diff = state.virtual_time.signed_duration_since(initial_time);
        assert_eq!(time_diff.num_seconds(), 10);

        println!(
            "真实流逝: 1秒, 时间倍率: 10x, 虚拟推进: {}秒",
            time_diff.num_seconds()
        );
    }

    #[test]
    fn test_demand_registers_tracked_per_dimension() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        engine.tick(&mut state, Duration::from_secs(1), 1.0);

        // 正向有功：总、当前费率、分相均已寄存
        let rate = state
            .tou_config
            .day_table_1
            .get_rate_at_time(&state.virtual_time);
        assert!(state
            .demand_registers
            .contains_key(&(0, 0, EnergyType::ForwardActive, 0)));
        assert!(state
            .demand_registers
            .contains_key(&(0, 0, EnergyType::ForwardActive, rate)));
        for phase in 1..=3u8 {
            assert!(state
                .demand_registers
                .contains_key(&(0, phase, EnergyType::ForwardActive, 0)));
        }
        // 正向无功总需量
        assert!(state
            .demand_registers
            .contains_key(&(0, 0, EnergyType::ForwardReactive, 0)));
    }

    #[test]
    fn test_fault_injection_generates_events() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 注入 A 相失压（0x01）与系统级电压逆相序（0x07）
        engine.set_forced_fault(0x01, 1, true);
        engine.set_forced_fault(0x07, 0, true);
        engine.tick(&mut state, Duration::from_secs(1), 1.0);

        assert_eq!(state.get_all_event_records(0x01, 0x01).len(), 1);
        assert_eq!(state.get_all_event_records(0x07, 0x00).len(), 1);
        // 故障事件数据为附录A.4尾段（119字节）
        let record = state.get_all_event_records(0x01, 0x01)[0];
        assert_eq!(record.data.len(), 119);

        // 解除注入 → 下一帧阈值判定结束事件
        engine.set_forced_fault(0x01, 1, false);
        engine.tick(&mut state, Duration::from_secs(1), 1.0);
        let record = state.get_all_event_records(0x01, 0x01)[0];
        assert!(record.end_time.is_some());
    }

    #[test]
    fn test_under_voltage_band_detection() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();
        // 额定 220V，A 相压到 80%（欠压区间 70%~90%，非失压）
        state.voltage_a = 176.0;
        engine.detect_events(&mut state);

        assert_eq!(state.get_all_event_records(0x02, 0x01).len(), 1); // 欠压
        assert_eq!(state.get_all_event_records(0x01, 0x01).len(), 0); // 非失压
    }

    #[test]
    fn test_daily_freeze_triggered_by_config() {
        use chrono::{Utc, TimeZone};

        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();
        // 每日 00:30 日冻结
        state.daily_freeze_mode = 1;
        state.daily_freeze_time = [0x00, 0x30];

        // 虚拟时钟推到 00:30:00
        state.virtual_time = Utc
            .with_ymd_and_hms(2025, 6, 2, 0, 30, 0)
            .single()
            .unwrap();
        engine.check_freeze_schedule(&mut state);
        assert!(!state.pending_freeze_triggers.is_empty());

        // 非冻结时刻不触发
        state.pending_freeze_triggers.clear();
        state.virtual_time = Utc
            .with_ymd_and_hms(2025, 6, 2, 0, 31, 0)
            .single()
            .unwrap();
        engine.check_freeze_schedule(&mut state);
        assert!(state.pending_freeze_triggers.is_empty());
    }

    #[test]
    fn test_settlement_rollover_resets_demand() {
        use chrono::{Utc, TimeZone};

        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();
        state.settlement_days = [1, 0, 0];

        // 从上月末推进跨过本月1日0点
        state.virtual_time = Utc
            .with_ymd_and_hms(2025, 5, 31, 23, 59, 50)
            .single()
            .unwrap();
        engine.tick(&mut state, Duration::from_secs(15), 1.0);

        // 跨过结算日：当前需量归零，上1结算日槽位有值
        assert_eq!(state.max_demand, 0.0);
        assert!(state
            .demand_registers
            .contains_key(&(1, 0, EnergyType::ForwardActive, 0)));
    }

    #[test]
    fn test_energy_accumulation() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        let initial_energy = state.get_energy(EnergyType::ForwardActive, None);

        // 模拟 1小时（3600秒，时间倍率 1.0）
        for _ in 0..3600 {
            engine.tick(&mut state, Duration::from_secs(1), 1.0);
        }

        let final_energy = state.get_energy(EnergyType::ForwardActive, None);

        // 验证电能增加了
        assert!(final_energy > initial_energy);

        println!("初始电能: {:.4} kWh", initial_energy);
        println!("1小时后电能: {:.4} kWh", final_energy);
        println!("电能增量: {:.4} kWh", final_energy - initial_energy);
    }

    #[test]
    fn test_rate_energy_accumulation() {
        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 模拟 1 小时
        for _ in 0..3600 {
            engine.tick(&mut state, Duration::from_secs(1), 1.0);
        }

        // 检查各费率的电能
        let total = state.get_energy(EnergyType::ForwardActive, None);
        let rate1 = state.get_energy(EnergyType::ForwardActive, Some(1));
        let rate2 = state.get_energy(EnergyType::ForwardActive, Some(2));
        let rate3 = state.get_energy(EnergyType::ForwardActive, Some(3));
        let rate4 = state.get_energy(EnergyType::ForwardActive, Some(4));

        println!("总电能: {:.4} kWh", total);
        println!("费率1（尖）: {:.4} kWh", rate1);
        println!("费率2（峰）: {:.4} kWh", rate2);
        println!("费率3（平）: {:.4} kWh", rate3);
        println!("费率4（谷）: {:.4} kWh", rate4);

        // 验证总和（总电能应该 = 各费率之和）
        let sum_rates = rate1 + rate2 + rate3 + rate4;
        let diff = (total - sum_rates).abs();
        assert!(diff < 0.001, "总电能应等于各费率之和，差值: {}", diff);
    }

    // ═══════════════════════════════════════════════════════════════
    // 冻结功能测试
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_freeze_trigger_detection() {
        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 配置小时周期冻结（模式字3）
        state.freeze_config.timed_freeze_mode = 3;

        // 设置虚拟时间为整点前1秒
        use chrono::{Utc, TimeZone};
        state.virtual_time = Utc.with_ymd_and_hms(2024, 6, 15, 10, 59, 59).unwrap();

        // 第1次tick：59秒，不应触发
        engine.check_freeze_schedule(&mut state);
        assert!(state.pending_freeze_triggers.is_empty(), "59秒不应触发冻结");

        // 推进到整点
        state.virtual_time = Utc.with_ymd_and_hms(2024, 6, 15, 11, 0, 0).unwrap();

        // 第2次tick：整点，应触发
        engine.check_freeze_schedule(&mut state);
        assert_eq!(state.pending_freeze_triggers, vec![FreezeType::Timed], "整点应触发冻结");

        println!("✓ 小时周期冻结触发检测正常");
    }

    #[test]
    fn test_freeze_snapshot_creation() {
        use crate::simulation::state::FreezeTrigger;

        let mut state = MeterState::default();

        // 设置一些初始数据
        use crate::simulation::state::EnergyType;
        state
            .energy_registers
            .insert((EnergyType::ForwardActive, 0), 12345.67); // 正向有功总
        state.voltage_a = 220.5;
        state.current_a = 12.345;

        // 创建定时冻结快照
        state.create_freeze_snapshot(FreezeTrigger::Timed);

        // 验证快照已创建
        let snapshot = state.get_freeze_snapshot(FreezeTrigger::Timed, 0x01);
        assert!(snapshot.is_some(), "应该创建了快照");

        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.trigger_type, FreezeTrigger::Timed);
        assert_eq!(snapshot.occurrence_index, 0x01); // 第1次
        assert!((snapshot.data.forward_active_total - 12345.67).abs() < 0.01);

        println!("✓ 冻结快照创建正常");
        println!("  快照时间: {}", snapshot.snapshot_time);
        println!("  正向有功: {:.2} kWh", snapshot.data.forward_active_total);
    }

    #[test]
    fn test_freeze_ring_buffer() {
        use crate::simulation::state::FreezeTrigger;

        let mut state = MeterState::default();

        // 创建13次定时冻结（环形缓冲容量12）
        for i in 1..=13 {
            state
                .energy_registers
                .insert((EnergyType::ForwardActive, 0), 1000.0 * i as f64);
            state.create_freeze_snapshot(FreezeTrigger::Timed);
        }

        // 验证环形缓冲行为
        let snapshot_01 = state.get_freeze_snapshot(FreezeTrigger::Timed, 0x01);
        let snapshot_0c = state.get_freeze_snapshot(FreezeTrigger::Timed, 0x0C);

        assert!(snapshot_01.is_some(), "最新的快照应该存在");
        assert!(snapshot_0c.is_some(), "第12次快照应该存在");

        // 第1次（最新）应该是第13次写入的数据
        let latest = snapshot_01.unwrap();
        assert!((latest.data.forward_active_total - 13000.0).abs() < 0.01);

        // 第12次应该是第2次写入的数据（第1次被覆盖）
        let oldest = snapshot_0c.unwrap();
        assert!((oldest.data.forward_active_total - 2000.0).abs() < 0.01);

        println!("✓ 冻结环形缓冲正常");
        println!(
            "  最新快照电能: {:.2} kWh",
            latest.data.forward_active_total
        );
        println!(
            "  最旧快照电能: {:.2} kWh",
            oldest.data.forward_active_total
        );
    }

    #[test]
    fn test_freeze_instant_command() {
        use crate::simulation::state::FreezeTrigger;

        let mut state = MeterState::default();
        state
            .energy_registers
            .insert((EnergyType::ForwardActive, 0), 5555.55);

        // 模拟瞬时冻结命令（瞬时冻结模式设为1=启用）
        state.freeze_config.instant_freeze_mode = 1;

        // 立即创建瞬时冻结快照
        state.create_freeze_snapshot(FreezeTrigger::Instant);

        // 验证
        let snapshot = state.get_freeze_snapshot(FreezeTrigger::Instant, 0x01);
        assert!(snapshot.is_some());

        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.trigger_type, FreezeTrigger::Instant);
        assert!((snapshot.data.forward_active_total - 5555.55).abs() < 0.01);

        println!("✓ 瞬时冻结正常");
    }

    // ═══════════════════════════════════════════════════════════════
    // 负荷记录测试
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_load_record_sampling_logic() {
        use chrono::{Duration, TimeZone};
        
        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 配置负荷记录：模式字选通电压电流频率块，第1类间隔15分钟
        state.load_record_config.mode_word = 0b00000001; // bit0=电压电流频率块
        state.load_record_config.intervals[0] = 15; // 第1类15分钟

        // 设置虚拟时间为非对齐点：09:07:30（不在0/15/30/45分上）
        state.virtual_time = chrono::Utc.with_ymd_and_hms(2026, 8, 20, 9, 7, 30).unwrap();

        // 第一次检查：不在对齐点，不应采样
        let samples = engine.check_load_record_sampling(&mut state);
        assert!(samples.is_empty(), "非对齐点不应采样");

        // 推进到对齐点：09:15:00
        state.virtual_time = chrono::Utc.with_ymd_and_hms(2026, 8, 20, 9, 15, 0).unwrap();
        let samples = engine.check_load_record_sampling(&mut state);
        assert_eq!(samples.len(), 1, "在对齐点应该采样");
        assert_eq!(samples[0].class_id, 1);
        assert_eq!(samples[0].sample_time, state.virtual_time);
        assert!(samples[0].data.vif.is_some(), "模式字 bit0 选通 vif 块");
        assert!(samples[0].data.pq.is_none(), "未选通的块应为 None");

        let vif = samples[0].data.vif.as_ref().unwrap();
        assert!((vif.voltage_a - 220.0).abs() < 1.0, "A相电压应接近220V");
        assert!((vif.frequency - 50.0).abs() < 1.0, "频率应接近50Hz");

        println!("✓ 首次负荷记录采样在对齐点触发");

        // 推进2分钟到09:17:00，不应再次采样（未跨越下一个对齐点）
        state.virtual_time = state.virtual_time + Duration::minutes(2);
        let samples = engine.check_load_record_sampling(&mut state);
        assert!(samples.is_empty(), "2分钟后不应采样（未到下一个对齐点）");

        // 推进到下一个对齐点：09:30:00
        state.virtual_time = chrono::Utc.with_ymd_and_hms(2026, 8, 20, 9, 30, 0).unwrap();
        let samples = engine.check_load_record_sampling(&mut state);
        assert_eq!(samples.len(), 1, "到达下一个对齐点应该再次采样");

        println!("✓ 负荷记录采样间隔控制正常（对齐到0/15/30/45分）");
    }

    #[test]
    fn test_load_record_mode_word_selects_blocks() {
        use chrono::TimeZone;
        
        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 设置虚拟时间为对齐点（秒数为0）
        state.virtual_time = chrono::Utc.with_ymd_and_hms(2026, 8, 20, 9, 0, 0).unwrap();

        // 设置一些已知值
        state.voltage_a = 220.0;
        state.current_a = 10.0;
        state.active_power_a = 2.2;
        state.power_factor = 0.95;

        // 模式字选通 bit0（vif）+ bit1（pq）+ bit2（pf）
        state.load_record_config.mode_word = 0b00000111;
        state.load_record_config.intervals[1] = 5; // 第2类5分钟

        let samples = engine.check_load_record_sampling(&mut state);
        assert_eq!(samples.len(), 1, "只有第2类配置了间隔");
        assert_eq!(samples[0].class_id, 2);

        let data = &samples[0].data;
        let vif = data.vif.as_ref().unwrap();
        assert!((vif.voltage_a - 220.0).abs() < 0.1, "A相电压应为220V");
        assert!((vif.current_a - 10.0).abs() < 0.1, "A相电流应为10A");

        let pq = data.pq.as_ref().unwrap();
        assert!((pq.active_a - 2.2).abs() < 0.1, "A相有功功率应为2.2kW");

        let pf = data.pf.as_ref().unwrap();
        assert!((pf.total - 0.95).abs() < 0.01, "功率因数应为0.95");

        // 未选通的块不存在
        assert!(data.energy.is_none());
        assert!(data.quadrant.is_none());
        assert!(data.demand.is_none());

        println!("✓ 负荷记录模式字块选通正常");
    }

    #[test]
    fn test_load_record_mode_word_zero_disables_sampling() {
        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 模式字为0（无任何数据块），即使配了间隔也不采样
        state.load_record_config.mode_word = 0;
        state.load_record_config.intervals[0] = 5;

        let samples = engine.check_load_record_sampling(&mut state);
        assert!(samples.is_empty(), "模式字为0时不应采样");

        println!("✓ 负荷记录模式字0禁用采样正常");
    }

    #[test]
    fn test_load_record_interval_zero_disables_sampling() {
        let engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 选通电压块，但间隔设为0
        state.load_record_config.mode_word = 0b00000001;
        state.load_record_config.intervals[0] = 0; // 间隔0=禁用

        let samples = engine.check_load_record_sampling(&mut state);

        // 不应该有任何采样
        assert!(samples.is_empty(), "间隔为0时不应采样");

        println!("✓ 负荷记录间隔0禁用采样正常");
    }

    // ═══════════════════════════════════════════════════════════════
    // 集成测试
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_freeze_and_load_integration() {
        use crate::simulation::state::FreezeTrigger;
        use chrono::{Utc, TimeZone};

        let mut engine = PhysicsEngine::new(LoadModelConfig::default());
        let mut state = MeterState::default();

        // 配置冻结和负荷记录：模式字选通 vif+pq+pf 三块，第1类15分钟
        state.freeze_config.timed_freeze_mode = 3; // 小时周期
        state.load_record_config.mode_word = 0b00000111;
        state.load_record_config.intervals[0] = 15;

        // 设置初始时间：10:45:00
        state.virtual_time = Utc.with_ymd_and_hms(2024, 6, 15, 10, 45, 0).unwrap();

        // 模拟运行30分钟（1800秒）
        for _ in 0..1800 {
            engine.tick(&mut state, Duration::from_secs(1), 1.0);

            // 检查负荷记录采样
            let samples = engine.check_load_record_sampling(&mut state);
            if !samples.is_empty() {
                println!(
                    "采样时间: {}, 采样类: {:?}",
                    state.virtual_time.format("%H:%M:%S"),
                    samples.iter().map(|s| s.class_id).collect::<Vec<_>>()
                );
            }

            // 检查冻结触发
            if !state.pending_freeze_triggers.is_empty() {
                println!(
                    "冻结触发: {}, 触发类型: {:?}",
                    state.virtual_time.format("%H:%M:%S"),
                    state.pending_freeze_triggers
                );

                // 生成快照
                state.create_freeze_snapshot(FreezeTrigger::Timed);
                state.pending_freeze_triggers.clear();
            }
        }

        // 验证：应该触发了1次冻结（11:00:00）
        let snapshot = state.get_freeze_snapshot(FreezeTrigger::Timed, 0x01);
        assert!(snapshot.is_some(), "应该有1次定时冻结快照");

        // 验证：应该进行了2次负荷记录采样（10:45首次，11:00第二次）
        // 注：这里只是逻辑验证，实际采样数据需要持久化才能查询

        println!("✓ 冻结与负荷记录集成测试通过");
        println!(
            "  最终时间: {}",
            state.virtual_time.format("%Y-%m-%d %H:%M:%S")
        );
        println!(
            "  电能累计: {:.4} kWh",
            state.get_energy(EnergyType::ForwardActive, None)
        );
    }
}