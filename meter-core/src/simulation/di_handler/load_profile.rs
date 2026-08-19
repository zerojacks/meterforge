// 表A.7 负荷记录读取（DI3=06）

use super::encoding::*;
use super::MeterState;
use crate::simulation::state::{
    DemandBlock, EnergyBlock, LoadRecordData, PfBlock, PqBlock, QuadrantBlock, VifBlock,
};
use crate::simulation::di_handler::DIHandler;
use chrono::{Datelike, Timelike};

impl DIHandler {
    /// 处理负荷记录读取（DI3=06）
    ///
    /// 两类负荷记录：
    /// 1. 第一类（06-DI2-00-xx）：记录块读取，返回附录B格式的完整块
    ///    - DI2: 00=全部类，01~06=第1~6类
    ///    - DI0: 00=最早，01=给定时间（需time参数），02=最近
    ///
    /// 2. 第二类（06-10-DI1-DI0）：曲线数据读取，返回时间范围内的特定数据项
    ///    - DI1: 数据类型（01=电压，02=电流，03=有功，04=无功，05=功率因数，
    ///                     06=电能，07=四象限，08=需量）
    ///    - DI0: 通道（00=总，01=A，02=B，03=C，FF=数据块）
    ///    - ⚠️ 限制：协议里这种寻址方式本身不带类别维度，这里固定取第1类
    ///      负荷记录的序列（见 `handle_load_profile_curve_read` 内注释）
    ///
    /// 参数：
    /// - di: 数据标识 [DI0, DI1, DI2, DI3]
    /// - state: 电表状态
    /// - address: 电表地址
    /// - db_pool: 数据库连接池
    /// - time_param: 给定时间参数（仅第一类DI0=01需要，6字节BCD：YYMMDDhhmm）
    /// - start_time: 查询起始时间（第二类用）
    /// - end_time: 查询结束时间（第二类用）
    /// - max_records: 最大返回记录数（第二类用，默认100）
    pub async fn handle_load_profile_read_async(
        &self,
        di: [u8; 4],
        state: &MeterState,
        address: &str,
        db_pool: &sqlx::SqlitePool,
        time_param: Option<&[u8]>,
        start_time: &chrono::DateTime<chrono::Utc>,
        end_time: &chrono::DateTime<chrono::Utc>,
        max_records: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let di0 = di[0];
        let di1 = di[1];
        let di2 = di[2];

        if di2 == 0x10 {
            // 第二类：曲线数据读取（06-10-DI1-DI0）
            self.handle_load_profile_curve_read(
                di1,
                di0,
                address,
                db_pool,
                start_time,
                end_time,
                max_records.unwrap_or(100),
            )
            .await
        } else {
            // 第一类：记录块读取（06-DI2-00-DI0）
            if di1 != 0x00 {
                return Err(format!(
                    "第一类负荷记录DI1必须为00，实际为：{:02X}",
                    di1
                ));
            }
            self.handle_load_profile_block_read(di2, di0, address, db_pool, time_param)
                .await
        }
    }

    /// 第一类负荷记录：记录块读取（06-DI2-00-DI0）
    ///
    /// 返回附录B格式：A0A0 + 字节数 + 时间5B + 选通块 + AA分隔 + 校验 + E5
    async fn handle_load_profile_block_read(
        &self,
        di2: u8,
        di0: u8,
        address: &str,
        db_pool: &sqlx::SqlitePool,
        time_param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        use crate::persistence::worker::PersistenceWorker;

        // DI2: 00=全部类（暂不支持），01~06=第1~6类
        if di2 == 0x00 {
            return Err("暂不支持06-00-00-xx（全部类负荷记录）".to_string());
        }
        if !(1..=6).contains(&di2) {
            return Err(format!("无效的负荷记录类别：DI2={:02X}（期望01-06）", di2));
        }

        let class_id = di2;

        // 根据DI0查询记录
        let rows = match di0 {
            0x00 => {
                // 最早记录块：ORDER BY sample_time_ms ASC LIMIT 1
                PersistenceWorker::query_load_records_earliest(db_pool, address, class_id).await
            }
            0x01 => {
                // 给定时间记录块：需要time_param（6字节BCD：YYMMDDhhmm）
                let time_bytes = time_param
                    .ok_or_else(|| "给定时间记录块（DI0=01）缺少时间参数".to_string())?;
                if time_bytes.len() < 5 {
                    return Err(format!(
                        "给定时间参数长度不足：期望5字节，实际{}字节",
                        time_bytes.len()
                    ));
                }
                // 解析YYMMDDhhmm（前5字节BCD）
                let target_time = decode_bcd_datetime_ymd_hm(time_bytes)?;
                PersistenceWorker::query_load_records_at_time(
                    db_pool,
                    address,
                    class_id,
                    &target_time,
                )
                .await
            }
            0x02 => {
                // 最近一个记录块：ORDER BY sample_time_ms DESC LIMIT 1
                PersistenceWorker::query_load_records_latest(db_pool, address, class_id).await
            }
            _ => {
                return Err(format!(
                    "无效的第一类负荷记录DI0：{:02X}（期望00/01/02）",
                    di0
                ))
            }
        }
        .map_err(|e| format!("数据库查询失败: {}", e))?;

        if rows.is_empty() {
            return Err("未找到负荷记录数据".to_string());
        }

        // 编码为附录B格式（只返回第一条记录）
        let row = &rows[0];
        let data: LoadRecordData = serde_json::from_value(row.payload.clone())
            .map_err(|e| format!("负荷记录数据反序列化失败: {}", e))?;

        self.encode_load_record_block(&row.sample_time, &data)
    }

