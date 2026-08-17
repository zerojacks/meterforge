# 电表仿真算法设计

本文档定义虚拟电表的核心仿真算法，确保行为接近真实电表。

---

## 1. 脉冲累加模型（电能计量）

### 1.1 原理

真实电表通过脉冲计数来计量电能，每个脉冲代表固定的电能量。

**关键参数**：
- **电表常数** `K`：每千瓦时产生的脉冲数（imp/kWh）
  - 常见值：800, 1600, 3200, 6400
  - 越大表示精度越高

### 1.2 计算公式

```rust
// 每tick的电能增量计算
fn calculate_energy_increment(power_kw: f32, tick_interval_sec: f32, meter_constant: u32) -> f64 {
    // 1. 计算时间间隔（小时）
    let delta_t_hour = tick_interval_sec / 3600.0;
    
    // 2. 计算电能增量（kWh）
    let delta_energy_kwh = power_kw * delta_t_hour;
    
    // 3. 转换为脉冲数
    let delta_pulses = delta_energy_kwh * (meter_constant as f64);
    
    delta_pulses
}

// 脉冲累加逻辑
struct PulseAccumulator {
    fractional_pulses: f64,  // 未满1个脉冲的余量
}

impl MeterActor {
    fn accumulate_energy(&mut self, power_kw: f32) {
        // 计算脉冲增量
        let delta_pulses = calculate_energy_increment(
            power_kw, 
            1.0,  // 假设tick间隔1秒
            self.meter_constant
        );
        
        // 累加到缓冲区
        self.pulse_buffer.fractional_pulses += delta_pulses;
        
        // 凑够整数脉冲才进位到电能寄存器
        if self.pulse_buffer.fractional_pulses >= 1.0 {
            let whole_pulses = self.pulse_buffer.fractional_pulses.floor();
            
            // 转换为kWh并累加
            let energy_increment = whole_pulses / (self.meter_constant as f64);
            self.energy_registers[rate_index] += energy_increment;
            
            // 更新缓冲区
            self.pulse_buffer.fractional_pulses -= whole_pulses;
        }
    }
}
```

### 1.3 示例计算

**给定**：
- 功率 P = 1.5 kW
- Tick间隔 Δt = 1秒
- 电表常数 K = 3200 imp/kWh

**计算过程**：
```
Δt (小时) = 1 / 3600 = 0.000278 小时
ΔE (kWh) = 1.5 × 0.000278 = 0.000417 kWh
Δimp = 0.000417 × 3200 = 1.333 脉冲

累加1秒后：fractional_pulses = 1.333
累加2秒后：fractional_pulses = 2.666
累加3秒后：fractional_pulses = 3.999
→ 进位3个脉冲，剩余0.999

电能增量 = 3 / 3200 = 0.0009375 kWh
```

---

## 2. 最大需量滑差窗口

### 2.1 原理

需量（Demand）= 一段时间内的平均功率。
最大需量 = 所有滑动窗口中的最大平均功率。

**关键参数**：
- **需量周期** `T`：计算平均功率的时间窗口（通常15分钟）
- **滑差时间** `S`：窗口滑动步长（通常1分钟）

### 2.2 算法实现

```rust
struct DemandWindow {
    samples: VecDeque<f32>,         // 功率采样队列
    window_size_minutes: u32,       // 需量周期（15分钟）
    slide_interval_minutes: u32,    // 滑差时间（1分钟）
    sample_interval_sec: u32,       // 采样间隔（60秒）
}

impl DemandWindow {
    fn new(window_minutes: u32, slide_minutes: u32) -> Self {
        let capacity = (window_minutes * 60 / 60) as usize;  // 假设1分钟采样1次
        Self {
            samples: VecDeque::with_capacity(capacity),
            window_size_minutes: window_minutes,
            slide_interval_minutes: slide_minutes,
            sample_interval_sec: 60,
        }
    }
    
    fn add_sample(&mut self, power_kw: f32) {
        self.samples.push_back(power_kw);
        
        // 保持窗口大小
        let max_samples = (self.window_size_minutes * 60 / self.sample_interval_sec) as usize;
        while self.samples.len() > max_samples {
            self.samples.pop_front();
        }
    }
    
    fn current_demand(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        
        // 计算窗口内的平均功率
        let sum: f32 = self.samples.iter().sum();
        sum / (self.samples.len() as f32)
    }
}

impl MeterActor {
    fn update_max_demand(&mut self, current_power_kw: f32) {
        // 每分钟采样一次（在tick中判断时间）
        if self.virtual_time.timestamp() % 60 == 0 {
            self.demand_window.add_sample(current_power_kw);
        }
        
        // 计算当前需量
        let current_demand = self.demand_window.current_demand();
        
        // 更新最大需量
        if current_demand > self.max_demand_value {
            self.max_demand_value = current_demand;
            self.max_demand_time = self.virtual_time;
        }
    }
}
```

### 2.3 示例场景

**给定**：
- 需量周期 T = 15分钟
- 滑差时间 S = 1分钟
- 采样间隔 = 1分钟

