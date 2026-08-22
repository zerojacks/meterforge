// DI 处理器 - 纯粹的数据映射层，负责 DI 码到数据项的映射和 BCD 编码
//
// 设计原则（按设计方案4.7节）：
// - DIHandler **不持有状态**，只负责数据映射和编码
// - 所有数据从 MeterState 读取
// - 不依赖 PhysicsEngine（PhysicsEngine 只负责更新 MeterState）
//
// 模块划分（对应 DL/T645-2007 附录A 数据标识表）：
// - energy:        表A.1 电能量（含结算日电能、分相电能）
// - demand:        表A.2 最大需量及发生时间
// - instantaneous: 表A.3 瞬时变量（电压/电流/功率/功率因数/相角/频率/谐波等）
// - event:         表A.4 事件记录
// - parameter:     表A.5 参变量
// - freeze:        表A.6 冻结数据
// - load_profile:  表A.7 负荷记录
// - encoding:      BCD 编码辅助函数

mod demand;
mod encoding;
mod energy;
mod event;
mod freeze;
mod instantaneous;
mod load_profile;
mod parameter;

use super::state::{EnergyType, MeterState};

/// DI 数据处理器 - 无状态的纯映射层
///
/// 职责：
/// 1. 将 DI 码映射到 MeterState 中的具体数据项
/// 2. 进行 BCD 编码和解码
/// 3. 不持有任何状态，所有状态通过参数传入
/// 4. 支持从数据库查询历史冻结数据（DI0 > 0C）
/// 5. 支持写操作：解码数据并更新MeterState
pub struct DIHandler;

impl DIHandler {
    /// 创建 DI 处理器
    pub fn new() -> Self {
        Self
    }

    /// 异步读数据（支持数据库查询）
    ///
    /// 统一入口，处理所有DI的读取请求：
    /// - DI3=06 负荷记录：查询数据库
    /// - DI3=05 冻结数据：根据DI0判断内存/数据库
    /// - 其他：内存同步读取
    ///
    /// 参数：
    /// - data: 完整的DATA字段（DI + 额外参数）
    /// - state: 电表状态
    /// - address: 电表地址字符串
    /// - db_pool: 数据库连接池（可选）
    pub async fn handle_read_async(
        &self,
        data: &[u8],
        state: &MeterState,
        address: &str,
        db_pool: Option<&sqlx::SqlitePool>,
    ) -> Result<Vec<u8>, String> {
        if data.len() < 4 {
            return Err("数据长度不足（至少需要4字节DI）".to_string());
        }

        let di = [data[0], data[1], data[2], data[3]];
        let rest = &data[4..];

        // 根据DI3分发
        match di[3] {
            0x06 => {
                // 负荷记录（DI3=06）
                let pool = db_pool.ok_or_else(|| "负荷记录查询需要数据库支持".to_string())?;
                self.handle_load_profile_with_params(di, state, address, pool, rest)
                    .await
            }
            0x05 => {
                // 冻结数据（DI3=05）- 统一走异步版本（支持从数据库加载）
                let pool = db_pool
                    .ok_or_else(|| "冻结数据查询需要数据库支持（从数据库加载历史快照）".to_string())?;
                self.handle_freeze_data_read_async(di, state, address, pool)
                    .await
            }
            _ => {
                // 其他DI - 内存同步读取
                self.handle_read(di, state)
            }
        }
    }