    /// 第二类负荷记录：曲线数据读取（06-10-DI1-DI0）
    ///
    /// 返回格式：记录数(1B) + 起始时间(6B) + DI(4B) + Σ[时间(6B)+数据]
    async fn handle_load_profile_curve_read(
        &self,
        di1: u8,
        di0: u8,
        address: &str,
        db_pool: &sqlx::SqlitePool,
        start_time: &chrono::DateTime<chrono::Utc>,
        end_time: &chrono::DateTime<chrono::Utc>,
        max_records: u32,
    ) -> Result<Vec<u8>, String> {
        use crate::persistence::worker::PersistenceWorker;

        if di0 == 0xFF {
            return Err("暂不支持数据块读取（DI0=FF）".to_string());
        }

        // 查询time范围内的所有记录
        //
        // 曲线查询（06-10-DI1-DI0）在协议里没有类别维度，这里约定固定读
        // 第1类负荷记录的序列——避免把多个类别（可能各自采样间隔不同）的
        // 记录交织成一条时间不规则的"曲线"。如果第1类没有配置对应的数据块
        // （模式字未选通），会在下面反序列化后因为字段是 None 而报错，
        // 提示调用方"数据未记录"，而不是静默返回混乱的数据。
        let rows = PersistenceWorker::query_load_records_range(
            db_pool,
            address,
            Some(1),
            start_time,
            end_time,
            max_records,
        )
        .await
        .map_err(|e| format!("数据库查询失败: {}", e))?;

        if rows.is_empty() {
            return Err("未找到负荷记录数据".to_string());
        }

        // 提取特定(DI1,DI0)字段
        let mut data = Vec::new();

        // 记录数(1字节)
        data.push(rows.len().min(255) as u8);

        // 起始时间(6字节BCD)
        data.extend(encode_datetime(&rows[0].sample_time));

        // 数据标识(4字节)：DI0 DI1 DI2 DI3
        data.extend(&[di0, di1, 0x10, 0x06]);

        // 遍历记录，提取(DI1,DI0)对应的字段值
        for row in &rows {
            let record_data: LoadRecordData = serde_json::from_value(row.payload.clone())
                .map_err(|e| format!("负荷记录数据反序列化失败: {}", e))?;

            // 时间(6字节BCD)
            data.extend(encode_datetime(&row.sample_time));

            // 数据值（根据DI1,DI0映射到JSON字段）
            data.extend(self.extract_curve_data_value(di1, di0, &record_data)?);
        }

        Ok(data)
    }

