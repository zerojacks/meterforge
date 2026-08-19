// 虚拟电表 - 完整的 DL/T645 虚拟电表实现

use super::di_handler::DIHandler;
use super::physics_engine::{PhysicsConfig, PhysicsEngine, SimulationConfig};
use super::state::MeterState;
use crate::persistence::{PersistRequest, PersistedMeterSettings};
use tokio::sync::mpsc;
use tracing::warn;

/// 虚拟电表
///
/// 架构（按设计方案 4.6 节）：
/// - VirtualMeter：持有 MeterState + PhysicsEngine + DIHandler
/// - MeterState：电表的所有数据（电能、瞬时量、参数等）
/// - PhysicsEngine：推进仿真，更新 MeterState
/// - DIHandler：纯映射层，读取 MeterState 进行 BCD 编码
///
/// 持久化集成（按设计方案 4.8 节）：
/// - 持有可选的 persist_tx 发送器
/// - 冻结快照生成后立即提交到 PersistenceWorker
/// - 电能寄存器按策略定期 flush
pub struct VirtualMeter {
    /// 电表状态（所有数据）
    state: MeterState,

    /// 物理仿真引擎（更新 state）
    physics: PhysicsEngine,

    /// DI 数据处理器（纯映射层，无状态）
    handler: DIHandler,

    /// 电表配置
    config: VirtualMeterConfig,

    /// 持久化请求发送器（可选）
    persist_tx: Option<mpsc::Sender<PersistRequest>>,

    /// 上次电能 flush 时间
    last_energy_flush: std::time::Instant,

    /// 上次 flush 时的电能值（用于检测增量）
    last_flushed_energy: f64,
}

impl VirtualMeter {
    /// 创建新的虚拟电表
    pub fn new(config: VirtualMeterConfig) -> Self {
        let mut state = MeterState::default();
        state.address = config.address;

        let physics = PhysicsEngine::new(config.physics_config.load_model.clone());
        let handler = DIHandler::new();

        Self {
            state,
            physics,
            handler,
            config,
            persist_tx: None,
            last_energy_flush: std::time::Instant::now(),
            last_flushed_energy: 0.0,
        }
    }

    /// 创建带持久化支持的虚拟电表
    pub fn with_persistence(
        config: VirtualMeterConfig,
        persist_tx: mpsc::Sender<PersistRequest>,
    ) -> Self {
        let mut meter = Self::new(config);
        meter.persist_tx = Some(persist_tx);
        meter
    }

    /// 从数据库恢复状态（用于启动时加载）
    ///
    /// 参数：
    /// - pool: 数据库连接池
    /// - address: 电表地址字符串
    ///
    /// 返回：是否成功恢复（如果数据库中没有此地址的数据，返回Ok(false)）
    pub async fn restore_from_database(&mut self, pool: &sqlx::SqlitePool) -> Result<bool, String> {
        use crate::persistence::PersistenceWorker;

        let address_str = address_to_string(&self.state.address);

        // 1. 恢复虚拟时钟
        match PersistenceWorker::restore_virtual_time(pool, &address_str).await {
            Ok(Some(virtual_time)) => {
                self.state.restore_virtual_time(virtual_time);
                println!(
                    "[VirtualMeter] Restored virtual_time: {}",
                    virtual_time.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S UTC")
                );
            }
            Ok(None) => {
                // 数据库中没有此地址的数据，这是正常的（首次启动）
                return Ok(false);
            }
            Err(e) => {
                return Err(format!("Failed to restore virtual_time: {}", e));
            }
        }

        // 2. 恢复电能寄存器
        match PersistenceWorker::restore_energy_registers(pool, &address_str).await {
            Ok(registers) => {
                if !registers.is_empty() {
                    self.state.restore_energy_registers(registers.clone());
                    println!(
                        "[VirtualMeter] Restored {} energy registers",
                        registers.len()
                    );

                    // 更新last_flushed_energy为恢复的值
                    self.last_flushed_energy = self
                        .state
                        .energy_registers
                        .get(&(super::state::EnergyType::ForwardActive, 0))
                        .copied()
                        .unwrap_or(0.0);
                }
            }
            Err(e) => {
                return Err(format!("Failed to restore energy_registers: {}", e));
            }
        }

        // 3. 恢复表配置（仿真参数 / 冻结模式 / 结算日 / 负荷记录配置）
        //
        // 对应 admin 命令实时下发时写的那份数据（save_simulation_config /
        // save_freeze_config / save_settlement_days / save_load_record_config），
        // 之前只在 UI 里改的时候写进去，从没在启动时读出来过，所以重启后
        // UI 上看到的一直是代码里的默认值，跟数据库里存的对不上。
        match PersistenceWorker::restore_meter_config(pool, &address_str).await {
            Ok(Some(settings)) => {
                self.apply_persisted_settings(&settings);
                println!("[VirtualMeter] Restored meter config for {}", address_str);
            }
            Ok(None) => {
                // 没有历史配置记录，保留当前（默认）配置即可
            }
            Err(e) => {
                return Err(format!("Failed to restore meter config: {}", e));
            }
        }

        // 4. 恢复负荷记录采样状态（每个类别的最后一次采样时间）
        //
        // 防止重启后重复采样：如果重启前刚采样过 12:15，重启后在 12:16
        // 不应该再次采样，应该等到 12:30。
        match PersistenceWorker::restore_last_sample_times(pool, &address_str).await {
            Ok(last_sample_times) => {
                let non_empty_count = last_sample_times.iter().filter(|t| t.is_some()).count();
                if non_empty_count > 0 {
                    self.state.load_profile_state = 
                        super::state::LoadProfileSamplingState::from_last_sample_times(last_sample_times);
                    println!(
                        "[VirtualMeter] Restored last sample times for {} class(es)",
                        non_empty_count
                    );
                }
            }
            Err(e) => {
                // 非致命错误，只记录警告，不阻止启动
                eprintln!("Warning: Failed to restore last sample times: {}", e);
            }
        }

        Ok(true)
    }