    /// 处理写数据命令
    ///
    /// 参数：
    /// - di: 数据标识 [DI0, DI1, DI2, DI3]
    /// - data: 要写入的数据（已去除DI、密码、操作者代码）
    /// - state: 电表状态（可变引用）
    ///
    /// 返回：Ok(()) 表示写入成功，Err 包含错误信息
    ///
    /// 注意：
    /// - 只有DI3=04的参数可写
    /// - 密码验证和权限检查在调用方（VirtualMeter）完成
    /// - 此方法只负责数据解码和状态更新
    pub fn handle_write(
        &self,
        di: [u8; 4],
        data: &[u8],
        state: &mut MeterState,
    ) -> Result<(), String> {
        use crate::protocol::format::bcd_to_u64;

        // 检查DI3是否为04（参数数据）
        if di[3] != 0x04 {
            return Err(format!(
                "DI {:02X}{:02X}{:02X}{:02X} 不可写入（只有DI3=04的参数可写）",
                di[3], di[2], di[1], di[0]
            ));
        }

        // 根据DI2和DI1路由到具体的写入逻辑
        match (di[2], di[1]) {
            // ========================================
            // 时间参数 (04-00-01-xx)
            // ========================================
            (0x00, 0x01) => {
                match di[0] {
                    0x01 => {
                        // 04-00-01-01: 日期及星期 YYMMDDww（4字节BCD）
                        if data.len() != 4 {
                            return Err(format!("数据长度错误：期望4字节，实际{}字节", data.len()));
                        }

                        let yy = bcd_to_u64(&data[0..1])
                            .map_err(|e| format!("解析年份失败: {}", e))?
                            as i32;
                        let mm = bcd_to_u64(&data[1..2])
                            .map_err(|e| format!("解析月份失败: {}", e))?
                            as u32;
                        let dd = bcd_to_u64(&data[2..3])
                            .map_err(|e| format!("解析日期失败: {}", e))?
                            as u32;
                        // data[3] = ww 星期，暂不使用

                        // 更新虚拟时钟的日期部分（保持时分秒不变）
                        use chrono::{Utc, TimeZone, Timelike};
                        let current = state.virtual_time;
                        let year = 2000 + yy;

                        match Utc
                            .with_ymd_and_hms(
                                year,
                                mm,
                                dd,
                                current.hour(),
                                current.minute(),
                                current.second(),
                            )
                            .single()
                        {
                            Some(new_time) => {
                                state.virtual_time = new_time;
                                Ok(())
                            }
                            None => Err(format!("无效的日期: {:04}-{:02}-{:02}", year, mm, dd)),
                        }
                    }
                    0x02 => {
                        // 04-00-01-02: 时间 hhmmss（3字节BCD）
                        if data.len() != 3 {
                            return Err(format!("数据长度错误：期望3字节，实际{}字节", data.len()));
                        }

                        let hh = bcd_to_u64(&data[0..1])
                            .map_err(|e| format!("解析小时失败: {}", e))?
                            as u32;
                        let mm = bcd_to_u64(&data[1..2])
                            .map_err(|e| format!("解析分钟失败: {}", e))?
                            as u32;
                        let ss = bcd_to_u64(&data[2..3])
                            .map_err(|e| format!("解析秒失败: {}", e))?
                            as u32;

                        // 更新虚拟时钟的时间部分（保持日期不变）
                        use chrono::{Datelike, Utc, TimeZone};
                        let current = state.virtual_time;

                        match Utc
                            .with_ymd_and_hms(
                                current.year(),
                                current.month(),
                                current.day(),
                                hh,
                                mm,
                                ss,
                            )
                            .single()
                        {
                            Some(new_time) => {
                                state.virtual_time = new_time;
                                Ok(())
                            }
                            None => Err(format!("无效的时间: {:02}:{:02}:{:02}", hh, mm, ss)),
                        }
                    }
                    0x03 => {
                        // 04-00-01-03: 最大需量周期（1字节BCD，分钟）
                        if data.len() != 1 {
                            return Err(format!("数据长度错误：期望1字节，实际{}字节", data.len()));
                        }
                        let period = bcd_to_u64(data).map_err(|e| format!("BCD解析失败: {}", e))?;
                        state.demand_period_minutes = period as u16;
                        Ok(())
                    }
                    0x04 => {
                        // 04-00-01-04: 滑差时间（1字节BCD，分钟）
                        if data.len() != 1 {
                            return Err(format!("数据长度错误：期望1字节，实际{}字节", data.len()));
                        }
                        let window = bcd_to_u64(data).map_err(|e| format!("BCD解析失败: {}", e))?;
                        state.sliding_window_minutes = window as u16;
                        Ok(())
                    }
                    _ => Err(format!(
                        "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                        di[3], di[2], di[1], di[0]
                    )),
                }
            }

            // ========================================
            // 费率时段参数 (04-00-02-xx 和 04-00-03-xx)
            // ========================================
            (0x00, 0x02) | (0x00, 0x03) => {
                // TODO: 实现时段表、时区表的写入
                Err(format!(
                    "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入（费率时段表功能待实现）",
                    di[3], di[2], di[1], di[0]
                ))
            }

            // ========================================
            // 铭牌参数 (04-00-04-xx，协议标准布局)
            // ========================================
            (0x00, 0x04) => {
                match di[0] {
                    0x01 => {
                        // 04-00-04-01: 通信地址（6字节BCD）
                        if data.len() != 6 {
                            return Err(format!("数据长度错误：期望6字节，实际{}字节", data.len()));
                        }
                        state.address = [data[5], data[4], data[3], data[2], data[1], data[0]];
                        Ok(())
                    }
                    0x09 => {
                        // 04-00-04-09: 电表有功常数（3字节BCD，imp/kWh）
                        if data.len() != 3 {
                            return Err(format!("数据长度错误：期望3字节，实际{}字节", data.len()));
                        }
                        let constant =
                            bcd_to_u64(data).map_err(|e| format!("BCD解析失败: {}", e))?;
                        state.meter_constant = constant as u32;
                        Ok(())
                    }
                    _ => Err(format!(
                        "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                        di[3], di[2], di[1], di[0]
                    )),
                }
            }

            // ========================================
            // 通信速率特征字 (04-00-07-xx)
            // ========================================
            (0x00, 0x07) => {
                let idx = match di[0] {
                    idx @ 0x01..=0x05 => idx as usize - 1,
                    _ => {
                        return Err(format!(
                            "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                            di[3], di[2], di[1], di[0]
                        ))
                    }
                };
                if data.len() != 1 {
                    return Err(format!("数据长度错误：期望1字节，实际{}字节", data.len()));
                }
                state.comm_speed_feature[idx] = data[0];
                // 通信口1（RS485）速率同步到波特率字段
                if idx == 2 {
                    state.baudrate = data[0];
                }
                Ok(())
            }

            // ========================================
            // 能量组合方式 (04-00-06-xx)
            // ========================================
            (0x00, 0x06) => {
                if data.len() != 1 {
                    return Err(format!("数据长度错误：期望1字节，实际{}字节", data.len()));
                }

                match di[0] {
                    0x01 => {
                        // 04-00-06-01: 有功组合方式特征字
                        state.active_combination_word = data[0];
                        Ok(())
                    }
                    0x02 => {
                        // 04-00-06-02: 无功组合方式1特征字
                        state.reactive_combination_1 = data[0];
                        Ok(())
                    }
                    0x03 => {
                        // 04-00-06-03: 无功组合方式2特征字
                        state.reactive_combination_2 = data[0];
                        Ok(())
                    }
                    _ => Err(format!(
                        "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                        di[3], di[2], di[1], di[0]
                    )),
                }
            }

            // ========================================
            // 周休日特征字 (04-00-08-01)
            // ========================================
            (0x00, 0x08) => {
                if di[0] == 0x01 {
                    if data.len() != 1 {
                        return Err(format!("数据长度错误：期望1字节，实际{}字节", data.len()));
                    }
                    state.weekly_rest_day_word = data[0];
                    Ok(())
                } else {
                    Err(format!(
                        "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                        di[3], di[2], di[1], di[0]
                    ))
                }
            }

            // ========================================
            // 冻结和负荷记录模式 (04-00-09-xx 和 04-00-0A-xx)
            // ========================================
            (0x00, 0x09) | (0x00, 0x0A) => {
                // TODO: 实现冻结模式字和负荷记录模式的写入
                Err(format!(
                    "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入（冻结/负荷记录配置待实现）",
                    di[3], di[2], di[1], di[0]
                ))
            }

            // ========================================
            // 结算日设置 (04-00-0B-01~03)，DDhh，2字节BCD
            //
            // 结算日存储冻结（(上N结算日)电能，DI3=00 DI0=01~0C）由
            // MeterState::settlement_rollover_if_due() 在虚拟时钟越过此处配置的
            // 结算日 DD 日 hh 时边界时自动触发转存，此处只负责写入触发条件本身。
            // ========================================
            (0x00, 0x0B) => {
                let idx = match di[0] {
                    idx @ 0x01..=0x03 => idx as usize - 1,
                    _ => {
                        return Err(format!(
                            "DI {:02X}{:02X}{:02X}{:02X} 暂不支持写入",
                            di[3], di[2], di[1], di[0]
                        ))
                    }
                };
                if data.len() != 2 {
                    return Err(format!("数据长度错误：期望2字节(DDhh)，实际{}字节", data.len()));
                }
                let day_bcd = data[0];
                let hour_bcd = data[1];
                // 9999（两字节均为 0x99）表示未设置该结算日
                if day_bcd == 0x99 && hour_bcd == 0x99 {
                    state.settlement_days[idx] = 0;
                    state.settlement_hours[idx] = 0;
                    return Ok(());
                }
                let day = encoding::bcd_to_decimal(day_bcd);
                let hour = encoding::bcd_to_decimal(hour_bcd);
                if day < 1 || day > 28 {
                    return Err(format!(
                        "结算日日期无效: {} (应为01~28，或9999表示未设置)",
                        day
                    ));
                }
                if hour > 23 {
                    return Err(format!("结算日小时无效: {} (应为00~23)", hour));
                }
                state.settlement_days[idx] = day;
                state.settlement_hours[idx] = hour;
                Ok(())
            }

            // ========================================
            // 其他不支持写入的DI2
            // ========================================
            _ => Err(format!(
                "DI {:02X}{:02X}{:02X}{:02X} 不支持写入",
                di[3], di[2], di[1], di[0]
            )),
        }
    }