    /// 编码附录B格式的负荷记录块
    ///
    /// 格式：A0A0 + 字节数 + 时间5B + 选通块 + AA分隔 + 校验 + E5
    fn encode_load_record_block(
        &self,
        sample_time: &chrono::DateTime<chrono::Utc>,
        data: &LoadRecordData,
    ) -> Result<Vec<u8>, String> {
        let mut block = Vec::new();

        // A0A0 起始码
        block.extend(&[0xA0, 0xA0]);

        // 先构建数据部分（用于计算字节数）
        let mut payload = Vec::new();

        // 时间5字节（年月日时分）
        payload.extend(encode_datetime_ymd_hm(sample_time));

        // 选通块（按模式字bit顺序）
        let mut has_data = false;

        // bit0: 电压电流频率（17字节）
        if let Some(vif) = &data.vif {
            payload.extend(encode_vif_block(vif)?);
            has_data = true;
        }
        if has_data || data.pq.is_some() {
            payload.push(0xAA); // 块分隔码
        }

        // bit1: 有无功功率（24字节）
        has_data = false;
        if let Some(pq) = &data.pq {
            payload.extend(encode_pq_block(pq)?);
            has_data = true;
        }
        if has_data || data.pf.is_some() {
            payload.push(0xAA);
        }

        // bit2: 功率因数（8字节）
        has_data = false;
        if let Some(pf) = &data.pf {
            payload.extend(encode_pf_block(pf)?);
            has_data = true;
        }
        if has_data || data.energy.is_some() {
            payload.push(0xAA);
        }

        // bit3: 有无功总电能（16字节）
        has_data = false;
        if let Some(energy) = &data.energy {
            payload.extend(encode_energy_block(energy)?);
            has_data = true;
        }
        if has_data || data.quadrant.is_some() {
            payload.push(0xAA);
        }

        // bit4: 四象限无功（16字节）
        has_data = false;
        if let Some(quadrant) = &data.quadrant {
            payload.extend(encode_quadrant_block(quadrant)?);
            has_data = true;
        }
        if has_data || data.demand.is_some() {
            payload.push(0xAA);
        }

        // bit5: 当前需量（6字节）
        if let Some(demand) = &data.demand {
            payload.extend(encode_demand_block(demand)?);
            payload.push(0xAA); // 最后一个块也要AA结束
        }

        // 字节数（不含A0A0和字节数本身，含校验和E5）
        let byte_count = (payload.len() + 2) as u8; // +2 = CS + E5
        block.push(byte_count);

        // 数据部分
        block.extend(&payload);

        // 累加校验码（从第一个A0到最后一个数据字节）
        let checksum: u8 = block.iter().skip(0).fold(0u8, |acc, &b| acc.wrapping_add(b));
        block.push(checksum);

        // E5 结束码
        block.push(0xE5);

        Ok(block)
    }