    /// 把从数据库读回的 [`PersistedMeterSettings`] 应用到当前电表状态。
    ///
    /// 字段和写入逻辑与 `MeterActor::on_admin_command` 里
    /// `ApplyFreezeConfig` / `ApplySettlementDays` / `ApplyLoadRecordConfig`
    /// 分支实时应用时完全一致——这里只是把上次持久化的值灌回内存，
    /// 不会再触发一次持久化写入。
    pub fn apply_persisted_settings(&mut self, settings: &PersistedMeterSettings) {
        if let Err(e) = self.apply_simulation_config(settings.simulation.clone()) {
            warn!("[VirtualMeter] 恢复仿真配置失败，保留默认值: {}", e);
        }

        let state = self.state_mut();
        state.freeze_config.timed_freeze_mode = settings.timed_freeze_mode;
        state.freeze_config.instant_freeze_mode = settings.instant_freeze_mode;
        state.freeze_config.appointment_freeze_mode = settings.appointment_freeze_mode;
        state.hourly_freeze_mode = settings.hourly_freeze_mode;
        state.daily_freeze_mode = settings.daily_freeze_mode;
        state.daily_freeze_time = settings.daily_freeze_time;
        state.hourly_freeze_start = settings.hourly_freeze_start;
        state.hourly_freeze_interval_min = settings.hourly_freeze_interval_min;
        state.appointment_freeze_time = settings.appointment_freeze_time;
        // 恢复的约定冻结时间允许重新触发一次
        state.appointment_freeze_fired = false;

        state.settlement_days = settings.settlement_days;
        state.settlement_hours = settings.settlement_hours;

        state.load_record_config.mode_word = settings.load_record_mode_word;
        state.load_record_start_time = settings.load_record_start_time;
        state.load_record_config.intervals = settings.load_record_intervals;
    }

    /// 创建默认虚拟电表
    pub fn default() -> Self {
        Self::new(VirtualMeterConfig::default())
    }

    /// 处理 DL/T645 读数据命令（同步，仅内存，供测试/示例使用）
    ///
    /// DIHandler 只接收 MeterState 引用，不接收 PhysicsEngine
    pub fn handle_read_command(&mut self, di: [u8; 4]) -> Result<Vec<u8>, String> {
        self.handler.handle_read(di, &self.state)
    }

    /// 处理 DL/T645 读数据命令（异步，支持数据库历史查询）
    ///
    /// 按设计 4.5.2.3 / §12 路由：
    /// - DI3=06 负荷记录：解析时间范围，查 `load_profile_records` 表
    /// - DI3=05 冻结数据且 DI0 超过内存环形缓冲容量：查 `freeze_snapshots` 表
    /// - 其余：内存同步读取
    pub async fn handle_read_async(
        &mut self,
        data: &[u8],
        db_pool: Option<&sqlx::SqlitePool>,
    ) -> Result<Vec<u8>, String> {
        if data.len() < 4 {
            return Err("读命令数据长度不足".to_string());
        }

        let di = [data[0], data[1], data[2], data[3]];
        let rest = &data[4..];
        let address = address_to_string(&self.state.address);

        // 负荷记录（DI3=06）
        if di[3] == 0x06 {
            let pool = db_pool.ok_or_else(|| "负荷记录查询需要数据库支持".to_string())?;
            
            // 判断是第一类（06-DI2-00-DI0）还是第二类（06-10-DI1-DI0）
            if di[2] == 0x10 {
                // 第二类：曲线数据读取，需要时间范围（10字节）
                if rest.len() != 10 {
                    return Err(format!(
                        "第二类负荷记录时间范围长度错误：期望10字节，实际{}字节",
                        rest.len()
                    ));
                }
                let start_time = parse_load_profile_time(&rest[0..5])?;
                let end_time = parse_load_profile_time(&rest[5..10])?;
                return self
                    .handler
                    .handle_load_profile_read_async(
                        di,
                        &self.state,
                        &address,
                        pool,
                        None, // time_param
                        &start_time,
                        &end_time,
                        None, // max_records
                    )
                    .await;
            } else {
                // 第一类：记录块读取
                // DI0=00（最早）或02（最近）：不需要额外参数
                // DI0=01（给定时间）：需要5字节BCD时间参数
                let time_param = if di[0] == 0x01 {
                    if rest.len() < 5 {
                        return Err(format!(
                            "给定时间记录块参数长度不足：期望至少5字节，实际{}字节",
                            rest.len()
                        ));
                    }
                    Some(&rest[0..5])
                } else {
                    None
                };
                
                // 第一类不需要时间范围，使用虚拟时间作为占位
                let now = self.state.virtual_time;
                return self
                    .handler
                    .handle_load_profile_read_async(
                        di,
                        &self.state,
                        &address,
                        pool,
                        time_param,
                        &now,
                        &now,
                        None,
                    )
                    .await;
            }
        }

        // 冻结数据（DI3=05）且 DI0 超过内存环形缓冲容量
        if di[3] == 0x05 {
            if let Some(trigger) = super::state::FreezeTrigger::from_di2(di[2]) {
                if di[0] > trigger.max_history_count() {
                    let pool = db_pool.ok_or_else(|| "历史冻结查询需要数据库支持".to_string())?;
                    return self
                        .handler
                        .handle_freeze_data_read_async(di, &self.state, &address, pool)
                        .await;
                }
            }
        }

        // 其余：内存同步读取
        self.handler.handle_read(di, &self.state)
    }

