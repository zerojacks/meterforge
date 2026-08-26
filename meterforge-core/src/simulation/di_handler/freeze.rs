// 表A.6 冻结数据读取（DI3=05）

use super::encoding::*;
use super::MeterState;
use crate::simulation::di_handler::DIHandler;

impl DIHandler {
    /// 异步版本：处理冻结数据读取（支持数据库查询）
    ///
    /// 此方法应在异步上下文中调用，用于支持从数据库加载历史冻结快照。
    ///
    /// 冻结数据格式：05-DI2-DI1-DI0
    /// - DI2：触发类型
    /// - DI1：数据类别
    /// - DI0：快照序号
    ///
    /// 读取策略：
    /// - DI0=00: 当前数据（从 MeterState 实时查询）
    /// - DI0=01 ~ max_memory_index: 优先从内存环形缓冲读取，若内存中没有则从数据库查询
    /// - DI0 > max_memory_index: 仅从数据库查询
    ///
    /// 这样即使软件启动时内存中没有冻结快照，也能从数据库中加载历史数据。
    pub async fn handle_freeze_data_read_async(
        &self,
        di: [u8; 4],
        state: &MeterState,
        address: &str,
        db_pool: &sqlx::SqlitePool,
    ) -> Result<Vec<u8>, String> {
        use crate::persistence::worker::PersistenceWorker;
        use crate::simulation::state::FreezeTrigger;

        // 内存（当前数据/环形缓冲）能命中的部分不需要数据库
        if let Some(data) = self.try_read_freeze_from_memory(di, state)? {
            return Ok(data);
        }

        let di0 = di[0];
        let di1 = di[1];
        let trigger = FreezeTrigger::from_di2(di[2])
            .ok_or_else(|| format!("无效的冻结触发类型: DI2={:02X}", di[2]))?;

        // 内存中没有数据（或 DI0 > max_memory_index），从数据库查询
        //
        // 注意：数据库里每次冻结只落一行完整摘要（category=0xFF，见
        // write_freeze_snapshot），具体某个 DI1 类别的数据是从这行完整快照里
        // 现场解码出来的（encode_freeze_data_from_db_row），所以查询时 category
        // 固定传 0xFF，不能直接传 di1 —— 之前这里传 di1 会导致除 di1=0xFF 外的
        // 查询永远查不到数据（库里根本没有那个 category 的行）。
        let snapshot_row =
            PersistenceWorker::query_freeze_snapshot(db_pool, address, trigger.to_di2(), 0xFF, di0)
                .await
                .map_err(|e| format!("数据库查询失败: {}", e))?
                .ok_or_else(|| {
                    format!(
                        "无此冻结快照（内存和数据库均无记录）: trigger={:?}, index={:02X}",
                        trigger, di0
                    )
                })?;

        // 从数据库行解析数据，按 di1 抽取具体类别
        self.encode_freeze_data_from_db_row(di1, &snapshot_row)
    }

    /// 同步版本：只处理内存可满足的冻结读取
    ///
    /// DI0=00（当前数据）或 DI0≤内存容量且环形缓冲命中时直接返回；
    /// 否则（历史快照存在数据库）返回错误，提示走异步版本。
    pub(super) fn handle_freeze_data_read_sync(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        match self.try_read_freeze_from_memory(di, state)? {
            Some(data) => Ok(data),
            None => Err("冻结数据需要数据库支持，请使用异步版本 handle_read_async()".to_string()),
        }
    }

