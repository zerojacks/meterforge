//! 仿真与电表参数配置面板：物理引擎参数、冻结、结算日、负荷记录、故障注入。
use super::parameter_dialogs::SyncConfirmDialog;
use gpui::*;
use gpui_component::select::{Select, SelectState};
use gpui_component::{
    button::{Button, ButtonVariants},
    form::field,
    group_box::{GroupBox, GroupBoxVariants as _},
    input::{Input, InputState},
    label::Label,
    *,
};
use meter_core::simulation::{LoadModelConfig, LoadProfile, SimulationConfig};
use meter_core::snapshot::SimulationSnapshot;

/// 单字节十进制转 BCD
fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// 确认时提交的完整电表配置（协议字段已按 BCD 编码）
#[derive(Debug, Clone)]
pub struct MeterSettings {
    pub simulation: SimulationConfig,
    pub freeze: FreezeSettings,
    pub settlement_days: [u8; 3],
    pub settlement_hours: [u8; 3],
    pub load_record: LoadRecordSettings,
}

/// 冻结配置（04-00-09-xx / 04-00-12-xx）
#[derive(Debug, Clone)]
pub struct FreezeSettings {
    /// 04-00-09-02 定时冻结模式字：0=关 1=月 2=日 3=时
    pub timed_mode: u8,
    /// 04-00-09-03 瞬时冻结数据模式字（位图）
    pub instant_mode: u8,
    /// 04-00-09-04 约定冻结数据模式字（位图）
    pub appointment_mode: u8,
    /// 04-00-09-05 整点冻结数据模式字（位图）
    pub hourly_mode: u8,
    /// 04-00-09-06 日冻结数据模式字（位图）
    pub daily_mode: u8,
    /// 04-00-12-03 日冻结时间 [hh, mm]（BCD）
    pub daily_time: [u8; 2],
    /// 04-00-12-01 整点冻结起始时间 [yy,mm,dd,hh,mi]（BCD）
    pub hourly_start: [u8; 5],
    /// 04-00-12-02 整点冻结间隔（分钟）
    pub hourly_interval_min: u8,
    /// 约定冻结触发时间 [yy,mm,dd,hh,mi]（BCD）
    pub appointment_time: [u8; 5],
}

/// 负荷记录配置（04-00-09-01 / 04-00-0A-xx）
#[derive(Debug, Clone)]
pub struct LoadRecordSettings {
    /// 04-00-09-01 负荷记录模式字（位图）
    pub mode_word: u8,
    /// 04-00-0A-01 负荷记录起始时间 [MM,DD,hh,mi]（BCD）
    pub start_time: [u8; 4],
    /// 04-00-0A-02~07 第 1~8 类负荷记录间隔（分钟，0=不记录）
    pub intervals: [u16; 8],
}

/// The only load curves supported by the simulation engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadProfileKind {
    Residential,
    Industrial,
    Commercial,
    Fixed,
}

impl LoadProfileKind {
    const ALL: [Self; 4] = [
        Self::Residential,
        Self::Industrial,
        Self::Commercial,
        Self::Fixed,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Residential => "居民负荷",
            Self::Industrial => "工业负荷",
            Self::Commercial => "商业负荷",
            Self::Fixed => "固定负荷",
        }
    }

    fn from_snapshot(value: &str) -> Self {
        match value {
            "Industrial" => Self::Industrial,
            "Commercial" => Self::Commercial,
            "Fixed" => Self::Fixed,
            _ => Self::Residential,
        }
    }

    fn from_label(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|kind| kind.label() == value)
            .unwrap_or(Self::Residential)
    }

    fn into_core(self, fixed_factor: f64) -> LoadProfile {
        match self {
            Self::Residential => LoadProfile::Residential,
            Self::Industrial => LoadProfile::Industrial,
            Self::Commercial => LoadProfile::Commercial,
            Self::Fixed => LoadProfile::Fixed(fixed_factor),
        }
    }
}

