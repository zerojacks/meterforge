// 表A.1 电能量数据标识读取（DI3=00）
//
// DI2 分组：
// - 00 组合有功（正向有功 - 反向有功）
// - 01 正向有功
// - 02 反向有功
// - 03 组合无功1（映射到仿真模型的正向无功）
// - 04 组合无功2（映射到仿真模型的反向无功）
// - 05~08 四象限无功
// - 09/0A 正向/反向视在
// - 15/29/3D A/B/C 相正向有功
//
// DI1：
// - 00 总值，01~3F 费率值，FF 组合块
//
// DI0：
// - 00 当前结算周期，01~0C 上1~12结算日

use super::encoding::*;
use super::{EnergyType, MeterState};
use crate::simulation::di_handler::DIHandler;

impl DIHandler {
    /// 电能量读取入口
    ///
    /// 返回 None 表示 DI 不属于电能量区间，交回通用匹配。
    pub(super) fn handle_energy_read(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Option<Result<Vec<u8>, String>> {
        if di[3] != 0x00 {
            return None;
        }
        // DI0 = 结算日序号：00=当前，01~0C=上1~12结算日，FF=当前+12结算日数据块
        let settlement = bcd_to_decimal(di[0]);
        if di[0] != 0xFF && settlement > 0x0C {
            return None;
        }
        // 分相正向有功 (00-15/29/3D-xx-xx，含结算日)
        if di[1] == 0x00 && di[0] != 0xFF {
            if let Some(result) = Self::read_phase_energy(di[2], settlement, state) {
                return Some(result);
            }
        }
        // 关联总电能 (00-80-xx-00)：映射到正向有功
        if di[2] == 0x80 {
            if di[1] == 0x00 || di[1] == 0xFF || (0x01..=0x3F).contains(&di[1]) {
                return Some(Self::read_energy_item(
                    EnergyType::ForwardActive,
                    di[1],
                    settlement,
                    state,
                ));
            }
            return None;
        }
        // 当前结算周期组合有功总累计用电量 (00-0B-00-00)
        if di[2] == 0x0B && settlement == 0 {
            if di[1] == 0x00 {
                return Some(Ok(encode_bcd_energy(state.get_settlement_energy(
                    0,
                    EnergyType::CombinedActive,
                    None,
                ))));
            }
            return None;
        }
        // 费控剩余电量/剩余金额 (00-90-01-00 / 00-90-02-00)
        if di[2] == 0x90 && settlement == 0 && di[1] == 0x01 {
            return Some(Ok(encode_bcd_energy(state.remaining_energy)));
        }
        if di[2] == 0x90 && settlement == 0 && di[1] == 0x02 {
            return Some(Ok(encode_bcd_energy(state.remaining_amount)));
        }
        // 某项当前和12个结算日电能数据块 (00-ZZ-ZZ-FF)
        if di[0] == 0xFF && di[1] == 0xFF {
            let energy_type = Self::energy_type_for_di2(di[2])?;
            return Some(Self::read_energy_span_block(energy_type, state));
        }
        let energy_type = Self::energy_type_for_di2(di[2])?;
        match di[1] {
            0x00 | 0x01..=0x3F | 0xFF => Some(Self::read_energy_item(
                energy_type,
                di[1],
                settlement,
                state,
            )),
            _ => None,
        }
    }

    /// DI2 → 能量类型映射
    fn energy_type_for_di2(di2: u8) -> Option<EnergyType> {
        match di2 {
            0x00 => Some(EnergyType::CombinedActive),
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

    /// 读取单个电能量项（总/费率/组合块），settlement=0 为当前结算周期
    fn read_energy_item(
        energy_type: EnergyType,
        di1: u8,
        settlement: u8,
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        let read_one = |rate: Option<u8>| -> Vec<u8> {
            encode_bcd_energy(state.get_settlement_energy(settlement, energy_type, rate))
        };
        match di1 {
            // 总电能
            0x00 => Ok(read_one(None)),
            // 费率电能 (BCD 编码的费率号)
            0x01..=0x3F => {
                let rate_number = bcd_to_decimal(di1);
                if rate_number >= 1 && rate_number <= state.num_rates {
                    Ok(read_one(Some(rate_number)))
                } else {
                    Err(format!(
                        "费率号超出范围: {} (配置的费率数: {})",
                        rate_number, state.num_rates
                    ))
                }
            }
            // 组合块：总 + 所有配置的费率
            0xFF => {
                let mut data = read_one(None);
                for rate in 1..=state.num_rates {
                    data.extend(read_one(Some(rate)));
                }
                Ok(data)
            }
            _ => unreachable!("di1 已在调用方过滤"),
        }
    }

    /// 当前+12个结算日数据块：每段 = 总 + 所有配置费率
    fn read_energy_span_block(
        energy_type: EnergyType,
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        let mut data = Vec::new();
        for settlement in 0..=12u8 {
            data.extend(encode_bcd_energy(state.get_settlement_energy(
                settlement,
                energy_type,
                None,
            )));
            for rate in 1..=state.num_rates {
                data.extend(encode_bcd_energy(state.get_settlement_energy(
                    settlement,
                    energy_type,
                    Some(rate),
                )));
            }
        }
        Ok(data)
    }

    /// 分相正向有功电能：DI2 = 15(A相) / 29(B相) / 3D(C相)
    fn read_phase_energy(
        di2: u8,
        settlement: u8,
        state: &MeterState,
    ) -> Option<Result<Vec<u8>, String>> {
        let phase = match di2 {
            0x15 => 0,
            0x29 => 1,
            0x3D => 2,
            _ => return None,
        };
        Some(Ok(encode_bcd_energy(
            state.get_phase_energy(settlement, phase),
        )))
    }
}