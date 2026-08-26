// 表A.2 最大需量及发生时间数据标识读取（DI3=01）
//
// - 01-0x-00-00 各类需量总值（x=01~0A 对应能量类型）
// - 01-0x-xx-00 各类费率需量
// - 01-0x-FF-00 各类需量数据块
// - 01-15~1E / 29~32 / 3D~46 分相需量（A/B/C）
// - DI0=01~0C 结算日需量
//
// 需量寄存器查无值时回退到 max_demand/max_demand_time。

use super::encoding::*;
use super::{EnergyType, MeterState};
use crate::simulation::di_handler::DIHandler;

impl DIHandler {
    /// 最大需量读取入口
    ///
    /// 返回 None 表示 DI 不属于需量区间，交回通用匹配。
    pub(super) fn handle_demand_read(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Option<Result<Vec<u8>, String>> {
        if di[3] != 0x01 {
            return None;
        }
        // DI0：00=当前，01~0C=上1~12结算日
        if di[0] > 0x0C {
            return None;
        }

        // 分相需量：0x15~0x1E=A相 0x29~0x32=B相 0x3D~0x46=C相
        let phase_of = |di2: u8| -> Option<u8> {
            match di2 {
                0x15..=0x1E => Some(1),
                0x29..=0x32 => Some(2),
                0x3D..=0x46 => Some(3),
                _ => None,
            }
        };
        if let Some(phase) = phase_of(di[2]) {
            if di[1] == 0x00 {
                let demand = state.get_demand(di[0], phase, EnergyType::ForwardActive, None);
                let mut data = encode_bcd_power(demand.value);
                data.extend(encode_datetime(&demand.time));
                return Some(Ok(data));
            }
            return None;
        }

        // 分类需量：DI2=01~0A 对应能量类型
        let energy_type = Self::demand_type_for_di2(di[2])?;
        let read_one = |rate: Option<u8>| -> Vec<u8> {
            let demand = state.get_demand(di[0], 0, energy_type, rate);
            let mut data = encode_bcd_power(demand.value);
            data.extend(encode_datetime(&demand.time));
            data
        };
        let result = match di[1] {
            // 总需量
            0x00 => Ok(read_one(None)),
            // 费率需量 (BCD 编码的费率号)
            rate @ 0x01..=0x3F => {
                let rate_number = bcd_to_decimal(rate);
                if rate_number >= 1 && rate_number <= state.num_rates {
                    Ok(read_one(Some(rate_number)))
                } else {
                    Err(format!(
                        "费率号超出范围: {} (配置的费率数: {})",
                        rate_number, state.num_rates
                    ))
                }
            }
            // 数据块：总 + 所有配置费率
            0xFF => {
                let mut data = read_one(None);
                for rate in 1..=state.num_rates {
                    data.extend(read_one(Some(rate)));
                }
                Ok(data)
            }
            _ => return None,
        };
        Some(result)
    }

    /// 需量能量类型映射：01=正向有功 02=反向有功 03=组合无功1 04=组合无功2
    /// 05~08=四象限无功 09=正向视在 0A=反向视在
    fn demand_type_for_di2(di2: u8) -> Option<EnergyType> {
        match di2 {
            0x01 => Some(EnergyType::ForwardActive),
            0x02 => Some(EnergyType::ReverseActive),
            0x03 => Some(EnergyType::ForwardReactive),
            0x04 => Some(EnergyType::ReverseReactive),
            0x05 => Some(EnergyType::Quadrant1Reactive),
            0x06 => Some(EnergyType::Quadrant2Reactive),
            0x07 => Some(EnergyType::Quadrant3Reactive),
            0x08 => Some(EnergyType::Quadrant4Reactive),
            0x09 => Some(EnergyType::ForwardApparent),
            0x0A => Some(EnergyType::ReverseApparent),
            _ => None,
        }
    }
}