    /// 处理 DL/T645 写数据命令
    ///
    /// 按设计方案 4.5.2 节实现：
    /// DATA格式：DI(4) + 密码(4) + 操作者代码(4) + 数据
    ///
    /// 架构说明：
    /// - VirtualMeter负责密码验证、权限检查、数据写入
    /// - MeterActor只负责帧格式解析和路由
    ///
    /// 参数：
    /// - data: 完整的写命令DATA字段（DI+密码+操作者代码+数据，至少13字节）
    ///
    /// 返回：成功返回Ok(操作者代码)，失败返回错误信息
    pub fn handle_write_command(&mut self, data: &[u8]) -> Result<[u8; 4], String> {
        // 最小长度检查：DI(4) + 密码(4) + 操作者代码(4) + 数据(至少1字节) = 13字节
        if data.len() < 13 {
            return Err(format!(
                "写命令数据长度不足：期望至少13字节，实际{}字节",
                data.len()
            ));
        }

        // 1. 解析DI
        let di = [data[0], data[1], data[2], data[3]];

        // 2. 解析密码（4字节）
        // PA0: 高半字节=权限等级（0-9，0最高），低半字节+P00P10P20=3.5字节密码
        let pa0 = data[4];
        let level = (pa0 >> 4) & 0x0F; // 高半字节
        let password = [
            data[5], // P00
            data[6], // P10
            data[7], // P20
        ];

        // 3. 解析操作者代码（4字节BCD）
        let operator_code = [data[8], data[9], data[10], data[11]];

        // 4. 数据字段（从第12字节开始）
        let write_data = &data[12..];

        // 5. 密码验证
        if !self.state.password_config.verify(level, &password) {
            return Err(format!("密码验证失败：权限等级={}", level));
        }

        // 6. 权限检查：写参数需要04级权限（level <= 4）
        // 注意：level数值越小权限越高，00=最高权限
        if level > 4 {
            return Err(format!("权限不足：当前等级{}，需要04级或更高权限", level));
        }

        // 7. 检查DI是否可写（只有DI3=04的参变量可写）
        if di[3] != 0x04 {
            return Err(format!(
                "DI {:02X}{:02X}{:02X}{:02X} 不支持写入（只读数据项）",
                di[3], di[2], di[1], di[0]
            ));
        }

        // 8. 根据DI调用DIHandler进行写入
        self.handler.handle_write(di, write_data, &mut self.state)?;

        // 9. 生成事件记录（附录A.4）
        // 写日期/时间参数（04-00-01-01/02）属于校时，生成 03-31 校时记录；
        // 其余写操作生成 03-30 编程记录，子类型按写入的 DI 区分
        if di[2] == 0x00 && di[1] == 0x01 && matches!(di[0], 0x01 | 0x02) {
            self.state.add_event_record(
                0x31, // 校时记录
                0x02, // 写参数校时
                self.state.virtual_time,
                operator_code.to_vec(),
            );
        } else {
            let event_sub_type = match di[2] {
                0x02 | 0x03 => 0x0F, // 费率时段表 -> 费率参数表编程
                0x04 => 0x10,        // 电表运行参数 -> 其他编程
                0x06 => 0x0F,        // 组合方式 -> 费率参数表编程
                _ => 0x10,           // 其他 -> 其他编程
            };
            self.state.add_event_record(
                0x30, // 编程记录
                event_sub_type,
                self.state.virtual_time,
                operator_code.to_vec(),
            );
        }

        // 成功：返回操作者代码
        Ok(operator_code)
    }

    /// 处理冻结命令（16H）
    ///
    /// DATA格式：mm hh DD MM（4字节BCD，分时日月）
    /// 特殊模式：
    /// - 99 DD hh mm = 按月周期冻结
    /// - 99 99 hh mm = 按日周期冻结
    /// - 99 99 99 mm = 按小时周期冻结
    /// - 99 99 99 99 = 瞬时冻结
    ///
    /// 返回：Ok(()) 表示成功，Err(错误信息) 表示失败
    pub fn handle_freeze_command(&mut self, data: &[u8]) -> Result<(), String> {
        use crate::protocol::format::bcd_to_u64;

        if data.len() != 4 {
            return Err(format!(
                "冻结命令数据长度错误：{} 字节（期望4字节）",
                data.len()
            ));
        }

        // 解析时间参数（BCD格式）
        let mm = bcd_to_u64(&data[0..1]).map_err(|e| format!("解析分钟失败: {}", e))? as u32;
        let hh = bcd_to_u64(&data[1..2]).map_err(|e| format!("解析小时失败: {}", e))? as u32;
        let dd = bcd_to_u64(&data[2..3]).map_err(|e| format!("解析日期失败: {}", e))? as u32;
        let month = bcd_to_u64(&data[3..4]).map_err(|e| format!("解析月份失败: {}", e))? as u32;

        // 判断冻结类型并立即执行
        if mm == 99 && hh == 99 && dd == 99 && month == 99 {
            // 瞬时冻结：立即生成快照
            self.state
                .create_freeze_snapshot(super::state::FreezeTrigger::Instant);
            Ok(())
        } else if hh == 99 && dd == 99 && month == 99 {
            // 按小时周期冻结：配置模式字
            self.state.freeze_config.timed_freeze_mode = 3; // 小时周期
            Ok(())
        } else if dd == 99 && month == 99 {
            // 按日周期冻结：配置模式字
            self.state.freeze_config.timed_freeze_mode = 2; // 日周期
            Ok(())
        } else if month == 99 {
            // 按月周期冻结：配置模式字
            self.state.freeze_config.timed_freeze_mode = 1; // 月周期
            Ok(())
        } else {
            // 定时冻结：在指定时间点执行（保存到约定冻结配置）
            // TODO: 将时间参数保存到freeze_config中，由check_freeze_schedule检测触发
            self.state.freeze_config.appointment_freeze_mode = 1;
            Ok(())
        }
    }