/// 定时冻结周期选项（0=关 1=月 2=日 3=时）
const TIMED_MODES: [(&str, u8); 4] = [("关闭", 0), ("按月", 1), ("按日", 2), ("按小时", 3)];

/// 可注入的故障事件（附录 A.4）
/// (标签, DI2, 是否相别事件)
const FAULT_KINDS: [(&str, u8, bool); 13] = [
    ("失压", 0x01, true),
    ("欠压", 0x02, true),
    ("过压", 0x03, true),
    ("断相", 0x04, true),
    ("全失压", 0x05, false),
    ("辅助电源失电", 0x06, false),
    ("电压逆相序", 0x07, false),
    ("电流逆相序", 0x08, false),
    ("电压不平衡", 0x09, false),
    ("电流不平衡", 0x0A, false),
    ("失流", 0x0B, true),
    ("过流", 0x0C, true),
    ("掉电", 0x0F, false),
];

/// Full-page simulation configuration form, rendered in the meter detail tab.
pub struct SimulationConfigPanel {
    // ── 物理引擎参数 ──
    profile: Entity<SelectState<Vec<String>>>,
    fixed_factor: Entity<InputState>,
    voltage: Entity<InputState>,
    current: Entity<InputState>,
    frequency: Entity<InputState>,
    power_factor: Entity<InputState>,
    meter_constant: Entity<InputState>,
    demand_period: Entity<InputState>,
    time_scale: Entity<InputState>,
    voltage_noise: Entity<InputState>,
    frequency_noise: Entity<InputState>,
    power_factor_noise: Entity<InputState>,
    power_factor_min: Entity<InputState>,
    power_factor_max: Entity<InputState>,
    phase_a: Entity<InputState>,
    phase_b: Entity<InputState>,
    phase_c: Entity<InputState>,

    // ── 冻结配置 ──
    timed_mode: Entity<SelectState<Vec<String>>>,
    instant_mode: Entity<InputState>,
    appointment_mode: Entity<InputState>,
    hourly_mode: Entity<InputState>,
    daily_mode: Entity<InputState>,
    daily_time_hh: Entity<InputState>,
    daily_time_mm: Entity<InputState>,
    hourly_start: [Entity<InputState>; 5], // yy mm dd hh mi
    hourly_interval: Entity<InputState>,
    appointment_time: [Entity<InputState>; 5], // yy mm dd hh mi

    // ── 结算日 ──
    settlement_day: [Entity<InputState>; 3],
    settlement_hour: [Entity<InputState>; 3],

    // ── 负荷记录 ──
    load_mode_word: Entity<InputState>,
    load_start: [Entity<InputState>; 4], // MM DD hh mi
    load_intervals: [Entity<InputState>; 8],

    // ── 故障注入 ──
    fault_kind: Entity<SelectState<Vec<String>>>,
    fault_phase: Entity<SelectState<Vec<String>>>,

    error: Option<SharedString>,
    on_confirm: Option<Box<dyn Fn(MeterSettings, &mut Window, &mut Context<Self>) + 'static>>,
    /// "应用到所有表"回调：参数与 on_confirm 相同，由详情视图广播给全部表。
    on_sync_all: Option<Box<dyn Fn(MeterSettings, &mut Window, &mut Context<Self>) + 'static>>,
    on_inject_fault: Option<Box<dyn Fn(u8, u8, bool, &mut Window, &mut Context<Self>) + 'static>>,
}