    /// 从LoadRecordData提取特定(DI1,DI0)对应的字段值
    fn extract_curve_data_value(
        &self,
        di1: u8,
        di0: u8,
        data: &LoadRecordData,
    ) -> Result<Vec<u8>, String> {
        match (di1, di0) {
            // 01: 电压 (XXX.X V, 2字节)
            (0x01, 0x01) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_voltage(vif.voltage_a))
                .ok_or_else(|| "电压数据未记录".to_string()),
            (0x01, 0x02) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_voltage(vif.voltage_b))
                .ok_or_else(|| "电压数据未记录".to_string()),
            (0x01, 0x03) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_voltage(vif.voltage_c))
                .ok_or_else(|| "电压数据未记录".to_string()),

            // 02: 电流 (XXX.XXX A, 3字节)
            (0x02, 0x01) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_current(vif.current_a))
                .ok_or_else(|| "电流数据未记录".to_string()),
            (0x02, 0x02) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_current(vif.current_b))
                .ok_or_else(|| "电流数据未记录".to_string()),
            (0x02, 0x03) => data
                .vif
                .as_ref()
                .map(|vif| encode_bcd_current(vif.current_c))
                .ok_or_else(|| "电流数据未记录".to_string()),

            // 03: 有功功率 (XX.XXXX kW, 3字节)
            (0x03, 0x00) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.active_total))
                .ok_or_else(|| "有功功率数据未记录".to_string()),
            (0x03, 0x01) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.active_a))
                .ok_or_else(|| "有功功率数据未记录".to_string()),
            (0x03, 0x02) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.active_b))
                .ok_or_else(|| "有功功率数据未记录".to_string()),
            (0x03, 0x03) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.active_c))
                .ok_or_else(|| "有功功率数据未记录".to_string()),

            // 04: 无功功率 (XX.XXXX kvar, 3字节)
            (0x04, 0x00) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.reactive_total))
                .ok_or_else(|| "无功功率数据未记录".to_string()),
            (0x04, 0x01) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.reactive_a))
                .ok_or_else(|| "无功功率数据未记录".to_string()),
            (0x04, 0x02) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.reactive_b))
                .ok_or_else(|| "无功功率数据未记录".to_string()),
            (0x04, 0x03) => data
                .pq
                .as_ref()
                .map(|pq| encode_bcd_power(pq.reactive_c))
                .ok_or_else(|| "无功功率数据未记录".to_string()),

            // 05: 功率因数 (X.XXX, 2字节)
            (0x05, 0x00) => data
                .pf
                .as_ref()
                .map(|pf| encode_bcd_power_factor(pf.total))
                .ok_or_else(|| "功率因数数据未记录".to_string()),
            (0x05, 0x01) => data
                .pf
                .as_ref()
                .map(|pf| encode_bcd_power_factor(pf.a))
                .ok_or_else(|| "功率因数数据未记录".to_string()),
            (0x05, 0x02) => data
                .pf
                .as_ref()
                .map(|pf| encode_bcd_power_factor(pf.b))
                .ok_or_else(|| "功率因数数据未记录".to_string()),
            (0x05, 0x03) => data
                .pf
                .as_ref()
                .map(|pf| encode_bcd_power_factor(pf.c))
                .ok_or_else(|| "功率因数数据未记录".to_string()),

            // 06: 电能 (XXXXXX.XX kWh, 4字节)
            (0x06, 0x01) => data
                .energy
                .as_ref()
                .map(|e| encode_bcd_energy(e.forward_active))
                .ok_or_else(|| "电能数据未记录".to_string()),
            (0x06, 0x02) => data
                .energy
                .as_ref()
                .map(|e| encode_bcd_energy(e.reverse_active))
                .ok_or_else(|| "电能数据未记录".to_string()),
            (0x06, 0x03) => data
                .energy
                .as_ref()
                .map(|e| encode_bcd_energy(e.combined_reactive1))
                .ok_or_else(|| "电能数据未记录".to_string()),
            (0x06, 0x04) => data
                .energy
                .as_ref()
                .map(|e| encode_bcd_energy(e.combined_reactive2))
                .ok_or_else(|| "电能数据未记录".to_string()),

            // 07: 四象限 (XXXXXX.XX kvarh, 4字节)
            (0x07, 0x01) => data
                .quadrant
                .as_ref()
                .map(|q| encode_bcd_energy(q.q1))
                .ok_or_else(|| "四象限数据未记录".to_string()),
            (0x07, 0x02) => data
                .quadrant
                .as_ref()
                .map(|q| encode_bcd_energy(q.q2))
                .ok_or_else(|| "四象限数据未记录".to_string()),
            (0x07, 0x03) => data
                .quadrant
                .as_ref()
                .map(|q| encode_bcd_energy(q.q3))
                .ok_or_else(|| "四象限数据未记录".to_string()),
            (0x07, 0x04) => data
                .quadrant
                .as_ref()
                .map(|q| encode_bcd_energy(q.q4))
                .ok_or_else(|| "四象限数据未记录".to_string()),

            // 08: 需量 (XX.XXXX kW/kvar, 3字节)
            (0x08, 0x01) => data
                .demand
                .as_ref()
                .map(|d| encode_bcd_power(d.active))
                .ok_or_else(|| "需量数据未记录".to_string()),
            (0x08, 0x02) => data
                .demand
                .as_ref()
                .map(|d| encode_bcd_power(d.reactive))
                .ok_or_else(|| "需量数据未记录".to_string()),

            _ => Err(format!(
                "不支持的负荷记录数据项：DI1={:02X}, DI0={:02X}",
                di1, di0
            )),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 附录B块编码函数
// ═══════════════════════════════════════════════════════════════════════════

/// B.2.1 电压电流频率块（17字节）
fn encode_vif_block(vif: &VifBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(17);
    // 电压 3×2字节（XXX.X V）
    data.extend(encode_bcd_voltage(vif.voltage_a));
    data.extend(encode_bcd_voltage(vif.voltage_b));
    data.extend(encode_bcd_voltage(vif.voltage_c));
    // 电流 3×3字节（XXX.XXX A）
    data.extend(encode_bcd_current(vif.current_a));
    data.extend(encode_bcd_current(vif.current_b));
    data.extend(encode_bcd_current(vif.current_c));
    // 频率 2字节（XX.XX Hz）
    data.extend(encode_bcd_frequency(vif.frequency));
    Ok(data)
}