    /// 处理更改通信速率命令（17H）
    ///
    /// DATA格式：1字节通信速率特征字
    /// 返回：Ok(()) 表示成功，Err(错误信息) 表示失败
    ///
    /// 注意：当前实现为"仿真模式"，只修改状态标记，不真实切换串口参数
    pub fn handle_change_baudrate_command(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() != 1 {
            return Err(format!(
                "更改速率命令数据长度错误：{} 字节（期望1字节）",
                data.len()
            ));
        }

        let baud_code = data[0];

        // 验证速率代码有效性（根据DL/T645-2007协议）
        // 01=600bps, 02=1200bps, 04=2400bps, 08=4800bps, 10=9600bps, 20=19200bps
        match baud_code {
            0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 => {
                self.state.baudrate = baud_code;
                Ok(())
            }
            _ => Err(format!("无效的波特率代码: 0x{:02X}", baud_code)),
        }
    }

    /// 处理修改密码命令（18H）
    ///
    /// DATA格式（12字节）：
    /// - DI0 DI1 DI2 DI3 (4字节，固定为 04 00 02 01-0A)
    /// - PA0 P00 P10 P20 (4字节，旧密码)
    /// - PAN P0N P1N P2N (4字节，新密码)
    ///
    /// PA0/PAN高半字节=权限等级（00-09）
    ///
    /// 返回：Ok(()) 表示成功，Err(错误信息) 表示失败
    pub fn handle_change_password_command(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() != 12 {
            return Err(format!(
                "修改密码命令数据长度错误：{} 字节（期望12字节）",
                data.len()
            ));
        }

        // 解析DI（验证是否为密码参数DI）
        let di = [data[0], data[1], data[2], data[3]];
        if di[3] != 0x04 || di[2] != 0x00 || di[1] != 0x02 {
            return Err(format!(
                "无效的DI码：{:02X}{:02X}{:02X}{:02X}（期望04-00-02-XX）",
                di[3], di[2], di[1], di[0]
            ));
        }

        // DI0指定密码等级（01-0A对应等级1-10，实际存储为0-9）
        if di[0] < 0x01 || di[0] > 0x0A {
            return Err(format!("无效的密码等级：{:02X}（期望01-0A）", di[0]));
        }
        let password_level = di[0] - 1; // 转换为0-9

        // 解析旧密码
        let old_pa0 = data[4];
        let old_level = (old_pa0 >> 4) & 0x0F;
        let old_password = [data[5], data[6], data[7]];

        // 验证旧密码
        if !self.state.password_config.verify(old_level, &old_password) {
            return Err("旧密码错误".to_string());
        }

        // 验证权限：只能修改自己权限或更低权限的密码
        if old_level > password_level {
            return Err(format!(
                "权限不足：当前权限等级{}，不能修改权限等级{}的密码",
                old_level, password_level
            ));
        }

        // 解析新密码
        let new_pa0 = data[8];
        let new_level = (new_pa0 >> 4) & 0x0F;
        let new_password = [data[9], data[10], data[11]];

        // 验证新密码等级与DI0一致
        if new_level != password_level {
            return Err(format!(
                "新密码等级{}与DI0指定的等级{}不一致",
                new_level, password_level
            ));
        }

        // 设置新密码
        self.state
            .password_config
            .set_password(password_level, &new_password);

        // 生成编程记录事件（03 30 12 01 密钥更新记录）
        self.state.add_event_record(
            0x30, // 编程记录
            0x12, // 密钥更新
            chrono::Utc::now(),
            vec![password_level], // 记录修改的密码等级
        );

        Ok(())
    }

    /// 处理需量清零命令（19H）
    ///
    /// 清零最大需量寄存器及发生时间，不影响电能量累计
    /// 需要04级权限
    ///
    /// DATA格式：DI(4) + 密码(4) + 操作者代码(4) = 12字节
    /// 返回：Ok(操作者代码) 或 Err(错误信息)
    pub fn handle_demand_clear_command(&mut self, data: &[u8]) -> Result<[u8; 4], String> {
        if data.len() != 12 {
            return Err(format!(
                "需量清零命令数据长度错误：{} 字节（期望12字节）",
                data.len()
            ));
        }

        // 解析密码和操作者代码
        let _di = [data[0], data[1], data[2], data[3]];
        let pa0 = data[4];
        let password = [data[5], data[6], data[7]];
        let operator_code = [data[8], data[9], data[10], data[11]];

        // 验证密码（需要04级权限）
        let level = (pa0 >> 4) & 0x0F;
        if !self.state.password_config.verify(level, &password) {
            return Err("密码错误".to_string());
        }

        // 验证权限：需要04级或更高
        if level > 4 {
            return Err(format!("权限不足：需要04级权限，当前权限等级{}", level));
        }

        // 清零最大需量寄存器
        self.state.max_demand = 0.0;
        self.state.max_demand_time = chrono::Utc::now();

        // 生成编程记录事件
        self.state.add_event_record(
            0x30, // 编程记录
            0x19, // 需量清零
            chrono::Utc::now(),
            operator_code.to_vec(),
        );

        Ok(operator_code)
    }

