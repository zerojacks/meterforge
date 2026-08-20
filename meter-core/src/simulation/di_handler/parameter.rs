// 表A.5 参变量数据标识读取（DI3=04）与厂商自定义信息（DI3=C0）
//
// 04-00-04-xx 采用协议标准铭牌布局（通信地址/表号/资产码/ASCII 参数/常数等）；
// 通信波特率读 04-00-07-xx 通信速率特征字。

use super::encoding::*;
use super::MeterState;
use crate::simulation::di_handler::DIHandler;
use chrono::{Datelike, Timelike};

impl DIHandler {
    /// 参变量与厂商信息读取入口
    ///
    /// 返回 None 表示 DI 不属于参变量区间，交回通用匹配。
    pub(super) fn handle_parameter_read(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Option<Result<Vec<u8>, String>> {
        let result = match di {
            // ── 厂商信息 (C0-xx-xx-xx) ──
            [0x00, 0x00, 0x31, 0xC0] => Ok(vec![0x07, 0x20]), // 协议版本 2007
            [0x00, 0x00, 0x32, 0xC0] => {
                // 厂商代码（逆序返回）
                let mut code = b"KIRO".to_vec();
                code.reverse();
                Ok(code)
            }
            [0x00, 0x00, 0x33, 0xC0] => {
                // 电表型号（逆序返回）
                let mut model = b"DLT645VM".to_vec();
                model.reverse();
                Ok(model)
            }
            [0x00, 0x00, 0x34, 0xC0] => {
                let now = state.virtual_time;
                Ok(vec![
                    to_bcd((now.year() % 100) as u8),
                    to_bcd(now.month() as u8),
                    to_bcd(now.day() as u8),
                ])
            }
            [0x00, 0x00, 0x35, 0xC0] => {
                // 版本号（逆序返回）
                let mut version = b"V1.0.0".to_vec();
                version.reverse();
                Ok(version)
            }

            // ── 日期时间与切换时间 (04-00-01-xx) ──
            [0x01, 0x01, 0x00, 0x04] => {
                // 日期及星期 YYMMDDww
                let now = state.virtual_time;
                Ok(vec![
                    to_bcd(now.weekday().num_days_from_sunday() as u8),
                    to_bcd(now.day() as u8),
                    to_bcd(now.month() as u8),
                    to_bcd((now.year() % 100) as u8),
                ])
            }
            [0x02, 0x01, 0x00, 0x04] => {
                // 时间 hhmmss
                let now = state.virtual_time;
                Ok(vec![
                    to_bcd(now.second() as u8),
                    to_bcd(now.minute() as u8),
                    to_bcd(now.hour() as u8),
                ])
            }
            [0x03, 0x01, 0x00, 0x04] => Ok(vec![to_bcd(state.demand_period_minutes as u8)]),
            [0x04, 0x01, 0x00, 0x04] => Ok(vec![to_bcd(state.sliding_window_minutes as u8)]),
            [0x05, 0x01, 0x00, 0x04] => {
                // 校表脉冲宽度（毫秒，2字节BCD）
                Ok(encode_bcd(state.calibration_pulse_width_ms as f64, 2, 0))
            }
            [0x06, 0x01, 0x00, 0x04] => Ok(state.timezone_switch_time.to_vec()),
            [0x07, 0x01, 0x00, 0x04] => Ok(state.daytable_switch_time.to_vec()),
            [0x08, 0x01, 0x00, 0x04] => Ok(state.price_switch_time.to_vec()),
            [0x09, 0x01, 0x00, 0x04] => Ok(state.ladder_switch_time.to_vec()),

            // ── 费率时段参数 (04-00-02-xx) ──
            [0x01, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.num_time_zones)]),
            [0x02, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.num_day_tables)]),
            [0x03, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.num_time_slots)]),
            [0x04, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.num_rates)]),
            [0x05, 0x02, 0x00, 0x04] => {
                let n = state.num_public_holidays;
                Ok(vec![to_bcd((n % 100) as u8), to_bcd((n / 100) as u8)])
            }
            [0x06, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.harmonic_analysis_orders)]),
            [0x07, 0x02, 0x00, 0x04] => Ok(vec![to_bcd(state.num_ladders)]),

            // ── 显示与互感器参数 (04-00-03-xx) ──
            [0x01, 0x03, 0x00, 0x04] => Ok(vec![to_bcd(state.display_config.cycle_screen_count)]),
            [0x02, 0x03, 0x00, 0x04] => {
                Ok(vec![to_bcd(state.display_config.screen_period_seconds)])
            }
            [0x03, 0x03, 0x00, 0x04] => Ok(vec![to_bcd(state.display_config.energy_decimals)]),
            [0x04, 0x03, 0x00, 0x04] => Ok(vec![to_bcd(state.display_config.demand_decimals)]),
            [0x05, 0x03, 0x00, 0x04] => Ok(vec![to_bcd(state.display_config.key_screen_count)]),
            [0x06, 0x03, 0x00, 0x04] => Ok(encode_bcd(state.display_config.ct_ratio as f64, 3, 0)),
            [0x07, 0x03, 0x00, 0x04] => Ok(encode_bcd(state.display_config.pt_ratio as f64, 3, 0)),

            // ── 铭牌与厂家参数 (04-00-04-xx，协议标准布局) ──
            [0x01, 0x04, 0x00, 0x04] => Ok(state.address.to_vec()), // 通信地址（BCD，不需要逆序）
            [0x02, 0x04, 0x00, 0x04] => Ok(state.nameplate.meter_no.to_vec()), // 表号（BCD，不需要逆序）
            [0x03, 0x04, 0x00, 0x04] => Ok(state.nameplate.asset_code.to_vec()), // 资产管理编码（BCD，不需要逆序）
            [0x04, 0x04, 0x00, 0x04] => {
                // 额定电压（ASCII，逆序返回）
                let mut data = state.nameplate.rated_voltage_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x05, 0x04, 0x00, 0x04] => {
                // 额定电流（ASCII，逆序返回）
                let mut data = state.nameplate.rated_current_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x06, 0x04, 0x00, 0x04] => {
                // 最大电流（ASCII，逆序返回）
                let mut data = state.nameplate.max_current_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x07, 0x04, 0x00, 0x04] => {
                // 有功准确度等级（ASCII，逆序返回）
                let mut data = state.nameplate.active_accuracy.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x08, 0x04, 0x00, 0x04] => {
                // 无功准确度等级（ASCII，逆序返回）
                let mut data = state.nameplate.reactive_accuracy.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x09, 0x04, 0x00, 0x04] => {
                Ok(encode_bcd(state.meter_constant as f64, 3, 0)) // 有功常数（BCD，不需要逆序）
            }
            [0x0A, 0x04, 0x00, 0x04] => Ok(encode_bcd(
                state.nameplate.reactive_meter_constant as f64,
                3,
                0,
            )), // 无功常数（BCD，不需要逆序）
            [0x0B, 0x04, 0x00, 0x04] => {
                // 电表型号（ASCII，逆序返回）
                let mut data = state.nameplate.meter_model_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x0C, 0x04, 0x00, 0x04] => {
                // 生产日期（ASCII，逆序返回）
                let mut data = state.nameplate.production_date_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x0D, 0x04, 0x00, 0x04] => {
                // 协议版本号（ASCII，逆序返回）
                let mut data = state.nameplate.protocol_version_ascii.to_vec();
                data.reverse();
                Ok(data)
            }
            [0x0E, 0x04, 0x00, 0x04] => Ok(state.nameplate.customer_no.to_vec()), // 客户编号（BCD，不需要逆序）

            // ── 派生状态字 (04-00-05-xx) 与数据块 ──
            [0x01, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_1.to_le_bytes().to_vec())
            }
            [0x02, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_2.to_le_bytes().to_vec())
            }
            [0x03, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_3.to_le_bytes().to_vec())
            }
            [0x04, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_4.to_le_bytes().to_vec())
            }
            [0x05, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_5.to_le_bytes().to_vec())
            }
            [0x06, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_6.to_le_bytes().to_vec())
            }
            [0x07, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_word_7.to_le_bytes().to_vec())
            }
            [0x08, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.status_key.to_le_bytes().to_vec())
            }
            [0x0E, 0x05, 0x00, 0x04] => {
                Ok(state.derived_status.meter_status.to_le_bytes().to_vec())
            }
            [0xFF, 0x05, 0x00, 0x04] => {
                let mut data = state.derived_status.status_word_1.to_le_bytes().to_vec();
                data.extend(state.derived_status.status_word_2.to_le_bytes());
                data.extend(state.derived_status.status_word_3.to_le_bytes());
                data.extend(state.derived_status.status_word_4.to_le_bytes());
                data.extend(state.derived_status.status_word_5.to_le_bytes());
                data.extend(state.derived_status.status_word_6.to_le_bytes());
                data.extend(state.derived_status.status_word_7.to_le_bytes());
                data.extend(state.derived_status.status_key.to_le_bytes());
                Ok(data)
            }

            // ── 组合方式特征字 (04-00-06-xx) ──
            [0x01, 0x06, 0x00, 0x04] => Ok(vec![state.active_combination_word]),
            [0x02, 0x06, 0x00, 0x04] => Ok(vec![state.reactive_combination_1]),
            [0x03, 0x06, 0x00, 0x04] => Ok(vec![state.reactive_combination_2]),

            // ── 通信速率特征字 (04-00-07-xx)，01~05 对应各通信口 ──
            [idx, 0x07, 0x00, 0x04] if (0x01..=0x05).contains(&idx) => {
                Ok(vec![state.comm_speed_feature[idx as usize - 1]])
            }

            // ── 周休日 (04-00-08-xx) ──
            [0x01, 0x08, 0x00, 0x04] => Ok(vec![state.weekly_rest_day_word]),
            [0x02, 0x08, 0x00, 0x04] => Ok(vec![to_bcd(state.rest_day_table_no)]),

            // ── 记录与冻结模式字 (04-00-09-xx) ──
            [0x01, 0x09, 0x00, 0x04] => Ok(vec![state.load_record_config.mode_word]),
            [0x02, 0x09, 0x00, 0x04] => Ok(vec![state.freeze_config.timed_freeze_mode]),
            [0x03, 0x09, 0x00, 0x04] => Ok(vec![state.freeze_config.instant_freeze_mode]),
            [0x04, 0x09, 0x00, 0x04] => Ok(vec![state.freeze_config.appointment_freeze_mode]),
            [0x05, 0x09, 0x00, 0x04] => Ok(vec![state.hourly_freeze_mode]),
            [0x06, 0x09, 0x00, 0x04] => Ok(vec![state.daily_freeze_mode]),

            // ── 负荷记录 (04-00-0A-xx) ──
            [0x01, 0x0A, 0x00, 0x04] => {
                let mut data = state.load_record_start_time.to_vec();
                data.reverse();
                Ok(data.to_vec())
            },
            [idx, 0x0A, 0x00, 0x04] if (0x02..=0x08).contains(&idx) => {
                let interval = state.load_record_config.intervals[idx as usize - 2];
                Ok(vec![
                    to_bcd((interval % 100) as u8),
                    to_bcd((interval / 100) as u8),
                ])
            }

            // ── 结算日 (04-00-0B-xx)，DDhh ──
            [idx, 0x0B, 0x00, 0x04] if (0x01..=0x03).contains(&idx) => {
                let day = state.settlement_days[idx as usize - 1];
                let hour = state.settlement_hours[idx as usize - 1];
                Ok(vec![to_bcd(day), to_bcd(hour)])
            }

            // ── 相网络系数 (04-00-0D-xx)，A/B/C 各4项：电导/电纳/电阻/电抗 ──
            [idx, 0x0D, 0x00, 0x04] if (0x01..=0x0C).contains(&idx) => {
                let phase = (idx - 1) / 4;
                let item = ((idx - 1) % 4) as usize;
                Ok(encode_bcd(
                    state.limits.network_coefficients[phase as usize][item],
                    2,
                    3,
                ))
            }

            // ── 功率与电压限值 (04-00-0E-xx) ──
            [0x01, 0x0E, 0x00, 0x04] => {
                Ok(encode_bcd_power(state.limits.forward_active_power_limit))
            }
            [0x02, 0x0E, 0x00, 0x04] => {
                Ok(encode_bcd_power(state.limits.reverse_active_power_limit))
            }
            [0x03, 0x0E, 0x00, 0x04] => Ok(encode_bcd(state.limits.voltage_upper, 2, 1)),
            [0x04, 0x0E, 0x00, 0x04] => Ok(encode_bcd(state.limits.voltage_lower, 2, 1)),

            // ── 电量限值 (04-00-0F-xx) ──
            [0x01, 0x0F, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.alarm_energy_1)),
            [0x02, 0x0F, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.alarm_energy_2)),
            [0x03, 0x0F, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.hoard_energy)),
            [0x04, 0x0F, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.overdraft_energy)),

            // ── 金额限值 (04-00-10-xx) ──
            [0x01, 0x10, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.alarm_amount_1)),
            [0x02, 0x10, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.alarm_amount_2)),
            [0x03, 0x10, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.overdraft_amount)),
            [0x04, 0x10, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.hoard_amount)),
            [0x05, 0x10, 0x00, 0x04] => Ok(encode_bcd_energy(state.limits.close_allow_amount)),

            // ── 运行特征字 (04-00-11-xx) ──
            [0x01, 0x11, 0x00, 0x04] => Ok(state.operation_feature_word_1.to_le_bytes().to_vec()),

            
            // ── 运行特征字 (04-00-11-xx) ──
            [0x04, 0x11, 0x00, 0x04] => Ok(state.active_report_mode.to_vec()),

            // ── 整点/日冻结时间 (04-00-12-xx) ──
            [0x01, 0x12, 0x00, 0x04] => Ok(state.hourly_freeze_start.to_vec()),
            [0x02, 0x12, 0x00, 0x04] => Ok(vec![to_bcd(state.hourly_freeze_interval_min)]),
            [0x03, 0x12, 0x00, 0x04] => Ok(state.daily_freeze_time.to_vec()),

            // ── 无线通信指示 (04-00-13-01) ──
            [0x01, 0x13, 0x00, 0x04] => Ok(vec![state.wireless_signal]),

            _ => return None,
        };
        Some(result)
    }
}