    /// 处理读数据命令（同步版本，仅内存）
    ///
    /// 参数：
    /// - di: 数据标识 [DI0, DI1, DI2, DI3]
    /// - state: 电表状态（只读访问）
    ///
    /// 返回：BCD 编码后的数据
    pub fn handle_read(&self, di: [u8; 4], state: &MeterState) -> Result<Vec<u8>, String> {
        self.handle_read_sync(di, state)
    }

    fn handle_read_sync(&self, di: [u8; 4], state: &MeterState) -> Result<Vec<u8>, String> {
        // 电能量（含结算日、分相）
        if let Some(result) = self.handle_energy_read(di, state) {
            return result;
        }
        // 瞬时变量
        if let Some(result) = self.handle_instantaneous_read(di, state) {
            return result;
        }
        // 最大需量及发生时间
        if let Some(result) = self.handle_demand_read(di, state) {
            return result;
        }
        // 参变量与厂商信息
        if let Some(result) = self.handle_parameter_read(di, state) {
            return result;
        }
        match di {
            // 事件记录 (03-DI2-DI1-DI0)
            di if di[3] == 0x03 => self.handle_event_record_read(di, state),
            // 负荷记录 (06-10-DI1-DI0) 需要数据库，走异步版本
            di if di[3] == 0x06 && di[2] == 0x10 => {
                Err("负荷记录需要数据库查询，请使用异步版本 handle_read_async()".to_string())
            }
            // 冻结数据 (05-xx-xx-xx)：内存（当前数据/环形缓冲）可满足的
            // 同步返回；历史快照存数据库，走异步版本
            di if di[3] == 0x05 => self.handle_freeze_data_read_sync(di, state),
            _ => Err(format!(
                "未支持的 DI: {:02X}{:02X}{:02X}{:02X}",
                di[3], di[2], di[1], di[0]
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Datelike;

use super::encoding::encode_bcd_current;
    use super::encoding::encode_bcd_energy;
    use super::encoding::encode_bcd_voltage;
    use super::encoding::to_bcd;
    use super::*;
    use chrono::Timelike;

    #[test]
    fn test_encode_bcd() {
        // 测试电压编码 220.5V
        let voltage_bytes = encode_bcd_voltage(220.5);
        assert_eq!(voltage_bytes.len(), 2);
        // 220.5 → 2205 → 0x2205 (BCD) → [0x05, 0x22]
        assert_eq!(voltage_bytes, vec![0x05, 0x22]);

        // 测试电流编码 12.345A
        let current_bytes = encode_bcd_current(12.345);
        assert_eq!(current_bytes.len(), 3);
        // 12.345 → 12345 → 0x012345 (BCD) → [0x45, 0x23, 0x01]
        assert_eq!(current_bytes, vec![0x45, 0x23, 0x01]);

        // 测试电能编码 12345.67 kWh
        let energy_bytes = encode_bcd_energy(12345.67);
        assert_eq!(energy_bytes.len(), 4);
        // 12345.67 → 1234567 → [0x67, 0x45, 0x23, 0x01]
        assert_eq!(energy_bytes, vec![0x67, 0x45, 0x23, 0x01]);
    }

    #[test]
    fn test_settlement_rollover_archives_data() {
        let mut state = MeterState::default();
        state.settlement_days = [1, 0, 0]; // 每月1日结算
        state.set_energy(EnergyType::ForwardActive, None, 100.0);
        state.set_energy(EnergyType::ForwardActive, Some(1), 60.0);
        state.phase_forward_active = [30.0, 35.0, 35.0];
        state.max_demand = 4.5;

        // 虚拟时钟设为某月2日：上次检查为上月15日，跨过了本月1日0点
        use chrono::{Datelike, Utc, TimeZone};
        let now = Utc
            .with_ymd_and_hms(2025, 6, 2, 8, 0, 0)
            .single()
            .unwrap();
        state.virtual_time = now;
        state.last_settlement_rollover = Some(
            Utc
                .with_ymd_and_hms(2025, 5, 15, 0, 0, 0)
                .single()
                .unwrap(),
        );

        state.settlement_rollover_if_due();

        // 上1结算日 = 本转存前的当前值
        assert!(
            (state.get_settlement_energy(1, EnergyType::ForwardActive, None) - 100.0).abs() < 1e-9
        );
        assert!(
            (state.get_settlement_energy(1, EnergyType::ForwardActive, Some(1)) - 60.0).abs()
                < 1e-9
        );
        for phase in 0..3u8 {
            assert!(
                (state.get_phase_energy(1, phase) - state.phase_forward_active[phase as usize])
                    .abs()
                    < 1e-9
            );
        }
        // 需量转存后归零
        assert_eq!(state.max_demand, 0.0);
        let handler = DIHandler::new();
        // 01010100 (上1结算日)正向有功总需量 = 4.5
        let result = handler
            .handle_read([0x01, 0x00, 0x01, 0x01], &state)
            .unwrap();
        assert_eq!(result.len(), 8); // 需量3字节(XX.XXXX) + 发生时间5字节(YYMMDDhhmm)

        // 未跨结算日时不转存：再次调用不会清掉上1结算日数据
        state.set_energy(EnergyType::ForwardActive, None, 200.0);
        state.settlement_rollover_if_due();
        assert!(
            (state.get_settlement_energy(1, EnergyType::ForwardActive, None) - 100.0).abs() < 1e-9
        );
        let _ = state.virtual_time.day();
    }

    #[test]
    fn test_write_settlement_day_config() {
        let mut state = MeterState::default();
        let handler = DIHandler::new();

        // 写入 04-00-0B-01：每月第1结算日 = 15日8时（DDhh BCD：15 08）
        assert!(handler
            .handle_write([0x01, 0x0B, 0x00, 0x04], &[0x15, 0x08], &mut state)
            .is_ok());
        assert_eq!(state.settlement_days[0], 15);
        assert_eq!(state.settlement_hours[0], 8);

        // 读回验证与写入一致 (04-00-0B-01 读格式为 DDhh 两字节BCD)
        let result = handler
            .handle_read([0x01, 0x0B, 0x00, 0x04], &state)
            .unwrap();
        assert_eq!(result, vec![0x15, 0x08]);

        // 写入 04-00-0B-02：9999 表示取消该结算日设置
        assert!(handler
            .handle_write([0x02, 0x0B, 0x00, 0x04], &[0x99, 0x99], &mut state)
            .is_ok());
        assert_eq!(state.settlement_days[1], 0);
        assert_eq!(state.settlement_hours[1], 0);

        // 非法日期（0日、29日以上）应报错
        assert!(handler
            .handle_write([0x03, 0x0B, 0x00, 0x04], &[0x00, 0x08], &mut state)
            .is_err());
        assert!(handler
            .handle_write([0x03, 0x0B, 0x00, 0x04], &[0x29, 0x08], &mut state)
            .is_err());
        // 非法小时（24时以上）应报错
        assert!(handler
            .handle_write([0x03, 0x0B, 0x00, 0x04], &[0x01, 0x24], &mut state)
            .is_err());
        // 数据长度错误应报错
        assert!(handler
            .handle_write([0x01, 0x0B, 0x00, 0x04], &[0x15], &mut state)
            .is_err());
        // 序号越界（idx=4）应报错
        assert!(handler
            .handle_write([0x04, 0x0B, 0x00, 0x04], &[0x15, 0x08], &mut state)
            .is_err());
    }

    #[test]
    fn test_energy_block_and_rate_reads() {
        let mut state = MeterState::default();
        state.set_energy(EnergyType::ReverseActive, None, 100.0);
        state.set_energy(EnergyType::ReverseActive, Some(1), 40.0);
        state.set_energy(EnergyType::ReverseActive, Some(2), 60.0);
        let handler = DIHandler::new();

        // 00020000 反向有功总电能
        let result = handler.handle_read([0x00, 0x00, 0x02, 0x00], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 4);

        // 00020100 反向有功费率1电能 (DI1=01)
        let result = handler.handle_read([0x00, 0x01, 0x02, 0x00], &state);
        assert!(result.is_ok());

        // 0002FF00 反向有功电能数据块 (总+费率1~num_rates)
        let result = handler.handle_read([0x00, 0xFF, 0x02, 0x00], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 4 * (1 + state.num_rates as usize));

        // 0003FF00 / 0004FF00 无功组合块
        for di2 in [0x03u8, 0x04] {
            let result = handler.handle_read([0x00, 0xFF, di2, 0x00], &state);
            assert!(result.is_ok(), "000{di2:X}FF00 should be supported");
        }

        // 超出配置费率数的费率号应报错
        let result = handler.handle_read([0x00, 0x10, 0x02, 0x00], &state);
        assert!(result.is_err());
    }

    #[test]
    fn test_extended_energy_and_quality_reads() {
        let mut state = MeterState::default();
        state.set_energy(EnergyType::ForwardActive, None, 120.0);
        state.set_energy(EnergyType::ReverseActive, None, 30.0);
        state.set_settlement_energy(1, EnergyType::ForwardActive, None, 88.0);
        state.phase_forward_active = [10.0, 20.0, 30.0];
        state.voltage_thd = [1.5, 1.5, 1.5];
        state.voltage_harmonics[0][3] = 2.5;
        state.neutral_current = 0.123;
        let handler = DIHandler::new();

        // 00000000 组合有功总 = 正向 - 反向 = 90.0
        let result = handler.handle_read([0x00, 0x00, 0x00, 0x00], &state);
        assert!(result.is_ok());
        let bytes = result.unwrap();
        // 90.00 → 9000 → [0x00, 0x90] 小端 BCD
        assert_eq!(bytes, vec![0x00, 0x90, 0x00, 0x00]);

        // 00010001 (上1结算日) 正向有功总 = 88.0
        let result = handler.handle_read([0x01, 0x00, 0x01, 0x00], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 4);

        // 00150000 A相正向有功 = 10.0
        let result = handler.handle_read([0x00, 0x00, 0x15, 0x00], &state);
        assert!(result.is_ok());

        // 0005FF00 第一象限无功组合块
        let result = handler.handle_read([0x00, 0xFF, 0x05, 0x00], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 4 * (1 + state.num_rates as usize));

        // 02080100 A相电压失真度
        assert!(handler
            .handle_read([0x00, 0x01, 0x08, 0x02], &state)
            .is_ok());
        // 0208FF00 失真度数据块 (3×2字节)
        let result = handler.handle_read([0x00, 0xFF, 0x08, 0x02], &state);
        assert_eq!(result.unwrap().len(), 6);
        // 020A0103 A相电压3次谐波
        assert!(handler
            .handle_read([0x03, 0x01, 0x0A, 0x02], &state)
            .is_ok());
        // 020A01FF A相电压谐波数据块 (21×2字节)
        let result = handler.handle_read([0xFF, 0x01, 0x0A, 0x02], &state);
        assert_eq!(result.unwrap().len(), 42);
        // 02800001 零线电流
        let result = handler.handle_read([0x01, 0x00, 0x80, 0x02], &state);
        assert_eq!(result.unwrap().len(), 3);
        // 02070100 A相相角（由功率因数 0.95 反推 ≈ 18.2°）
        let result = handler.handle_read([0x00, 0x01, 0x07, 0x02], &state);
        assert!(result.is_ok());
        // 01010100 正向有功费率1需量
        assert!(handler
            .handle_read([0x00, 0x01, 0x01, 0x01], &state)
            .is_ok());
    }

    #[test]
    fn test_protocol_extended_reads() {
        let mut state = MeterState::default();
        state.remaining_energy = 50.0;
        state.remaining_amount = 25.5;
        state.meter_temperature = 26.5;
        state.current_step_price = 0.55;
        state.settlement_days = [1, 0, 0];
        let handler = DIHandler::new();

        // 04000101 日期及星期 (4字节) / 04000102 时间 (3字节)
        assert_eq!(
            handler
                .handle_read([0x01, 0x01, 0x00, 0x04], &state)
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            handler
                .handle_read([0x02, 0x01, 0x00, 0x04], &state)
                .unwrap()
                .len(),
            3
        );
        // 04000103/04 需量周期/滑差时间
        assert_eq!(
            handler
                .handle_read([0x03, 0x01, 0x00, 0x04], &state)
                .unwrap(),
            vec![to_bcd(state.demand_period_minutes as u8)]
        );
        // 04000703 通信口1速率特征字
        assert!(handler
            .handle_read([0x03, 0x07, 0x00, 0x04], &state)
            .is_ok());
        // 040005FF 状态字数据块 = 运行状态字1~7（7×2字节）+ 密钥状态字（4字节）
        assert_eq!(
            handler
                .handle_read([0xFF, 0x05, 0x00, 0x04], &state)
                .unwrap()
                .len(),
            18
        );
        // 04000901 负荷记录模式字 / 04000A02 第1类间隔
        assert!(handler
            .handle_read([0x01, 0x09, 0x00, 0x04], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x02, 0x0A, 0x00, 0x04], &state)
            .is_ok());
        // 04000B01 结算日 DDhh
        assert_eq!(
            handler
                .handle_read([0x01, 0x0B, 0x00, 0x04], &state)
                .unwrap(),
            vec![0x01, 0x00]
        );

        // 00800000 关联总电能
        assert!(handler
            .handle_read([0x00, 0x00, 0x80, 0x00], &state)
            .is_ok());
        // 00900001 剩余电量 / 00900002 剩余金额
        assert!(handler
            .handle_read([0x00, 0x01, 0x90, 0x00], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x00, 0x02, 0x90, 0x00], &state)
            .is_ok());
        // 000B0000 结算周期累计用电量
        assert!(handler
            .handle_read([0x00, 0x00, 0x0B, 0x00], &state)
            .is_ok());
        // 0001FFFF 正向有功当前+12结算日数据块
        let result = handler.handle_read([0xFF, 0xFF, 0x01, 0x00], &state);
        assert_eq!(
            result.unwrap().len(),
            4 * 13 * (1 + state.num_rates as usize)
        );

        // 需量扩展：反向费率、分相、结算日
        assert!(handler
            .handle_read([0x00, 0x02, 0x02, 0x01], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x00, 0x00, 0x15, 0x01], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x01, 0x00, 0x01, 0x01], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x00, 0xFF, 0x04, 0x01], &state)
            .is_ok());