/// B.2.2 有无功功率块（24字节）
fn encode_pq_block(pq: &PqBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(24);
    // 有功功率 4×3字节（XX.XXXX kW）
    data.extend(encode_bcd_power(pq.active_total));
    data.extend(encode_bcd_power(pq.active_a));
    data.extend(encode_bcd_power(pq.active_b));
    data.extend(encode_bcd_power(pq.active_c));
    // 无功功率 4×3字节（XX.XXXX kvar）
    data.extend(encode_bcd_power(pq.reactive_total));
    data.extend(encode_bcd_power(pq.reactive_a));
    data.extend(encode_bcd_power(pq.reactive_b));
    data.extend(encode_bcd_power(pq.reactive_c));
    Ok(data)
}

/// B.2.3 功率因数块（8字节）
fn encode_pf_block(pf: &PfBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(8);
    // 功率因数 4×2字节（X.XXX）
    data.extend(encode_bcd_power_factor(pf.total));
    data.extend(encode_bcd_power_factor(pf.a));
    data.extend(encode_bcd_power_factor(pf.b));
    data.extend(encode_bcd_power_factor(pf.c));
    Ok(data)
}

/// B.2.4 有无功总电能块（16字节）
fn encode_energy_block(energy: &EnergyBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(16);
    // 电能 4×4字节（XXXXXX.XX kWh）
    data.extend(encode_bcd_energy(energy.forward_active));
    data.extend(encode_bcd_energy(energy.reverse_active));
    data.extend(encode_bcd_energy(energy.combined_reactive1));
    data.extend(encode_bcd_energy(energy.combined_reactive2));
    Ok(data)
}

/// B.2.5 四象限无功块（16字节）
fn encode_quadrant_block(quadrant: &QuadrantBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(16);
    // 四象限 4×4字节（XXXXXX.XX kvarh）
    data.extend(encode_bcd_energy(quadrant.q1));
    data.extend(encode_bcd_energy(quadrant.q2));
    data.extend(encode_bcd_energy(quadrant.q3));
    data.extend(encode_bcd_energy(quadrant.q4));
    Ok(data)
}

/// B.2.6 当前需量块（6字节）
fn encode_demand_block(demand: &DemandBlock) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(6);
    // 需量 2×3字节（XX.XXXX kW/kvar）
    data.extend(encode_bcd_power(demand.active));
    data.extend(encode_bcd_power(demand.reactive));
    Ok(data)
}

/// 编码频率（XX.XX Hz，2字节BCD）
fn encode_bcd_frequency(freq: f64) -> Vec<u8> {
    encode_bcd(freq, 2, 2)
}

/// 编码时间（YYMMDDhhmm，5字节BCD）
fn encode_datetime_ymd_hm(dt: &chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    let yy = (dt.year() % 100) as u8;
    let mm = dt.month() as u8;
    let dd = dt.day() as u8;
    let hh = dt.hour() as u8;
    let minute = dt.minute() as u8;
    vec![
        ((yy / 10) << 4) | (yy % 10),
        ((mm / 10) << 4) | (mm % 10),
        ((dd / 10) << 4) | (dd % 10),
        ((hh / 10) << 4) | (hh % 10),
        ((minute / 10) << 4) | (minute % 10),
    ]
}

/// 解码时间（YYMMDDhhmm，5字节BCD）
fn decode_bcd_datetime_ymd_hm(bcd: &[u8]) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if bcd.len() < 5 {
        return Err(format!("时间参数长度不足：期望5字节，实际{}字节", bcd.len()));
    }
    let yy = ((bcd[0] >> 4) * 10 + (bcd[0] & 0x0F)) as i32 + 2000;
    let mm = ((bcd[1] >> 4) * 10 + (bcd[1] & 0x0F)) as u32;
    let dd = ((bcd[2] >> 4) * 10 + (bcd[2] & 0x0F)) as u32;
    let hh = ((bcd[3] >> 4) * 10 + (bcd[3] & 0x0F)) as u32;
    let minute = ((bcd[4] >> 4) * 10 + (bcd[4] & 0x0F)) as u32;

    chrono::NaiveDate::from_ymd_opt(yy, mm, dd)
        .and_then(|date| date.and_hms_opt(hh, minute, 0))
        .and_then(|dt| Some(chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)))
        .ok_or_else(|| format!("无效的日期时间：{}-{}-{} {}:{}", yy, mm, dd, hh, minute))
}