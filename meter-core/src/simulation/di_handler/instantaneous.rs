// 表A.3 瞬时变量数据标识读取（DI3=02）
//
// - 02-01 电压（分相 + 数据块）
// - 02-02 电流（分相 + 数据块）
// - 02-03 瞬时有功功率（总/分相 + 数据块）
// - 02-04 瞬时无功功率（总/分相 + 数据块）
// - 02-05 瞬时视在功率（总/分相 + 数据块，分相由有功/无功合成）
// - 02-06 功率因数（总/分相 + 数据块，分相简化使用总值）
// - 02-07 相角（由功率因数反推 U/I 夹角）
// - 02-08/09 电压/电流波形失真度（分相 + 数据块）
// - 02-0A/0B 电压/电流谐波含量（1~21次 + 数据块）
// - 02-80 零线电流、电网频率

use super::encoding::*;
use super::MeterState;
use crate::simulation::di_handler::DIHandler;

impl DIHandler {
    /// 瞬时变量读取入口
    ///
    /// 返回 None 表示 DI 不属于瞬时变量区间，交回通用匹配。
    pub(super) fn handle_instantaneous_read(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Option<Result<Vec<u8>, String>> {
        if di[3] != 0x02 {
            return None;
        }
        let result = match di {
            // ── 电压 (02-01-xx-00) ──
            [0x00, 0x01, 0x01, 0x02] => Ok(encode_bcd_voltage(state.voltage_a)),
            [0x00, 0x02, 0x01, 0x02] => Ok(encode_bcd_voltage(state.voltage_b)),
            [0x00, 0x03, 0x01, 0x02] => Ok(encode_bcd_voltage(state.voltage_c)),
            [0x00, 0xFF, 0x01, 0x02] => {
                let mut data = encode_bcd_voltage(state.voltage_a);
                data.extend(encode_bcd_voltage(state.voltage_b));
                data.extend(encode_bcd_voltage(state.voltage_c));
                Ok(data)
            }

            // ── 电流 (02-02-xx-00) ──
            [0x00, 0x01, 0x02, 0x02] => Ok(encode_bcd_current(state.current_a)),
            [0x00, 0x02, 0x02, 0x02] => Ok(encode_bcd_current(state.current_b)),
            [0x00, 0x03, 0x02, 0x02] => Ok(encode_bcd_current(state.current_c)),
            [0x00, 0xFF, 0x02, 0x02] => {
                let mut data = encode_bcd_current(state.current_a);
                data.extend(encode_bcd_current(state.current_b));
                data.extend(encode_bcd_current(state.current_c));
                Ok(data)
            }

            // ── 瞬时有功功率 (02-03-xx-00) ──
            [0x00, 0x00, 0x03, 0x02] => Ok(encode_bcd_power(state.active_power_total)),
            [0x00, 0x01, 0x03, 0x02] => Ok(encode_bcd_power(state.active_power_a)),
            [0x00, 0x02, 0x03, 0x02] => Ok(encode_bcd_power(state.active_power_b)),
            [0x00, 0x03, 0x03, 0x02] => Ok(encode_bcd_power(state.active_power_c)),
            [0x00, 0xFF, 0x03, 0x02] => {
                let mut data = encode_bcd_power(state.active_power_total);
                data.extend(encode_bcd_power(state.active_power_a));
                data.extend(encode_bcd_power(state.active_power_b));
                data.extend(encode_bcd_power(state.active_power_c));
                Ok(data)
            }

            // ── 瞬时无功功率 (02-04-xx-00) ──
            [0x00, 0x00, 0x04, 0x02] => Ok(encode_bcd_power(state.reactive_power_total)),
            [0x00, 0x01, 0x04, 0x02] => Ok(encode_bcd_power(state.reactive_power_a)),
            [0x00, 0x02, 0x04, 0x02] => Ok(encode_bcd_power(state.reactive_power_b)),
            [0x00, 0x03, 0x04, 0x02] => Ok(encode_bcd_power(state.reactive_power_c)),
            [0x00, 0xFF, 0x04, 0x02] => {
                let mut data = encode_bcd_power(state.reactive_power_total);
                data.extend(encode_bcd_power(state.reactive_power_a));
                data.extend(encode_bcd_power(state.reactive_power_b));
                data.extend(encode_bcd_power(state.reactive_power_c));
                Ok(data)
            }

            // ── 瞬时视在功率 (02-05-xx-00)，分相值由有功/无功合成 ──
            [0x00, 0x00, 0x05, 0x02] => Ok(encode_bcd_power(state.apparent_power_total)),
            [0x00, 0x01, 0x05, 0x02] => Ok(encode_bcd_power(
                state.active_power_a.hypot(state.reactive_power_a),
            )),
            [0x00, 0x02, 0x05, 0x02] => Ok(encode_bcd_power(
                state.active_power_b.hypot(state.reactive_power_b),
            )),
            [0x00, 0x03, 0x05, 0x02] => Ok(encode_bcd_power(
                state.active_power_c.hypot(state.reactive_power_c),
            )),
            [0x00, 0xFF, 0x05, 0x02] => {
                let mut data = encode_bcd_power(state.apparent_power_total);
                data.extend(encode_bcd_power(
                    state.active_power_a.hypot(state.reactive_power_a),
                ));
                data.extend(encode_bcd_power(
                    state.active_power_b.hypot(state.reactive_power_b),
                ));
                data.extend(encode_bcd_power(
                    state.active_power_c.hypot(state.reactive_power_c),
                ));
                Ok(data)
            }

            // ── 功率因数 (02-06-xx-00)，分相简化使用总值 ──
            [0x00, 0x00, 0x06, 0x02] => Ok(encode_bcd_power_factor(state.power_factor)),
            [0x00, 0x01, 0x06, 0x02] | [0x00, 0x02, 0x06, 0x02] | [0x00, 0x03, 0x06, 0x02] => {
                Ok(encode_bcd_power_factor(state.power_factor))
            }
            [0x00, 0xFF, 0x06, 0x02] => {
                let mut data = encode_bcd_power_factor(state.power_factor);
                for _ in 0..3 {
                    data.extend(encode_bcd_power_factor(state.power_factor));
                }
                Ok(data)
            }

            // ── 相角 (02-07-xx-00)，由功率因数反推 U/I 夹角 ──
            [di0, 0x01, 0x07, 0x02] | [di0, 0x02, 0x07, 0x02] | [di0, 0x03, 0x07, 0x02]
                if matches!(di0, 0x00 | 0x01 | 0x02 | 0x03) =>
            {
                let _ = di0;
                Ok(encode_bcd(state.phase_angle_degrees(), 2, 1))
            }
            [0x00, 0xFF, 0x07, 0x02] => {
                let mut data = encode_bcd(state.phase_angle_degrees(), 2, 1);
                for _ in 0..3 {
                    data.extend(encode_bcd(state.phase_angle_degrees(), 2, 1));
                }
                Ok(data)
            }

            // ── 电压波形失真度 (02-08-xx-00) ──
            [0x00, phase, 0x08, 0x02] if (0x01..=0x03).contains(&phase) => {
                let idx = phase as usize - 1;
                Ok(encode_bcd(state.voltage_thd[idx], 2, 2))
            }
            [0x00, 0xFF, 0x08, 0x02] => {
                let mut data = Vec::new();
                for idx in 0..3 {
                    data.extend(encode_bcd(state.voltage_thd[idx], 2, 2));
                }
                Ok(data)
            }

            // ── 电流波形失真度 (02-09-xx-00) ──
            [0x00, phase, 0x09, 0x02] if (0x01..=0x03).contains(&phase) => {
                let idx = phase as usize - 1;
                Ok(encode_bcd(state.current_thd[idx], 2, 2))
            }
            [0x00, 0xFF, 0x09, 0x02] => {
                let mut data = Vec::new();
                for idx in 0..3 {
                    data.extend(encode_bcd(state.current_thd[idx], 2, 2));
                }
                Ok(data)
            }

            // ── 电压谐波含量 (02-0A-相-次)，DI1=相，DI0=1~21次(BCD 01~15) 或 FF 块 ──
            [harmonic, phase, 0x0A, 0x02] if (0x01..=0x03).contains(&phase) => {
                Self::read_harmonic(state.voltage_harmonics, phase, harmonic)
            }
            // ── 电流谐波含量 (02-0B-相-次) ──
            [harmonic, phase, 0x0B, 0x02] if (0x01..=0x03).contains(&phase) => {
                Self::read_harmonic(state.current_harmonics, phase, harmonic)
            }

            // ── 零线电流 (02-80-00-01) ──
            [0x01, 0x00, 0x80, 0x02] => Ok(encode_bcd_current(state.neutral_current)),

            // ── 电网频率 (02-80-00-02) ──
            [0x02, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.frequency, 2, 2)),

            // ── 其他实时变量 (02-80-00-xx) ──
            // 一分钟有功总平均功率
            [0x03, 0x00, 0x80, 0x02] => Ok(encode_bcd_power(state.active_power_total)),
            // 当前有功需量
            [0x04, 0x00, 0x80, 0x02] => Ok(encode_bcd_power(state.max_demand)),
            // 当前无功需量（简化：复用有功需量）
            [0x05, 0x00, 0x80, 0x02] => Ok(encode_bcd_power(state.max_demand)),
            // 当前视在功率
            [0x06, 0x00, 0x80, 0x02] => Ok(encode_bcd_power(state.apparent_power_total)),
            // 表内温度
            [0x07, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.meter_temperature, 2, 1)),
            // 时钟电池电压
            [0x08, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.clock_battery_voltage, 2, 2)),
            // 停电抄表电池电压
            [0x09, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.outage_battery_voltage, 2, 2)),
            // 内部电池工作时间（分钟）
            [0x0A, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.battery_work_minutes as f64, 4, 0)),
            // 当前阶梯电价（元/kWh）
            [0x0B, 0x00, 0x80, 0x02] => Ok(encode_bcd(state.current_step_price, 2, 4)),

            _ => return None,
        };
        Some(result)
    }

    /// 谐波含量读取：harmonic=0x01~0x15(BCD 1~21次) 单项，0xFF 数据块
    fn read_harmonic(
        harmonics: [[f64; 22]; 3],
        phase: u8,
        harmonic: u8,
    ) -> Result<Vec<u8>, String> {
        let phase_idx = phase as usize - 1;
        if harmonic == 0xFF {
            let mut data = Vec::new();
            for order in 1..=21 {
                data.extend(encode_bcd(harmonics[phase_idx][order], 2, 2));
            }
            Ok(data)
        } else {
            let order = bcd_to_decimal(harmonic) as usize;
            if (1..=21).contains(&order) {
                Ok(encode_bcd(harmonics[phase_idx][order], 2, 2))
            } else {
                Err(format!("谐波次数超出范围: {order} (支持 1~21 次)"))
            }
        }
    }
}