    /// 尝试从内存满足冻结读取
    ///
    /// 返回 Ok(Some(data)) 表示内存命中；Ok(None) 表示需要走数据库；
    /// Err 表示 DI 本身非法（触发类型/数据类别无法解析）。
    fn try_read_freeze_from_memory(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Result<Option<Vec<u8>>, String> {
        use crate::simulation::state::FreezeTrigger;

        let trigger = FreezeTrigger::from_di2(di[2])
            .ok_or_else(|| format!("无效的冻结触发类型: DI2={:02X}", di[2]))?;

        // DI0=00: 当前数据
        if di[0] == 0x00 {
            return Ok(Some(self.read_current_freeze_data(di[1], state)?));
        }

        // DI0 ≤ max_memory_index: 从内存环形缓冲读取
        if di[0] <= trigger.max_history_count() {
            if let Some(snapshot) = state.get_freeze_snapshot(trigger, di[0]) {
                return Ok(Some(self.encode_freeze_data_item(
                    di[1],
                    &snapshot.data,
                    &snapshot.snapshot_time,
                )?));
            }
        }
        Ok(None)
    }

    /// 从数据库行编码冻结数据
    fn encode_freeze_data_from_db_row(
        &self,
        di1: u8,
        row: &crate::persistence::FreezeSnapshotRow,
    ) -> Result<Vec<u8>, String> {
        // 整体反序列化为 FreezeData 后复用与内存快照一致的编码路径；
        // 旧数据行缺失的字段由 serde default 兜底（值为 0）
        let freeze_data: crate::simulation::state::FreezeData =
            serde_json::from_value(row.payload.clone())
                .map_err(|e| format!("解析冻结快照 payload 失败: {}", e))?;
        self.encode_freeze_data_item(di1, &freeze_data, &row.snapshot_time)
    }

    /// 读取"当前"冻结数据（DI0=00）
    ///
    /// 当 DI0=00 时，不读取历史快照，而是直接返回当前 MeterState 的实时数据
    fn read_current_freeze_data(&self, di1: u8, state: &MeterState) -> Result<Vec<u8>, String> {
        // 当前数据（DI0=00）与历史快照共用同一编码路径
        let freeze_data = crate::simulation::state::FreezeData::from_meter_state(state);
        self.encode_freeze_data_item(di1, &freeze_data, &state.virtual_time)
    }

    /// 编码冻结快照数据项
    ///
    /// 根据 DI1 从快照中提取对应的数据并编码。
    ///
    /// 电能类（DI1=01~08）按附录A.6输出 总 + 费率1~n（4×(1+n) 字节）；
    /// 需量类（09/0A）输出 总 + 各费率的（需量3字节 + 发生时间5字节）；
    /// 变量块（10）输出 8 项 × 3 字节瞬时变量；
    /// FF 数据块 = 00 + 01~0A 顺序拼接。
    fn encode_freeze_data_item(
        &self,
        di1: u8,
        freeze_data: &crate::simulation::state::FreezeData,
        snapshot_time: &chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<u8>, String> {
        // 电能项：总 + 各费率
        let energy_item = |total: f64, rates: &[f64]| -> Vec<u8> {
            let mut data = encode_bcd_energy(total);
            for &value in rates {
                data.extend(encode_bcd_energy(value));
            }
            data
        };
        // 需量项：总 + 各费率，每项 = 需量(3B XX.XXXX) + 发生时间(5B YYMMDDhhmm)
        let demand_item = |total: f64,
                           total_time: &chrono::DateTime<chrono::Utc>,
                           rates: &[(f64, chrono::DateTime<chrono::Utc>)]|
         -> Vec<u8> {
            let mut data = encode_bcd_power(total);
            data.extend(encode_freeze_datetime(total_time));
            for (value, time) in rates {
                data.extend(encode_bcd_power(*value));
                data.extend(encode_freeze_datetime(time));
            }
            data
        };

        match di1 {
            // 00: 冻结时间 YYMMDDWW hh:mm:ss
            0x00 => {
                let data = encode_datetime(snapshot_time);
                Ok(data)
            }
            // 01~04: 正/反向有功、正/反向无功（总+费率）
            0x01 => Ok(energy_item(
                freeze_data.forward_active_total,
                &freeze_data.forward_active_rates,
            )),
            0x02 => Ok(energy_item(
                freeze_data.reverse_active_total,
                &freeze_data.reverse_active_rates,
            )),
            0x03 => Ok(energy_item(
                freeze_data.forward_reactive_total,
                &freeze_data.forward_reactive_rates,
            )),
            0x04 => Ok(energy_item(
                freeze_data.reverse_reactive_total,
                &freeze_data.reverse_reactive_rates,
            )),
            // 05~08: 第一~四象限无功（总+费率）
            0x05 => Ok(energy_item(
                freeze_data.quadrant1_reactive_total,
                &freeze_data.quadrant1_reactive_rates,
            )),
            0x06 => Ok(energy_item(
                freeze_data.quadrant2_reactive_total,
                &freeze_data.quadrant2_reactive_rates,
            )),
            0x07 => Ok(energy_item(
                freeze_data.quadrant3_reactive_total,
                &freeze_data.quadrant3_reactive_rates,
            )),
            0x08 => Ok(energy_item(
                freeze_data.quadrant4_reactive_total,
                &freeze_data.quadrant4_reactive_rates,
            )),
            // 09: 正向有功最大需量及发生时间（总+费率）
            0x09 => Ok(demand_item(
                freeze_data.max_demand_active,
                &freeze_data.max_demand_active_time,
                &freeze_data.max_demand_active_rates,
            )),
            // 0A: 反向有功最大需量及发生时间（总+费率，复用无功需量数据）
            0x0A => Ok(demand_item(
                freeze_data.max_demand_reactive,
                &freeze_data.max_demand_reactive_time,
                &freeze_data.max_demand_reactive_rates,
            )),
            // 10: 瞬时变量数据块（8项 × 3字节 XX.XXXX）：
            // A/B/C 相电压、三相电流、有功、无功、功率因数（格式按协议 3×8）
            0x10 => {
                let empty3 = || [0.0f64; 3];
                let voltages = freeze_data.voltages.unwrap_or_else(empty3);
                let currents = freeze_data.currents.unwrap_or_else(empty3);
                let mut data = Vec::new();
                for value in [
                    voltages[0],
                    voltages[1],
                    voltages[2],
                    currents[0],
                    currents[1],
                    currents[2],
                    freeze_data.active_power.unwrap_or(0.0),
                    freeze_data.reactive_power.unwrap_or(0.0),
                ] {
                    data.extend(encode_bcd_power(value));
                }
                Ok(data)
            }
            // 15/16/17: A/B/C 相电压（单项）
            0x15 | 0x16 | 0x17 => {
                if let Some(voltages) = freeze_data.voltages {
                    let idx = (di1 - 0x15) as usize;
                    Ok(encode_bcd_voltage(voltages[idx]))
                } else {
                    Err("快照中无电压数据".to_string())
                }
            }
            // FF: 数据块（时间 + 全部电能/需量类别）
            0xFF => {
                let mut data = Vec::new();
                for category in 0x00..=0x0A {
                    data.extend(self.encode_freeze_data_item(
                        category,
                        freeze_data,
                        snapshot_time,
                    )?);
                }
                Ok(data)
            }
            _ => Err(format!("未支持的冻结数据类别: DI1={:02X}", di1)),
        }
    }
}

/// 冻结需量发生时间编码（YYMMDDhhmm，5字节BCD）
fn encode_freeze_datetime(dt: &chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    vec![
        to_bcd((dt.year() % 100) as u8),
        to_bcd(dt.month() as u8),
        to_bcd(dt.day() as u8),
        to_bcd(dt.hour() as u8),
        to_bcd(dt.minute() as u8),
    ]
}