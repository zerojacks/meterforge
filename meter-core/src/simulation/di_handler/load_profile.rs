// 表A.7 负荷记录读取（DI3=06）

use super::encoding::*;
use super::MeterState;
use crate::simulation::di_handler::DIHandler;
use crate::simulation::state::LoadProfileDataType;

impl DIHandler {
    /// 异步版本：处理负荷记录读取（DI3=06）
    ///
    /// 负荷记录格式：06-10-DI1-DI0
    /// - DI2=10：固定值（负荷记录数据项）
    /// - DI1：数据类型（01=电压，02=电流，03=有功功率，04=无功功率，
    ///                  05=功率因数，06=电能量，07=无功电能，08=需量）
    /// - DI0：通道（00=总，01=A相，02=B相，03=C相）
    ///
    /// 参数：
    /// - di: 数据标识
    /// - state: 电表状态
    /// - address: 电表地址
    /// - db_pool: 数据库连接池
    /// - start_time: 查询起始时间
    /// - end_time: 查询结束时间
    /// - max_records: 最大返回记录数（可选，默认100）
    ///
    /// 返回：BCD编码的负荷记录数据块
    /// 格式：[记录1时间(6字节) + 记录1数据 + 记录2时间 + 记录2数据 + ...]
    pub async fn handle_load_profile_read_async(
        &self,
        di: [u8; 4],
        state: &MeterState,
        address: &str,
        db_pool: &sqlx::SqlitePool,
        start_time: &chrono::DateTime<chrono::Local>,
        end_time: &chrono::DateTime<chrono::Local>,
        max_records: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        use crate::persistence::worker::PersistenceWorker;
        use crate::simulation::state::LoadProfileDataType;

        let di0 = di[0]; // 通道
        let di1 = di[1]; // 数据类型
        let di2 = di[2]; // 应该是0x10

        if di2 != 0x10 {
            return Err(format!("无效的负荷记录DI2：{:02X}（期望10）", di2));
        }

        if di0 > 0x03 {
            return Err(format!("无效的通道号：DI0={:02X}（期望00-03）", di0));
        }

        // 解析数据类型
        let data_type = LoadProfileDataType::from_di1(di1)
            .ok_or_else(|| format!("无效的负荷记录数据类型：DI1={:02X}", di1))?;

        // 从数据库查询负荷记录
        let samples = PersistenceWorker::query_load_profile_samples(
            db_pool,
            address,
            data_type as u8,
            di0,
            start_time,
            end_time,
            max_records.unwrap_or(100),
        )
        .await
        .map_err(|e| format!("数据库查询失败: {}", e))?;

        if samples.is_empty() {
            return Err("未找到负荷记录数据".to_string());
        }

        // 编码负荷记录数据
        let mut data = Vec::new();

        for sample in samples {
            // 采样时间（6字节BCD）
            data.extend(encode_datetime(&sample.sample_time));

            // 采样值（根据数据类型编码）
            data.extend(self.encode_load_profile_value(data_type, sample.value)?);
        }

        Ok(data)
    }

    /// 编码负荷记录采样值
    ///
    /// 根据数据类型，将采样值编码为BCD格式
    fn encode_load_profile_value(
        &self,
        data_type: LoadProfileDataType,
        value: f64,
    ) -> Result<Vec<u8>, String> {
        use crate::simulation::state::LoadProfileDataType;

        match data_type {
            LoadProfileDataType::Voltage => {
                // 电压：XXX.X V（2字节）
                Ok(encode_bcd_voltage(value))
            }
            LoadProfileDataType::Current => {
                // 电流：XXX.XXX A（3字节）
                Ok(encode_bcd_current(value))
            }
            LoadProfileDataType::ActivePower => {
                // 有功功率：XX.XXXX kW（3字节）
                Ok(encode_bcd_power(value))
            }
            LoadProfileDataType::ReactivePower => {
                // 无功功率：XX.XXXX kvar（3字节）
                Ok(encode_bcd_power(value))
            }
            LoadProfileDataType::PowerFactor => {
                // 功率因数：X.XXX（2字节）
                Ok(encode_bcd_power_factor(value))
            }
            LoadProfileDataType::Energy => {
                // 电能量：XXXXXX.XX kWh（4字节）
                Ok(encode_bcd_energy(value))
            }
            LoadProfileDataType::ReactiveEnergy => {
                // 无功电能：XXXXXX.XX kvarh（4字节）
                Ok(encode_bcd_energy(value))
            }
            LoadProfileDataType::Demand => {
                // 需量：XX.XXXX kW（3字节）
                Ok(encode_bcd_power(value))
            }
        }
    }
}