    /// 处理电表清零命令（1AH）
    ///
    /// 重置电能寄存器、最大需量为0，但保留事件记录、冻结历史、参量配置
    /// 需要02级权限
    ///
    /// DATA格式：密码(4) + 操作者代码(4) = 8字节
    /// 返回：Ok(操作者代码) 或 Err(错误信息)
    pub fn handle_meter_clear_command(&mut self, data: &[u8]) -> Result<[u8; 4], String> {
        if data.len() != 8 {
            return Err(format!(
                "电表清零命令数据长度错误：{} 字节（期望8字节）",
                data.len()
            ));
        }

        // 解析密码和操作者代码
        let pa0 = data[0];
        let password = [data[1], data[2], data[3]];
        let operator_code = [data[4], data[5], data[6], data[7]];

        // 验证密码（需要02级权限）
        let level = (pa0 >> 4) & 0x0F;
        if !self.state.password_config.verify(level, &password) {
            return Err("密码错误".to_string());
        }

        // 验证权限：需要02级或更高
        if level > 2 {
            return Err(format!("权限不足：需要02级权限，当前权限等级{}", level));
        }

        // 清零电能寄存器
        self.state.energy_registers.clear();

        // 清零最大需量
        self.state.max_demand = 0.0;
        self.state.max_demand_time = chrono::Utc::now();

        // 生成编程记录事件（03 32 01 01）
        self.state.add_event_record(
            0x32, // 清零记录
            0x01, // 电表清零
            chrono::Utc::now(),
            operator_code.to_vec(),
        );

        Ok(operator_code)
    }

    /// 处理事件清零命令（1BH）
    ///
    /// 清空事件记录明细与计数，但操作本身要追加一条编程记录日志
    /// 需要02级权限
    ///
    /// DATA格式：密码(4) + 操作者代码(4) = 8字节
    /// 返回：Ok(操作者代码) 或 Err(错误信息)
    pub fn handle_event_clear_command(&mut self, data: &[u8]) -> Result<[u8; 4], String> {
        if data.len() != 8 {
            return Err(format!(
                "事件清零命令数据长度错误：{} 字节（期望8字节）",
                data.len()
            ));
        }

        // 解析密码和操作者代码
        let pa0 = data[0];
        let password = [data[1], data[2], data[3]];
        let operator_code = [data[4], data[5], data[6], data[7]];

        // 验证密码（需要02级权限）
        let level = (pa0 >> 4) & 0x0F;
        if !self.state.password_config.verify(level, &password) {
            return Err("密码错误".to_string());
        }

        // 验证权限：需要02级或更高
        if level > 2 {
            return Err(format!("权限不足：需要02级权限，当前权限等级{}", level));
        }

        // 生成编程记录事件（在清零之前）
        // 事件类型：03-30-XX（编程记录）
        // 这里使用 0x30 0x1B 表示事件清零操作
        self.state.add_event_record(
            0x30, // 编程记录
            0x1B, // 事件清零
            chrono::Utc::now(),
            operator_code.to_vec(),
        );

        // 清空事件记录
        self.state.clear_all_events();

        Ok(operator_code)
    }

    /// 获取电表地址
    pub fn address(&self) -> [u8; 6] {
        self.state.address
    }

    /// 获取表状态的只读引用（用于测试和查询）
    pub fn state(&self) -> &MeterState {
        &self.state
    }

    /// 获取表状态的可变引用（用于测试和 admin 命令）
    pub fn state_mut(&mut self) -> &mut MeterState {
        &mut self.state
    }

    /// Applies the inputs used by the instantaneous power calculation.
    pub fn set_load_model(
        &mut self,
        voltage: f64,
        current: f64,
        power_factor: f64,
    ) -> Result<(), String> {
        if !(100.0..=1_000.0).contains(&voltage)
            || !(0.0..=10_000.0).contains(&current)
            || !(0.0..=1.0).contains(&power_factor)
        {
            return Err("simulation values are outside their valid range".into());
        }
        self.state.rated_voltage = (voltage * 1000.0).round() as u32;
        self.state.rated_current = (current * 1000.0).round() as u32;
        self.state.power_factor = power_factor;
        Ok(())
    }

    pub fn set_load_profile(&mut self, profile: super::physics_engine::LoadProfile) {
        self.physics.set_load_profile(profile);
    }

    pub fn simulation_config(&self) -> SimulationConfig {
        SimulationConfig {
            load_model: self.physics.load_model_config().clone(),
            rated_voltage: self.state.rated_voltage as f64 / 1000.0,
            rated_current: self.state.rated_current as f64 / 1000.0,
            rated_frequency: self.state.rated_frequency as f64,
            power_factor: self.state.power_factor,
            meter_constant: self.state.meter_constant,
            demand_period_minutes: self.state.demand_period_minutes,
            time_scale: self.state.simulation_time_scale,
        }
    }

    pub fn load_model_config(&self) -> &super::physics_engine::LoadModelConfig {
        self.physics.load_model_config()
    }

    pub fn apply_simulation_config(&mut self, config: SimulationConfig) -> Result<(), String> {
        config.validate()?;
        self.state.rated_voltage = (config.rated_voltage * 1000.0).round() as u32;
        self.state.rated_current = (config.rated_current * 1000.0).round() as u32;
        self.state.rated_frequency = config.rated_frequency.round() as u8;
        self.state.power_factor = config.power_factor;
        self.state.meter_constant = config.meter_constant;
        self.state.demand_period_minutes = config.demand_period_minutes;
        self.state.simulation_time_scale = config.time_scale;
        self.physics.set_load_model_config(config.load_model);
        Ok(())
    }