impl SimulationConfigPanel {
    pub fn new(value: &SimulationSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |text: String, window: &mut Window, cx: &mut Context<Self>| {
            let state = cx.new(|cx| InputState::new(window, cx));
            state.update(cx, |state, cx| state.set_value(text, window, cx));
            state
        };
        let num =
            |v: u8, window: &mut Window, cx: &mut Context<Self>| input(v.to_string(), window, cx);

        let make_select =
            |items: Vec<String>, selected: usize, window: &mut Window, cx: &mut App| {
                cx.new(|cx| SelectState::new(items, Some(IndexPath::new(selected)), window, cx))
            };

        // 负荷类型
        let selected_profile = LoadProfileKind::from_snapshot(&value.load_profile);
        let profile_options = LoadProfileKind::ALL
            .into_iter()
            .map(|kind| kind.label().to_string())
            .collect::<Vec<_>>();
        let profile = cx.new(|cx| {
            let selected_index = profile_options
                .iter()
                .position(|label| label == selected_profile.label())
                .map(IndexPath::new);
            SelectState::new(profile_options, selected_index, window, cx)
        });

        // 定时冻结模式
        let timed_index = TIMED_MODES
            .iter()
            .position(|(_, mode)| *mode == value.freeze.timed_mode)
            .unwrap_or(0);
        let timed_mode = make_select(
            TIMED_MODES
                .iter()
                .map(|(label, _)| label.to_string())
                .collect(),
            timed_index,
            window,
            cx,
        );

        // 故障类型与相别
        let fault_kind = make_select(
            FAULT_KINDS
                .iter()
                .map(|(label, _, _)| label.to_string())
                .collect(),
            0,
            window,
            cx,
        );
        let fault_phase = make_select(
            vec!["不分相".into(), "A相".into(), "B相".into(), "C相".into()],
            0,
            window,
            cx,
        );

        let fz = &value.freeze;
        let hs = fz.hourly_start;
        let at = fz.appointment_time;
        let ls = value.load_record_start_time;

        Self {
            profile,
            fixed_factor: input(
                value.fixed_load_factor.unwrap_or(0.5).to_string(),
                window,
                cx,
            ),
            voltage: input(value.rated_voltage_v.to_string(), window, cx),
            current: input(value.rated_current_a.to_string(), window, cx),
            frequency: input(value.rated_frequency_hz.to_string(), window, cx),
            power_factor: input(value.power_factor.to_string(), window, cx),
            meter_constant: input(value.meter_constant.to_string(), window, cx),
            demand_period: input(value.demand_period_minutes.to_string(), window, cx),
            time_scale: input(value.time_scale.to_string(), window, cx),
            voltage_noise: input(value.voltage_noise_v.to_string(), window, cx),
            frequency_noise: input(value.frequency_noise_hz.to_string(), window, cx),
            power_factor_noise: input(value.power_factor_noise.to_string(), window, cx),
            power_factor_min: input(value.power_factor_min.to_string(), window, cx),
            power_factor_max: input(value.power_factor_max.to_string(), window, cx),
            phase_a: input(value.phase_current_factors[0].to_string(), window, cx),
            phase_b: input(value.phase_current_factors[1].to_string(), window, cx),
            phase_c: input(value.phase_current_factors[2].to_string(), window, cx),

            timed_mode,
            instant_mode: num(fz.instant_mode, window, cx),
            appointment_mode: num(fz.appointment_mode, window, cx),
            hourly_mode: num(fz.hourly_mode, window, cx),
            daily_mode: num(fz.daily_mode, window, cx),
            daily_time_hh: num(fz.daily_time[0], window, cx),
            daily_time_mm: num(fz.daily_time[1], window, cx),
            hourly_start: [
                num(hs[0], window, cx),
                num(hs[1], window, cx),
                num(hs[2], window, cx),
                num(hs[3], window, cx),
                num(hs[4], window, cx),
            ],
            hourly_interval: num(fz.hourly_interval_min, window, cx),
            appointment_time: [
                num(at[0], window, cx),
                num(at[1], window, cx),
                num(at[2], window, cx),
                num(at[3], window, cx),
                num(at[4], window, cx),
            ],

            settlement_day: [
                num(value.settlement_days[0], window, cx),
                num(value.settlement_days[1], window, cx),
                num(value.settlement_days[2], window, cx),
            ],
            settlement_hour: [
                num(value.settlement_hours[0], window, cx),
                num(value.settlement_hours[1], window, cx),
                num(value.settlement_hours[2], window, cx),
            ],

            load_mode_word: num(value.load_record_mode_word, window, cx),
            load_start: [
                num(ls[0], window, cx),
                num(ls[1], window, cx),
                num(ls[2], window, cx),
                num(ls[3], window, cx),
            ],
            load_intervals: [
                num(value.load_record_intervals[0].min(255) as u8, window, cx),
                num(value.load_record_intervals[1].min(255) as u8, window, cx),
                num(value.load_record_intervals[2].min(255) as u8, window, cx),
                num(value.load_record_intervals[3].min(255) as u8, window, cx),
                num(value.load_record_intervals[4].min(255) as u8, window, cx),
                num(value.load_record_intervals[5].min(255) as u8, window, cx),
                num(value.load_record_intervals[6].min(255) as u8, window, cx),
                num(value.load_record_intervals[7].min(255) as u8, window, cx),
            ],

            fault_kind,
            fault_phase,

            error: None,
            on_confirm: None,
            on_sync_all: None,
            on_inject_fault: None,
        }
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(MeterSettings, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    /// "应用到所有表"回调：确认对话框通过后触发，携带与"应用配置"
    /// 相同的完整表单配置。
    pub fn on_sync_all<F>(mut self, callback: F) -> Self
    where
        F: Fn(MeterSettings, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_sync_all = Some(Box::new(callback));
        self
    }

    /// 故障注入回调（立即执行，不经确认按钮）
    pub fn on_inject_fault<F>(mut self, callback: F) -> Self
    where
        F: Fn(u8, u8, bool, &mut Window, &mut Context<Self>) + 'static,
    {
        self.on_inject_fault = Some(Box::new(callback));
        self
    }

    fn value<T: std::str::FromStr>(
        &self,
        input: &Entity<InputState>,
        cx: &App,
        name: &str,
    ) -> Result<T, String> {
        input
            .read(cx)
            .value()
            .parse()
            .map_err(|_| format!("{name} 格式不正确"))
    }

    /// 读取 u8 输入并校验范围
    fn u8_in(
        &self,
        input: &Entity<InputState>,
        cx: &App,
        name: &str,
        min: u8,
        max: u8,
    ) -> Result<u8, String> {
        let value: u8 = self.value(input, cx, name)?;
        if value < min || value > max {
            return Err(format!("{name} 必须在 {min}~{max} 之间"));
        }
        Ok(value)
    }

    /// 读取 5 个十进制分量为 BCD 时间 [yy,mm,dd,hh,mi]
    fn bcd_time5(
        &self,
        inputs: &[Entity<InputState>; 5],
        cx: &App,
        name: &str,
    ) -> Result<[u8; 5], String> {
        Ok([
            self.u8_in(&inputs[0], cx, &format!("{name} 年"), 0, 99)?,
            self.u8_in(&inputs[1], cx, &format!("{name} 月"), 1, 12)?,
            self.u8_in(&inputs[2], cx, &format!("{name} 日"), 1, 31)?,
            self.u8_in(&inputs[3], cx, &format!("{name} 时"), 0, 23)?,
            self.u8_in(&inputs[4], cx, &format!("{name} 分"), 0, 59)?,
        ])
        .map(|parts: [u8; 5]| parts.map(to_bcd))
    }

    /// 校验表单并收集完整配置；"应用配置"与"应用到所有表"共用。
    fn collect_settings(&self, cx: &App) -> Result<MeterSettings, String> {
        (|| -> Result<MeterSettings, String> {
            // ── 物理引擎 ──
            let fixed: f64 = self.value(&self.fixed_factor, cx, "固定负荷系数")?;
            let profile = self
                .profile
                .read(cx)
                .selected_value()
                .map(|value| LoadProfileKind::from_label(value).into_core(fixed))
                .ok_or_else(|| "请选择负荷类型".to_string())?;
            let simulation = SimulationConfig {
                load_model: LoadModelConfig {
                    profile,
                    voltage_noise_v: self.value(&self.voltage_noise, cx, "电压波动")?,
                    frequency_noise_hz: self.value(&self.frequency_noise, cx, "频率波动")?,
                    power_factor_noise: self.value(&self.power_factor_noise, cx, "功率因数波动")?,
                    power_factor_min: self.value(&self.power_factor_min, cx, "功率因数下限")?,
                    power_factor_max: self.value(&self.power_factor_max, cx, "功率因数上限")?,
                    phase_current_factors: [
                        self.value(&self.phase_a, cx, "A相系数")?,
                        self.value(&self.phase_b, cx, "B相系数")?,
                        self.value(&self.phase_c, cx, "C相系数")?,
                    ],
                },
                rated_voltage: self.value(&self.voltage, cx, "额定电压")?,
                rated_current: self.value(&self.current, cx, "额定电流")?,
                rated_frequency: self.value(&self.frequency, cx, "额定频率")?,
                power_factor: self.value(&self.power_factor, cx, "功率因数")?,
                meter_constant: self.value(&self.meter_constant, cx, "电表常数")?,
                demand_period_minutes: self.value(&self.demand_period, cx, "需量周期")?,
                time_scale: self.value(&self.time_scale, cx, "时间倍率")?,
            };
            simulation.validate()?;

            // ── 冻结配置 ──
            let timed_mode = self
                .timed_mode
                .read(cx)
                .selected_index(cx)
                .map(|index| TIMED_MODES[index.row].1)
                .unwrap_or(0);
            let daily_hh = self.u8_in(&self.daily_time_hh, cx, "日冻结时", 0, 23)?;
            let daily_mm = self.u8_in(&self.daily_time_mm, cx, "日冻结分", 0, 59)?;
            let freeze = FreezeSettings {
                timed_mode,
                instant_mode: self.u8_in(&self.instant_mode, cx, "瞬时冻结模式字", 0, 255)?,
                appointment_mode: self.u8_in(
                    &self.appointment_mode,
                    cx,
                    "约定冻结模式字",
                    0,
                    255,
                )?,
                hourly_mode: self.u8_in(&self.hourly_mode, cx, "整点冻结模式字", 0, 255)?,
                daily_mode: self.u8_in(&self.daily_mode, cx, "日冻结模式字", 0, 255)?,
                daily_time: [to_bcd(daily_hh), to_bcd(daily_mm)],
                hourly_start: self.bcd_time5(&self.hourly_start, cx, "整点冻结起始时间")?,
                hourly_interval_min: self.u8_in(
                    &self.hourly_interval,
                    cx,
                    "整点冻结间隔",
                    0,
                    255,
                )?,
                appointment_time: self.bcd_time5(&self.appointment_time, cx, "约定冻结时间")?,
            };

            // ── 结算日 ──
            let mut settlement_days = [0u8; 3];
            let mut settlement_hours = [0u8; 3];
            for i in 0..3 {
                settlement_days[i] = self.u8_in(
                    &self.settlement_day[i],
                    cx,
                    &format!("结算日{}", i + 1),
                    0,
                    28,
                )?;
                settlement_hours[i] = self.u8_in(
                    &self.settlement_hour[i],
                    cx,
                    &format!("结算时{}", i + 1),
                    0,
                    23,
                )?;
                if settlement_days[i] == 0 && settlement_hours[i] != 0 {
                    return Err(format!("结算日{} 未启用时小时应为 0", i + 1));
                }
            }

            // ── 负荷记录 ──
            let mut load_start_dec = [0u8; 4];
            load_start_dec[0] = self.u8_in(&self.load_start[0], cx, "负荷记录起始月", 1, 12)?;
            load_start_dec[1] = self.u8_in(&self.load_start[1], cx, "负荷记录起始日", 1, 31)?;
            load_start_dec[2] = self.u8_in(&self.load_start[2], cx, "负荷记录起始时", 0, 23)?;
            load_start_dec[3] = self.u8_in(&self.load_start[3], cx, "负荷记录起始分", 0, 59)?;
            let mut intervals = [0u16; 8];
            for (i, input) in self.load_intervals.iter().enumerate() {
                intervals[i] = self.u8_in(input, cx, &format!("第{}类间隔", i + 1), 0, 255)? as u16;
            }
            let load_record = LoadRecordSettings {
                mode_word: self.u8_in(&self.load_mode_word, cx, "负荷记录模式字", 0, 255)?,
                start_time: load_start_dec.map(to_bcd),
                intervals,
            };

            Ok(MeterSettings {
                simulation,
                freeze,
                settlement_days,
                settlement_hours,
                load_record,
            })
        })()
    }

    fn confirm(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        match self.collect_settings(cx) {
            Ok(settings) => {
                self.error = None;
                if let Some(callback) = self.on_confirm.take() {
                    callback(settings, window, cx);
                    self.on_confirm = Some(callback);
                }
            }
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
            }
        }
    }

    /// "应用到所有表"：先走与"应用配置"完全相同的校验，通过后弹确认框，
    /// 用户确认再触发 on_sync_all（由详情视图广播给全部表，含当前表）。
    fn sync_to_all(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let settings = match self.collect_settings(cx) {
            Ok(settings) => {
                self.error = None;
                settings
            }
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        // 对话框确认发生在 SyncConfirmDialog 的上下文里，需要回到本面板
        // 才能拿到 on_sync_all 回调。
        let this = cx.entity();
        let dialog_entity = cx.new(|_| {
            SyncConfirmDialog::new(
                "确认后将当前表单的模拟计算、冻结、结算日、负荷记录配置覆盖所有电表（含当前表），并写入数据库。此操作不可撤销。",
                "应用到所有表",
            )
            .on_confirm(move |window, cx| {
                this.update(cx, |panel, cx| {
                    if let Some(callback) = panel.on_sync_all.take() {
                        callback(settings.clone(), window, cx);
                        panel.on_sync_all = Some(callback);
                    }
                });
            })
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("同步模拟配置").w(px(500.)).content({
                let dialog_entity = dialog_entity.clone();
                move |content, _, _| content.child(dialog_entity.clone())
            })
        });
    }

    /// 解析当前选择的故障 (event_type, phase)
    fn selected_fault(&self, cx: &App) -> Result<(u8, u8), String> {
        let label = self
            .fault_kind
            .read(cx)
            .selected_value()
            .cloned()
            .ok_or_else(|| "请选择故障类型".to_string())?;
        let (_, event_type, per_phase) = FAULT_KINDS
            .iter()
            .find(|(name, _, _)| *name == label)
            .ok_or_else(|| "未知故障类型".to_string())?;
        let phase_label = self
            .fault_phase
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_else(|| "不分相".into());
        let phase = match phase_label.as_ref() {
            "A相" => 1,
            "B相" => 2,
            "C相" => 3,
            _ => 0,
        };
        if *per_phase && phase == 0 {
            return Err("该故障为相别事件，请选择 A/B/C 相".to_string());
        }
        if !*per_phase && phase != 0 {
            return Err("该故障为系统级事件，相别应选择“不分相”".to_string());
        }
        Ok((*event_type, phase))
    }

    fn inject_fault(
        &mut self,
        active: bool,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_fault(cx) {
            Ok((event_type, phase)) => {
                self.error = None;
                if let Some(callback) = self.on_inject_fault.take() {
                    callback(event_type, phase, active, window, cx);
                    self.on_inject_fault = Some(callback);
                }
            }
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
            }
        }
    }
}

fn section_box(title: &str, content: impl IntoElement) -> AnyElement {
    GroupBox::new()
        .outline()
        .title(title.to_string())
        .child(content)
        .into_any_element()
}

impl Render for SimulationConfigPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let error = self.error.clone();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(Label::new("模拟计算与电表参数配置").text_2xl().font_semibold())
            .child(
                Label::new("配置物理引擎、冻结、结算日、负荷记录参数，确认后由后端统一校验并原子应用；故障注入立即生效。")
                    .text_xs()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_4()
                    // ── 负荷与额定值 ──
                    .child(section_box(
                        "负荷与额定值",
                        div()
                            .grid()
                            .grid_cols(3)
                            .gap_4()
                            .child(
                                field()
                                    .label("负荷类型")
                                    .child(Select::new(&self.profile).w_full()),
                            )
                            .child(
                                field()
                                    .label("固定负荷系数（仅固定负荷）")
                                    .child(Input::new(&self.fixed_factor)),
                            )
                            .child(field().label("额定电压 (V)").child(Input::new(&self.voltage)))
                            .child(field().label("额定电流 (A)").child(Input::new(&self.current)))
                            .child(
                                field()
                                    .label("额定频率 (Hz)")
                                    .child(Input::new(&self.frequency)),
                            )
                            .child(
                                field()
                                    .label("初始功率因数")
                                    .child(Input::new(&self.power_factor)),
                            ),
                    ))
                    // ── 计量与需量 ──
                    .child(section_box(
                        "计量与需量",
                        div()
                            .grid()
                            .grid_cols(3)
                            .gap_4()
                            .child(
                                field()
                                    .label("电表常数 (imp/kWh)")
                                    .child(Input::new(&self.meter_constant)),
                            )
                            .child(
                                field()
                                    .label("需量周期 (min)")
                                    .child(Input::new(&self.demand_period)),
                            )
                            .child(field().label("时间倍率").child(Input::new(&self.time_scale)))
                            .child(
                                field()
                                    .label("功率因数下限")
                                    .child(Input::new(&self.power_factor_min)),
                            )
                            .child(
                                field()
                                    .label("功率因数上限")
                                    .child(Input::new(&self.power_factor_max)),
                            ),
                    ))
                    // ── 波动与三相修正 ──
                    .child(section_box(
                        "波动与三相修正",
                        div()
                            .grid()
                            .grid_cols(3)
                            .gap_4()
                            .child(
                                field()
                                    .label("电压波动 (V)")
                                    .child(Input::new(&self.voltage_noise)),
                            )
                            .child(
                                field()
                                    .label("频率波动 (Hz)")
                                    .child(Input::new(&self.frequency_noise)),
                            )
                            .child(
                                field()
                                    .label("功率因数波动")
                                    .child(Input::new(&self.power_factor_noise)),
                            )
                            .child(
                                field()
                                    .label("A相电流系数")
                                    .child(Input::new(&self.phase_a)),
                            )
                            .child(
                                field()
                                    .label("B相电流系数")
                                    .child(Input::new(&self.phase_b)),
                            )
                            .child(
                                field()
                                    .label("C相电流系数")
                                    .child(Input::new(&self.phase_c)),
                            ),
                    ))
                    // ── 冻结配置 ──
                    .child(section_box(
                        "冻结配置 (04-00-09 / 04-00-12)",
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(3)
                                    .gap_4()
                                    .child(
                                        field()
                                            .label("定时冻结周期")
                                            .child(Select::new(&self.timed_mode).w_full()),
                                    )
                                    .child(
                                        field()
                                            .label("日冻结时间 时 (0~23)")
                                            .child(Input::new(&self.daily_time_hh)),
                                    )
                                    .child(
                                        field()
                                            .label("日冻结时间 分 (0~59)")
                                            .child(Input::new(&self.daily_time_mm)),
                                    ),
                            )
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(3)
                                    .gap_4()
                                    .child(
                                        field()
                                            .label("瞬时冻结模式字 (位图)")
                                            .child(Input::new(&self.instant_mode)),
                                    )
                                    .child(
                                        field()
                                            .label("约定冻结模式字 (位图)")
                                            .child(Input::new(&self.appointment_mode)),
                                    )
                                    .child(
                                        field()
                                            .label("日冻结模式字 (位图)")
                                            .child(Input::new(&self.daily_mode)),
                                    )
                                    .child(
                                        field()
                                            .label("整点冻结模式字 (位图)")
                                            .child(Input::new(&self.hourly_mode)),
                                    )
                                    .child(
                                        field()
                                            .label("整点冻结间隔 (分钟)")
                                            .child(Input::new(&self.hourly_interval)),
                                    ),
                            )
                            .child(
                                field().label("整点冻结起始时间（年 月 日 时 分）").child(
                                    h_flex().gap_2().children(
                                        self.hourly_start
                                            .iter()
                                            .map(|state| Input::new(state).w(px(64.))),
                                    ),
                                ),
                            )
                            .child(
                                field().label("约定冻结时间（年 月 日 时 分）").child(
                                    h_flex().gap_2().children(
                                        self.appointment_time
                                            .iter()
                                            .map(|state| Input::new(state).w(px(64.))),
                                    ),
                                ),
                            ),
                    ))
                    // ── 结算日 ──
                    .child(section_box(
                        "结算日 (04-00-0B，DDhh，DD=0 不启用)",
                        div().flex().flex_col().gap_3().children((0..3usize).map(|i| {
                            field().label(format!("结算日 {}", i + 1)).child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(Input::new(&self.settlement_day[i]).w(px(80.)))
                                    .child(Label::new("日"))
                                    .child(Input::new(&self.settlement_hour[i]).w(px(80.)))
                                    .child(Label::new("时")),
                            )
                        })),
                    ))
                    // ── 负荷记录 ──
                    .child(section_box(
                        "负荷记录 (04-00-09-01 / 04-00-0A)",
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(3)
                                    .gap_4()
                                    .child(
                                        field()
                                            .label("负荷记录模式字 (位图)")
                                            .child(Input::new(&self.load_mode_word)),
                                    ),
                            )
                            .child(
                                field().label("负荷记录起始时间（月 日 时 分）").child(
                                    h_flex().gap_2().children(
                                        self.load_start
                                            .iter()
                                            .map(|state| Input::new(state).w(px(64.))),
                                    ),
                                ),
                            )
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(3)
                                    .gap_4()
                                    .child(
                                        field()
                                            .label("第1类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[0])),
                                    )
                                    .child(
                                        field()
                                            .label("第2类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[1])),
                                    )
                                    .child(
                                        field()
                                            .label("第3类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[2])),
                                    )
                                    .child(
                                        field()
                                            .label("第4类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[3])),
                                    )
                                    .child(
                                        field()
                                            .label("第5类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[4])),
                                    )
                                    .child(
                                        field()
                                            .label("第6类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[5])),
                                    )
                                    .child(
                                        field()
                                            .label("第7类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[6])),
                                    )
                                    .child(
                                        field()
                                            .label("第8类间隔 (min)")
                                            .child(Input::new(&self.load_intervals[7])),
                                    ),
                            ),
                    ))
                    // ── 故障注入 ──
                    .child(section_box(
                        "故障注入（附录 A.4 事件生成，立即生效）",
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(3)
                                    .gap_4()
                                    .child(
                                        field()
                                            .label("故障类型")
                                            .child(Select::new(&self.fault_kind).w_full()),
                                    )
                                    .child(
                                        field()
                                            .label("相别")
                                            .child(Select::new(&self.fault_phase).w_full()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("inject-fault-on")
                                            .label("注入故障")
                                            .danger()
                                            .on_click(cx.listener(move |this, e, w, c| {
                                                this.inject_fault(true, e, w, c)
                                            })),
                                    )
                                    .child(
                                        Button::new("inject-fault-off")
                                            .label("解除故障")
                                            .on_click(cx.listener(move |this, e, w, c| {
                                                this.inject_fault(false, e, w, c)
                                            })),
                                    ),
                            )
                            .child(
                                Label::new("注入后物理引擎持续生成对应事件记录；解除后回到阈值自动判定，事件自然结束并回填电能增量。相别事件需选择 A/B/C 相，系统级事件选择“不分相”。")
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            ),
                    )),
            )
            .children(error.map(|message| {
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme.danger.opacity(0.1))
                    .child(Label::new(message).text_sm().text_color(theme.danger))
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("apply-simulation-config-to-all")
                            .label("应用到所有表")
                            .on_click(cx.listener(Self::sync_to_all)),
                    )
                    .child(
                        Button::new("apply-simulation-config")
                            .label("应用配置")
                            .on_click(cx.listener(Self::confirm)),
                    ),
            )
    }
}