        // 02800007 表内温度 / 0280000B 阶梯电价
        assert!(handler
            .handle_read([0x07, 0x00, 0x80, 0x02], &state)
            .is_ok());
        assert!(handler
            .handle_read([0x0B, 0x00, 0x80, 0x02], &state)
            .is_ok());
    }

    #[test]
    fn test_instantaneous_block_reads() {
        let state = MeterState::default();
        let handler = DIHandler::new();

        // 无功功率数据块 0204FF00 (总+三相)
        let result = handler.handle_read([0x00, 0xFF, 0x04, 0x02], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3 * 4);

        // 视在功率数据块 0205FF00 (总+三相)
        let result = handler.handle_read([0x00, 0xFF, 0x05, 0x02], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3 * 4);

        // 功率因数数据块 0206FF00 (总+三相)
        let result = handler.handle_read([0x00, 0xFF, 0x06, 0x02], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2 * 4);

        // 电网频率 02800002
        let result = handler.handle_read([0x02, 0x00, 0x80, 0x02], &state);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);

        // 各类需量总值 0101~0104 0000
        for di2 in [0x01u8, 0x02, 0x03, 0x04] {
            let result = handler.handle_read([0x00, 0x00, di2, 0x01], &state);
            assert!(result.is_ok(), "010{di2:X}0000 should be supported");
        }
    }

    #[test]
    fn test_di_handler() {
        let state = MeterState::default();
        let handler = DIHandler::new();

        // 测试读电压
        let result = handler.handle_read([0x00, 0x01, 0x01, 0x02], &state);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data.len(), 2);

        // 测试读电流
        let result = handler.handle_read([0x00, 0x01, 0x02, 0x02], &state);
        assert!(result.is_ok());

        // 测试读功率
        let result = handler.handle_read([0x00, 0x00, 0x03, 0x02], &state);
        assert!(result.is_ok());

        // 测试读电能
        let result = handler.handle_read([0x00, 0x00, 0x01, 0x00], &state);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data.len(), 4);
    }

    #[test]
    fn test_di_handler_write() {
        use crate::protocol::format::u64_to_bcd;

        let handler = DIHandler::new();
        let mut state = MeterState::default();

        // 测试1: 写入通信速率特征字 (04-00-07-03 通信口1)
        let di = [0x03, 0x07, 0x00, 0x04];
        let data = vec![0x04];
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.baudrate, 0x04);
        assert_eq!(state.comm_speed_feature[2], 0x04);

        // 测试2: 写入电表有功常数 (04-00-04-09)
        let di = [0x09, 0x04, 0x00, 0x04];
        let data = u64_to_bcd(1600, 3);
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.meter_constant, 1600);

        // 测试3: 写入最大需量周期 (04-00-01-03)
        let di = [0x03, 0x01, 0x00, 0x04];
        let data = vec![to_bcd(15)];
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.demand_period_minutes, 15);

        // 测试3b: 写入通信地址 (04-00-04-01)
        let di = [0x01, 0x04, 0x00, 0x04];
        let data = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.address, [0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);

        // 测试4: 写入有功组合方式 (04-00-06-01)
        let di = [0x01, 0x06, 0x00, 0x04];
        let data = vec![0x12];
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.active_combination_word, 0x12);

        // 测试5: 写入周休日 (04-00-08-01)
        let di = [0x01, 0x08, 0x00, 0x04];
        let data = vec![0x07];
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.weekly_rest_day_word, 0x07);

        // 测试6: 写入日期 (04-00-01-01)
        let di = [0x01, 0x01, 0x00, 0x04];
        let data = vec![0x24, 0x06, 0x15, 0x03]; // YY=24, MM=06, DD=15
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.virtual_time.year(), 2024);
        assert_eq!(state.virtual_time.month(), 6);
        assert_eq!(state.virtual_time.day(), 15);

        // 测试7: 写入时间 (04-00-01-02)
        let di = [0x02, 0x01, 0x00, 0x04];
        let data = vec![0x10, 0x30, 0x45]; // hh=10, mm=30, ss=45
        assert!(handler.handle_write(di, &data, &mut state).is_ok());
        assert_eq!(state.virtual_time.hour(), 10);
        assert_eq!(state.virtual_time.minute(), 30);
        assert_eq!(state.virtual_time.second(), 45);

        // 测试8: 尝试写入DI3!=04的数据（应该失败）
        let di = [0x00, 0x00, 0x01, 0x00]; // 电能数据
        let data = vec![0x00, 0x00, 0x00, 0x00];
        assert!(handler.handle_write(di, &data, &mut state).is_err());

        // 测试9: 数据长度错误（应该失败）：通信地址需要6字节，给了2字节
        let di = [0x01, 0x04, 0x00, 0x04];
        let data = vec![0x04, 0x05];
        assert!(handler.handle_write(di, &data, &mut state).is_err());
    }
}

