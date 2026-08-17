// 表A.4 事件记录读取（DI3=03）

use super::encoding::*;
use super::MeterState;
use crate::simulation::di_handler::DIHandler;

impl DIHandler {
    /// 处理事件记录读取（DI3=03）
    ///
    /// 事件记录格式：03-DI2-DI1-DI0
    /// - DI2：事件类型
    /// - DI1：事件子类型
    /// - DI0：发生次数索引（00=汇总，01-0A=明细）
    ///
    /// 返回：BCD编码的事件数据
    pub(super) fn handle_event_record_read(
        &self,
        di: [u8; 4],
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        let di0 = di[0]; // 发生次数索引
        let di1 = di[1]; // 事件子类型
        let di2 = di[2]; // 事件类型

        if di0 == 0x00 {
            // DI0=00：读取事件汇总信息（总次数+总时长）
            self.read_event_summary(di2, di1, state)
        } else if di0 >= 0x01 && di0 <= 0x0A {
            // DI0=01-0A：读取事件明细记录
            self.read_event_detail(di2, di1, di0, state)
        } else {
            Err(format!("无效的事件记录索引：DI0={:02X}（期望00-0A）", di0))
        }
    }

    /// 读取事件汇总信息（DI0=00）
    ///
    /// 返回格式（附录A.4）：
    /// - 总次数（3字节BCD，XXXXXX 次）
    /// - 总累计时间（3字节BCD，XXXXXX 分钟）
    fn read_event_summary(
        &self,
        event_type: u8,
        sub_type: u8,
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        let summary = state
            .get_event_summary(event_type, sub_type)
            .ok_or_else(|| format!("事件汇总不存在：{:02X}-{:02X}", event_type, sub_type))?;

        let mut data = Vec::new();

        // 总次数（3字节BCD）
        let count = summary.total_count.min(999999);
        data.push(to_bcd((count % 100) as u8));
        data.push(to_bcd(((count / 100) % 100) as u8));
        data.push(to_bcd(((count / 10000) % 100) as u8));

        // 总累计时间（3字节BCD，分钟）
        let duration = summary.total_duration_minutes.min(999999);
        data.push(to_bcd((duration % 100) as u8));
        data.push(to_bcd(((duration / 100) % 100) as u8));
        data.push(to_bcd(((duration / 10000) % 100) as u8));

        Ok(data)
    }

    /// 读取事件明细记录（DI0=01-0A）
    ///
    /// 返回格式：
    /// - 发生时间（6字节BCD：秒分时日月年）
    /// - 结束时间（6字节BCD，故障类事件有效）
    /// - 事件数据（变长，取决于事件类型）
    fn read_event_detail(
        &self,
        event_type: u8,
        sub_type: u8,
        occurrence_idx: u8,
        state: &MeterState,
    ) -> Result<Vec<u8>, String> {
        let record = state
            .get_event_record(event_type, sub_type, occurrence_idx)
            .ok_or_else(|| {
                format!(
                    "事件记录不存在：{:02X}-{:02X}-{:02X}",
                    event_type, sub_type, occurrence_idx
                )
            })?;

        let mut data = Vec::new();

        // 发生时间（6字节BCD）
        data.extend(encode_datetime(&record.start_time));

        // 结束时间（6字节BCD，故障类事件有效；编程记录为00 00 00 00 00 00）
        if let Some(end_time) = record.end_time {
            data.extend(encode_datetime(&end_time));
        } else {
            // 无结束时间，填充00
            data.extend(vec![0x00; 6]);
        }

        // 事件数据：故障类（DI2=01~0F）为附录A.4定义的119字节尾段；
        // 其余类型（编程/校时/清零等）为变长自定义数据
        if (0x01..=0x0F).contains(&event_type) {
            let mut tail = record.data.clone();
            tail.resize(119, 0);
            data.extend(tail);
        } else {
            data.extend(&record.data);
        }

        Ok(data)
    }
}