    /// 故障注入开关（转发到物理引擎）
    pub fn set_forced_fault(&mut self, event_type: u8, phase: u8, active: bool) {
        self.physics.set_forced_fault(event_type, phase, active);
    }

    /// 推进仿真（供外部定时调用，模拟 tick）
    ///
    /// 设计说明：
    /// - VirtualMeter 不创建 tick，只接收外部传入的 elapsed
    /// - 实际使用时，由全局 broadcast channel 统一产出 tick
    /// - 所有 VirtualMeter/MeterActor 订阅同一个广播通道
    ///
    /// 工作流程：
    /// 1. PhysicsEngine 更新 MeterState（包括冻结检测）
    /// 2. 检查 pending_freeze_trigger 标志
    /// 3. 如果有待处理的冻结，异步生成快照
    /// 4. 检查电能寄存器是否需要 flush
    pub fn tick(&mut self, elapsed: std::time::Duration, time_scale: f64) {
        // PhysicsEngine 更新 MeterState
        self.physics.tick(&mut self.state, elapsed, time_scale);

        // 负荷记录采样（设计 4.5.1.7 / §12.2）：检测采样条件并提交持久化
        let samples = self.physics.check_load_record_sampling(&mut self.state);
        if !samples.is_empty() {
            self.persist_load_records(samples);
        }

        // 检查并处理冻结触发
        self.process_pending_freeze();

        // 检查并处理电能 flush
        self.check_energy_flush();
    }

    /// 将负荷记录采样提交到 PersistenceWorker
    ///
    /// 注意：`tick()` 是同步函数，这里不能 `.await`，也不能 `tokio::spawn`
    /// （`VirtualMeter` 可能运行在没有 tokio Runtime 上下文的执行器里，
    /// `tokio::spawn` 会直接 panic）。改用 `try_send`：非阻塞、不需要 Runtime，
    /// 队列满/已关闭时立刻 `warn!`，而不是把失败信息丢进一个可能永远不会
    /// 被 poll 的任务里。
    fn persist_load_records(&self, samples: Vec<super::state::LoadRecordSample>) {
        use crate::persistence::LoadRecordRow;

        if let Some(persist_tx) = &self.persist_tx {
            let address_str = address_to_string(&self.state.address);
            for sample in samples {
                // 将 LoadRecordSample 转换为 LoadRecordRow
                let payload = match serde_json::to_value(&sample.data) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "[VirtualMeter {}] 负荷记录数据序列化失败: {}",
                            address_str, e
                        );
                        continue;
                    }
                };

                let row = LoadRecordRow {
                    meter_address: address_str.clone(),
                    class_id: sample.class_id,
                    sample_time: sample.sample_time,
                    payload,
                };