#[test]
fn test_event_record_read() {
    use chrono::{Duration, Utc};

    let handler = DIHandler::new();
    let mut state = MeterState::default();
    let now = Utc::now();

    // 添加编程记录事件
    state.add_event_record(
        0x30, // 编程记录
        0x0F, // 费率参数表编程
        now,
        vec![0x01, 0x02, 0x03, 0x04], // 操作者代码
    );

    // 添加故障事件
    let fault_start = now - Duration::minutes(30);
    state.add_event_record(
        0x01, // 失压事件
        0x01, // A相失压
        fault_start,
        vec![0xAA, 0xBB], // 故障数据
    );
    state.end_event_record(0x01, 0x01, now);

    // 测试1：读取编程记录明细（DI=03-30-0F-01）
    let di = [0x01, 0x0F, 0x30, 0x03];
    let result = handler.handle_read_sync(di, &state).unwrap();

    // 验证：7字节时间 + 7字节结束时间（00） + 4字节数据
    // 实际可能多一字节（星期），让我们打印实际长度
    println!("编程记录数据长度：{}", result.len());
    assert!(result.len() >= 14); // 至少14字节（两个时间戳）
                                 // 最后4字节应该是数据
    let data_start = result.len() - 4;
    assert_eq!(&result[data_start..], &[0x01, 0x02, 0x03, 0x04]);

    // 测试2：读取故障事件汇总（DI=03-01-01-00）
    let di = [0x00, 0x01, 0x01, 0x03];
    let result = handler.handle_read_sync(di, &state).unwrap();

    // 验证：3字节总次数 + 3字节总累计时间（附录A.4格式）
    assert_eq!(result.len(), 6);
    // 总次数=1，BCD格式：01 00 00
    assert_eq!(result[0], 0x01);
    assert_eq!(result[1], 0x00);
    assert_eq!(result[2], 0x00);
    // 总时长=30分钟，BCD格式：30 00 00
    assert_eq!(result[3], 0x30);
    assert_eq!(result[4], 0x00);
    assert_eq!(result[5], 0x00);
}