**时间线**：
```
t=0:  采样 [1.0]                  → 平均 1.0 kW
t=1:  采样 [1.0, 1.2]             → 平均 1.1 kW
t=2:  采样 [1.0, 1.2, 1.5]        → 平均 1.23 kW
...
t=14: 采样 [1.0, ..., 2.0]        → 平均 1.5 kW
t=15: 采样 [1.2, ..., 2.0, 1.8]   → 平均 1.6 kW (移除最旧的1.0)
      ↑ 此时最大需量 = 1.6 kW
```

---

## 3. 时段表与费率查询

### 3.1 原理

不同时段电价不同，需要根据当前时间确定费率。

**关键数据**：
- **两套日时段表**：工作日时段表、休息日时段表
- **周休日特征字**：定义哪些星期是休息日

### 3.2 算法实现

```rust
#[derive(Clone)]
struct TimeSlot {
    start_hour: u8,
    start_minute: u8,
    end_hour: u8,
    end_minute: u8,
    rate_index: u8,  // 费率号（1-63）
}

struct TouConfig {
    slot_table_1: Vec<TimeSlot>,  // 工作日时段表
    slot_table_2: Vec<TimeSlot>,  // 休息日时段表
    rest_day_pattern: u8,          // 周休日特征字：bit0=周日, bit1=周一, ...
}

impl MeterActor {
    fn get_current_rate(&self) -> u8 {
        // 1. 判断今天是工作日还是休息日
        let weekday = self.virtual_time.weekday().num_days_from_sunday();  // 0=周日, 1=周一, ...
        let is_rest_day = (self.tou_config.rest_day_pattern & (1 << weekday)) != 0;
        
        // 2. 选择对应的时段表
        let slot_table = if is_rest_day {
            &self.tou_config.slot_table_2
        } else {
            &self.tou_config.slot_table_1
        };
        
        // 3. 查询当前时刻所在时段
        let hour = self.virtual_time.hour() as u8;
        let minute = self.virtual_time.minute() as u8;
        
        for slot in slot_table {
            if self.is_in_slot(hour, minute, slot) {
                return slot.rate_index;
            }
        }
        
        1  // 默认费率1
    }
    
    fn is_in_slot(&self, hour: u8, minute: u8, slot: &TimeSlot) -> bool {
        let current_minutes = (hour as u16) * 60 + (minute as u16);
        let slot_start = (slot.start_hour as u16) * 60 + (slot.start_minute as u16);
        let slot_end = (slot.end_hour as u16) * 60 + (slot.end_minute as u16);
        
        if slot_end > slot_start {
            // 正常时段：如 08:00-12:00
            current_minutes >= slot_start && current_minutes < slot_end
        } else {
            // 跨天时段：如 22:00-06:00
            current_minutes >= slot_start || current_minutes < slot_end
        }
    }
}
```

### 3.3 配置示例

```toml
# 工作日时段表（4费率）
[[tou.slot_table_1]]
start_hour = 0
start_minute = 0
end_hour = 8
end_minute = 0
rate_index = 4  # 谷时段

[[tou.slot_table_1]]
start_hour = 8
start_minute = 0
end_hour = 12
end_minute = 0
rate_index = 2  # 平时段

[[tou.slot_table_1]]
start_hour = 12
start_minute = 0
end_hour = 14
end_minute = 0
rate_index = 1  # 峰时段

# ... 更多时段

# 休息日全天平价
[[tou.slot_table_2]]
start_hour = 0
start_minute = 0
end_hour = 24
end_minute = 0
rate_index = 2

# 周休日特征字：0x41 = 0b01000001 = 周日和周六
[tou]
rest_day_pattern = 0x41
```

---

## 4. 负荷模型

### 4.1 固定负荷

```rust
struct FixedLoad {
    power_kw: f32,
}

impl LoadModel for FixedLoad {
    fn get_instant_power(&self, _time: DateTime<Utc>) -> f32 {
        self.power_kw
    }
}
```

### 4.2 正弦波动负荷

```rust
struct SinusoidalLoad {
    base_power_kw: f32,     // 基准功率
    amplitude_kw: f32,      // 波动幅度
    period_minutes: u32,    // 周期
}

impl LoadModel for SinusoidalLoad {
    fn get_instant_power(&self, time: DateTime<Utc>) -> f32 {
        let t = time.timestamp() as f64;
        let omega = 2.0 * std::f64::consts::PI / (self.period_minutes as f64 * 60.0);
        let variation = (omega * t).sin() as f32;
        
        self.base_power_kw + self.amplitude_kw * variation
    }
}
```

### 4.3 日负荷曲线

```rust
struct DailyLoadCurve {
    night: f32,      // 0-6时功率
    morning: f32,    // 6-12时功率
    afternoon: f32,  // 12-18时功率
    evening: f32,    // 18-24时功率
}

impl LoadModel for DailyLoadCurve {
    fn get_instant_power(&self, time: DateTime<Utc>) -> f32 {
        let hour = time.hour();
        match hour {
            0..=5 => self.night,
            6..=11 => self.morning,
            12..=17 => self.afternoon,
            18..=23 => self.evening,
            _ => self.night,
        }
    }
}
```