                if let Err(e) = persist_tx.try_send(PersistRequest::WriteLoadRecord(row)) {
                    warn!(
                        "[VirtualMeter {}] 负荷记录采样持久化队列已满或已关闭: {}",
                        address_str, e
                    );
                }
            }
        }
    }

    /// 处理待处理的冻结触发
    ///
    /// 此方法在 tick() 之后调用，检查 pending_freeze_trigger 标志
    /// 如果有待处理的冻结，生成快照并提交到 PersistenceWorker
    ///
    /// 持久化策略（按设计方案 4.6.4 节）：
    /// - 内存环形缓冲：定时冻结12次，瞬时冻结3次
    /// - 数据库持久化：日冻结62次，月冻结24次
    /// - 本方法同时处理内存写入和数据库提交
    fn process_pending_freeze(&mut self) {
        if let Some(freeze_type) = self.state.pending_freeze_trigger.take() {
            // 根据 FreezeType 确定 FreezeTrigger
            let trigger = match freeze_type {
                super::state::FreezeType::Timed => super::state::FreezeTrigger::Timed,
                super::state::FreezeType::Instant(_) => super::state::FreezeTrigger::Instant,
                super::state::FreezeType::Hourly => super::state::FreezeTrigger::Hourly,
                super::state::FreezeType::Daily => super::state::FreezeTrigger::Daily,
                super::state::FreezeType::Appointment => {
                    // 约定冻结暂时映射到定时冻结
                    super::state::FreezeTrigger::Timed
                }
                super::state::FreezeType::Event(_) => {
                    // 事件冻结暂时映射到瞬时冻结
                    super::state::FreezeTrigger::Instant
                }
            };

            // 如果有持久化通道，生成带持久化的快照
            if let Some(persist_tx) = &self.persist_tx {
                let (occurrence_index, snapshot_row) =
                    self.state.create_freeze_snapshot_with_persist(trigger);

                // 转换地址为字符串
                let address_str = address_to_string(&self.state.address);

                // 提交到 PersistenceWorker（非阻塞、不需要 tokio Runtime，见
                // persist_load_records 上方注释）
                let mut row = snapshot_row;
                row.meter_address = address_str.clone();

                if let Err(e) = persist_tx.try_send(PersistRequest::WriteFreezeSnapshot(row)) {
                    warn!(
                        "[VirtualMeter {}] 冻结快照持久化队列已满或已关闭: {}",
                        address_str, e
                    );
                }

                // 日志记录（可选，用于调试）
                #[cfg(debug_assertions)]
                {
                    println!(
                        "[VirtualMeter {}] 冻结快照已生成并提交持久化: trigger={:?}, time={}, occurrence={}",
                        address_str,
                        trigger,
                        self.state.virtual_time.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S UTC"),
                        occurrence_index
                    );
                }
            } else {
                // 无持久化通道，只生成内存快照
                let _occurrence_index = self.state.create_freeze_snapshot(trigger);
                let address_str = address_to_string(&self.state.address);
                #[cfg(debug_assertions)]
                {
                    println!(
                        "[VirtualMeter {}] 冻结快照已生成(仅内存): trigger={:?}, time={}, occurrence={}",
                        address_str,
                        trigger,
                        self.state.virtual_time.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S UTC"),
                        _occurrence_index
                    );
                }
            }
        }
    }

    /// 检查电能寄存器是否需要 flush（按设计方案 4.7 节）
    ///
    /// 触发条件（满足任一即触发）：
    /// 1. 时间间隔达到阈值（例如每5分钟）
    /// 2. 电量增量达到阈值（例如增加超过1kWh）
    ///
    /// 持久化内容：
    /// - 组合有功电能（正向、反向、四象限）
    /// - 各费率电能
    /// - 虚拟时钟
    fn check_energy_flush(&mut self) {
        // 检查是否有持久化通道
        if self.persist_tx.is_none() {
            return;
        }

        // 检查时间间隔
        let elapsed = self.last_energy_flush.elapsed();
        let time_triggered = elapsed.as_secs() >= self.config.energy_flush_config.interval_secs;

        // 检查电量增量（使用正向有功总电能）
        use super::state::EnergyType;
        let current_energy = self
            .state
            .energy_registers
            .get(&(EnergyType::ForwardActive, 0))
            .copied()
            .unwrap_or(0.0);
        let energy_delta = (current_energy - self.last_flushed_energy).abs();
        let energy_triggered = energy_delta >= self.config.energy_flush_config.threshold_kwh;

        // 任一条件满足则触发 flush
        if time_triggered || energy_triggered {
            self.flush_energy_registers();
            self.last_flushed_energy = current_energy;
        }
    }

    /// 执行电能寄存器 flush
    ///
    /// 将当前电能寄存器值和虚拟时间提交到 PersistenceWorker
    fn flush_energy_registers(&mut self) {
        if let Some(persist_tx) = &self.persist_tx {
            use super::state::EnergyType;
            use crate::persistence::EnergyRegisterRow;

            let address_str = address_to_string(&self.state.address);

            // 从HashMap中提取所有电能值
            let get_energy = |energy_type: EnergyType, rate: u8| -> f64 {
                self.state
                    .energy_registers
                    .get(&(energy_type, rate))
                    .copied()
                    .unwrap_or(0.0)
            };

            let row = EnergyRegisterRow {
                meter_address: address_str.clone(),
                timestamp: self.state.virtual_time,
                combined_active_positive: get_energy(EnergyType::ForwardActive, 0),
                combined_active_negative: get_energy(EnergyType::ReverseActive, 0),
                combined_reactive_positive: get_energy(EnergyType::ForwardReactive, 0),
                combined_reactive_negative: get_energy(EnergyType::ReverseReactive, 0),
                rate1_active_positive: get_energy(EnergyType::ForwardActive, 1),
                rate2_active_positive: get_energy(EnergyType::ForwardActive, 2),
                rate3_active_positive: get_energy(EnergyType::ForwardActive, 3),
                rate4_active_positive: get_energy(EnergyType::ForwardActive, 4),
            };

            // 非阻塞提交电能寄存器（见 persist_load_records 上方注释：这里不能
            // .await，也不能 tokio::spawn，用 try_send 代替）
            if let Err(e) = persist_tx.try_send(PersistRequest::WriteEnergyRegister(row)) {
                warn!(
                    "[VirtualMeter {}] 电能寄存器持久化队列已满或已关闭: {}",
                    address_str, e
                );
            }

            // 同时保存虚拟时间和配置（防止异常退出时丢失）
            let simulation_config = crate::simulation::SimulationConfig {
                load_model: self.config.physics_config.load_model.clone(),
                rated_voltage: self.state.rated_voltage as f64 / 1000.0, // 毫伏转伏
                rated_current: self.state.rated_current as f64 / 1000.0, // 毫安转安
                rated_frequency: self.state.rated_frequency as f64,
                power_factor: self.state.power_factor,
                meter_constant: self.state.meter_constant,
                demand_period_minutes: self.state.demand_period_minutes,
                time_scale: self.state.simulation_time_scale,
            };
            
            if let Err(e) = persist_tx.try_send(PersistRequest::SaveVirtualTime {
                address: address_str.clone(),
                virtual_time: self.state.virtual_time.with_timezone(&chrono::Utc),
                time_scale: self.state.simulation_time_scale,
                simulation_config,
            }) {
                warn!(
                    "[VirtualMeter {}] 虚拟时间持久化队列已满或已关闭: {}",
                    address_str, e
                );
            }

            // 更新 flush 状态
            self.last_energy_flush = std::time::Instant::now();

            #[cfg(debug_assertions)]
            {
                println!(
                    "[VirtualMeter {}] 电能寄存器和虚拟时间已 flush: time={}, energy={:.3} kWh",
                    address_str,
                    self.state.virtual_time.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S UTC"),
                    get_energy(EnergyType::ForwardActive, 0)
                );
            }
        }
    }

    /// 强制flush电能寄存器（公开方法，用于优雅关闭）
    ///
    /// 返回发送器的clone，调用方可用于等待写入完成
    pub fn force_flush_energy(&mut self) -> Option<mpsc::Sender<PersistRequest>> {
        self.flush_energy_registers();
        self.persist_tx.clone()
    }

    /// 保存虚拟时钟到数据库（用于优雅关闭）
    ///
    /// 此方法需要数据库连接池，由MeterActor调用
    pub async fn save_virtual_time(&self, pool: &sqlx::SqlitePool) -> Result<(), String> {
        use crate::persistence::PersistenceWorker;

        let address_str = address_to_string(&self.state.address);
        PersistenceWorker::save_virtual_time(
            pool,
            &address_str,
            self.state.virtual_time,
            1.0, // time_scale默认为1.0
        )
        .await
        .map_err(|e| format!("Failed to save virtual time: {}", e))
    }
}