### 4.4 随机游走

```rust
struct RandomWalkLoad {
    current_power: f32,
    base_power: f32,
    max_delta: f32,     // 每tick最大变化量
    rng: StdRng,
}

impl RandomWalkLoad {
    fn update(&mut self) {
        let delta = self.rng.gen_range(-self.max_delta..=self.max_delta);
        self.current_power = (self.current_power + delta)
            .max(0.0)
            .min(self.base_power * 2.0);  // 限制在0到2倍基准之间
    }
}
```

---

## 5. 事件检测

### 5.1 失压事件（电压过低）

```rust
fn detect_voltage_loss(&mut self) {
    let threshold = 0.7 * self.rated_voltage;  // 失压阈值：额定电压的70%
    let duration_threshold = Duration::seconds(3);  // 持续3秒
    
    if self.instant.voltage_a < threshold {
        if self.undervoltage_start.is_none() {
            self.undervoltage_start = Some(self.virtual_time);
        } else if self.virtual_time - self.undervoltage_start.unwrap() > duration_threshold {
            // 生成失压事件
            self.generate_event(EventKind::PhaseAVoltageLoss);
        }
    } else {
        // 电压恢复
        if let Some(start_time) = self.undervoltage_start.take() {
            self.end_event(EventKind::PhaseAVoltageLoss, start_time);
        }
    }
}
```

### 5.2 过流事件

```rust
fn detect_over_current(&mut self) {
    let threshold = 1.2 * self.rated_current;  // 过流阈值：额定电流的120%
    
    if self.instant.current_a > threshold {
        if !self.is_event_active(EventKind::PhaseAOverCurrent) {
            self.generate_event(EventKind::PhaseAOverCurrent);
        }
    } else {
        if self.is_event_active(EventKind::PhaseAOverCurrent) {
            self.end_event(EventKind::PhaseAOverCurrent, /*start_time*/);
        }
    }
}
```

---

## 6. 瞬时量计算

### 6.1 三相功率计算

```rust
struct InstantVars {
    // 电压（V）
    voltage_a: f32,
    voltage_b: f32,
    voltage_c: f32,
    
    // 电流（A）
    current_a: f32,
    current_b: f32,
    current_c: f32,
    
    // 功率因数
    power_factor_a: f32,
    power_factor_b: f32,
    power_factor_c: f32,
    
    // 有功功率（kW）
    active_power_a: f32,
    active_power_b: f32,
    active_power_c: f32,
    active_power_total: f32,
    
    // 无功功率（kvar）
    reactive_power_a: f32,
    reactive_power_b: f32,
    reactive_power_c: f32,
    reactive_power_total: f32,
    
    // 视在功率（kVA）
    apparent_power_total: f32,
    
    // 频率（Hz）
    frequency: f32,
}

impl InstantVars {
    fn calculate_from_load_model(&mut self, load_model: &dyn LoadModel, time: DateTime<Utc>) {
        // 1. 从负荷模型获取总功率
        let total_power_kw = load_model.get_instant_power(time);
        
        // 2. 假设三相平衡，平均分配到各相
        self.active_power_a = total_power_kw / 3.0;
        self.active_power_b = total_power_kw / 3.0;
        self.active_power_c = total_power_kw / 3.0;
        self.active_power_total = total_power_kw;
        
        // 3. 根据功率反推电流（P = U × I × cosφ）
        // I = P / (U × cosφ)
        self.current_a = (self.active_power_a * 1000.0) / (self.voltage_a * self.power_factor_a);
        self.current_b = (self.active_power_b * 1000.0) / (self.voltage_b * self.power_factor_b);
        self.current_c = (self.active_power_c * 1000.0) / (self.voltage_c * self.power_factor_c);
        
        // 4. 计算无功功率（Q = P × tanφ）
        let tan_phi_a = ((1.0 - self.power_factor_a.powi(2)).sqrt()) / self.power_factor_a;
        self.reactive_power_a = self.active_power_a * tan_phi_a;
        self.reactive_power_b = self.active_power_b * tan_phi_a;  // 简化：假设各相功率因数相同
        self.reactive_power_c = self.active_power_c * tan_phi_a;
        self.reactive_power_total = self.reactive_power_a + self.reactive_power_b + self.reactive_power_c;
        
        // 5. 计算视在功率（S = √(P² + Q²)）
        self.apparent_power_total = (self.active_power_total.powi(2) + self.reactive_power_total.powi(2)).sqrt();
    }
}
```

---

## 总结

以上算法覆盖了电表仿真的核心功能：

1. ✅ **脉冲累加** - 高精度电能计量
2. ✅ **滑差窗口** - 最大需量计算
3. ✅ **时段表查询** - 分时费率
4. ✅ **负荷模型** - 多种仿真模式
5. ✅ **事件检测** - 失压、过流等异常
6. ✅ **瞬时量计算** - 三相功率推导

这些算法简单实用，易于实现和调试。