/// 虚拟电表配置
#[derive(Debug, Clone)]
pub struct VirtualMeterConfig {
    /// 电表地址 (12位 BCD)
    pub address: [u8; 6],

    /// 物理引擎配置
    pub physics_config: PhysicsConfig,

    /// 电能 flush 配置
    pub energy_flush_config: EnergyFlushConfig,
}

impl Default for VirtualMeterConfig {
    fn default() -> Self {
        Self {
            address: [0x12, 0x34, 0x56, 0x78, 0x90, 0x12], // 123456789012
            physics_config: PhysicsConfig::default(),
            energy_flush_config: EnergyFlushConfig::default(),
        }
    }
}

/// 电能寄存器 flush 配置（按设计方案 4.7 节）
#[derive(Debug, Clone)]
pub struct EnergyFlushConfig {
    /// 时间间隔触发（秒）
    /// 例如：300 = 每5分钟 flush 一次
    pub interval_secs: u64,

    /// 电量增量阈值（kWh）
    /// 例如：1.0 = 电量增加超过 1kWh 就 flush
    pub threshold_kwh: f64,
}

impl Default for EnergyFlushConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300, // 5分钟
            threshold_kwh: 1.0, // 1kWh
        }
    }
}

// ============================================
// 辅助函数
// ============================================

// 地址字符串转换以 protocol::format 为唯一权威实现（低字节先传，反序 human-readable）。
// 此处为兼容历史调用方而保留同名别名。
pub use crate::protocol::format::{
    format_address as address_to_string, parse_address as string_to_address,
};

/// 解析负荷记录读取命令的时间范围字段（5 字节 BCD：mm hh DD MM YY，低字段先传）
fn parse_load_profile_time(data: &[u8]) -> Result<chrono::DateTime<chrono::Utc>, String> {
    use crate::protocol::format::bcd_to_u64;

    if data.len() != 5 {
        return Err("负荷记录时间字段长度错误".to_string());
    }

    let mm = bcd_to_u64(&data[0..1]).map_err(|e| e.to_string())? as u32;
    let hh = bcd_to_u64(&data[1..2]).map_err(|e| e.to_string())? as u32;
    let dd = bcd_to_u64(&data[2..3]).map_err(|e| e.to_string())? as u32;
    let month = bcd_to_u64(&data[3..4]).map_err(|e| e.to_string())? as u32;
    let yy = bcd_to_u64(&data[4..5]).map_err(|e| e.to_string())? as i32;
    let year = 2000 + yy;

    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(year, month, dd, hh, mm, 0)
        .single()
        .ok_or_else(|| {
            format!(
                "无效的负荷记录时间：{:04}-{:02}-{:02} {:02}:{:02}",
                year, month, dd, hh, mm
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_meter_basic() {
        let mut meter = VirtualMeter::default();

        // 测试读电压
        let di = [0x00, 0x01, 0x01, 0x02]; // A相电压
        let result = meter.handle_read_command(di);
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 2); // 2字节 BCD

        println!("电压数据: {:02X} {:02X}", data[0], data[1]);
    }

    #[test]
    fn test_address_conversion() {
        let address = [0x12, 0x90, 0x78, 0x56, 0x34, 0x12];
        let s = address_to_string(&address);
        assert_eq!(s, "123456789012");

        let addr2 = string_to_address("123456789012").unwrap();
        assert_eq!(addr2, address);
    }

    #[test]
    fn simulation_configuration_controls_state_and_calculation() {
        use super::super::physics_engine::{LoadModelConfig, LoadProfile, SimulationConfig};
        let mut meter = VirtualMeter::default();
        let config = SimulationConfig {
            load_model: LoadModelConfig {
                profile: LoadProfile::Fixed(0.5),
                voltage_noise_v: 0.0,
                frequency_noise_hz: 0.0,
                power_factor_noise: 0.0,
                power_factor_min: 0.0,
                power_factor_max: 1.0,
                phase_current_factors: [1.0, 1.0, 1.0],
            },
            rated_voltage: 230.0,
            rated_current: 20.0,
            rated_frequency: 50.0,
            power_factor: 0.9,
            meter_constant: 800,
            demand_period_minutes: 10,
            time_scale: 4.0,
        };
        meter.apply_simulation_config(config.clone()).unwrap();
        meter.tick(std::time::Duration::from_secs(1), config.time_scale);
        assert_eq!(meter.state().rated_voltage, 230_000);
        assert_eq!(meter.state().rated_current, 20_000);
        assert!((meter.state().voltage_a - 230.0).abs() < f64::EPSILON);
        assert!((meter.state().current_a - 10.0).abs() < f64::EPSILON);
        assert!((meter.state().active_power_total - 6.21).abs() < 0.001);
        assert!(matches!(
            meter.simulation_config().load_model.profile,
            LoadProfile::Fixed(0.5)
        ));
    }

    #[test]
    fn invalid_simulation_configuration_does_not_modify_meter_state() {
        let mut meter = VirtualMeter::default();
        let before = meter.simulation_config();
        let mut invalid = before.clone();
        invalid.rated_voltage = 10.0;

        assert!(meter.apply_simulation_config(invalid).is_err());

        let after = meter.simulation_config();
        assert_eq!(after.rated_voltage, before.rated_voltage);
        assert_eq!(after.rated_current, before.rated_current);
        assert_eq!(after.time_scale, before.time_scale);
    }
}