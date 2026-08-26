// DL/T 645-2007 控制码定义

/// DL/T 645-2007 控制码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ControlCode {
    /// 0x11: 读数据
    Read = 0x11,

    /// 0x91: 从站正常应答读数据
    ReadResponse = 0x91,

    /// 0x14: 写数据
    Write = 0x14,

    /// 0x94: 从站正常应答写数据
    WriteResponse = 0x94,

    /// 0x08: 广播校时
    BroadcastTime = 0x08,

    /// 0x16: 冻结命令
    Freeze = 0x16,

    /// 0x96: 从站正常应答冻结
    FreezeResponse = 0x96,

    /// 0x17: 改通信速率
    ChangeBaud = 0x17,

    /// 0x97: 从站正常应答改速率
    ChangeBaudResponse = 0x97,

    /// 0x18: 改密码
    ChangePassword = 0x18,

    /// 0x98: 从站正常应答改密码
    ChangePasswordResponse = 0x98,

    /// 0x19: 最大需量清零
    ClearDemand = 0x19,

    /// 0x99: 从站正常应答清需量
    ClearDemandResponse = 0x99,

    /// 0x1A: 电表清零
    ClearEnergy = 0x1A,

    /// 0x9A: 从站正常应答清电表
    ClearEnergyResponse = 0x9A,

    /// 0x1B: 事件清零
    ClearEvent = 0x1B,

    /// 0x9B: 从站正常应答清事件
    ClearEventResponse = 0x9B,

    /// 0xD1: 从站异常应答（带错误码）
    Error = 0xD1,
}

impl ControlCode {
    /// 从字节转换为控制码
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x11 => Some(Self::Read),
            0x91 => Some(Self::ReadResponse),
            0x14 => Some(Self::Write),
            0x94 => Some(Self::WriteResponse),
            0x08 => Some(Self::BroadcastTime),
            0x16 => Some(Self::Freeze),
            0x96 => Some(Self::FreezeResponse),
            0x17 => Some(Self::ChangeBaud),
            0x97 => Some(Self::ChangeBaudResponse),
            0x18 => Some(Self::ChangePassword),
            0x98 => Some(Self::ChangePasswordResponse),
            0x19 => Some(Self::ClearDemand),
            0x99 => Some(Self::ClearDemandResponse),
            0x1A => Some(Self::ClearEnergy),
            0x9A => Some(Self::ClearEnergyResponse),
            0x1B => Some(Self::ClearEvent),
            0x9B => Some(Self::ClearEventResponse),
            0xD1 => Some(Self::Error),
            _ => None,
        }
    }

    /// 判断是否为从站应答
    pub fn is_response(&self) -> bool {
        matches!(
            self,
            Self::ReadResponse
                | Self::WriteResponse
                | Self::FreezeResponse
                | Self::ChangeBaudResponse
                | Self::ChangePasswordResponse
                | Self::ClearDemandResponse
                | Self::ClearEnergyResponse
                | Self::ClearEventResponse
                | Self::Error
        )
    }

    /// 判断是否为主站命令
    pub fn is_command(&self) -> bool {
        !self.is_response()
    }
}

/// 错误码定义（用于 0xD1 异常应答的数据域）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    /// 其他错误
    Other = 0x01,

    /// 无请求数据
    NoData = 0x02,

    /// 密码错误/未授权
    Unauthorized = 0x04,

    /// 通信速率不能更改
    BaudNotChangeable = 0x08,

    /// 年时区数超
    YearZoneOverflow = 0x10,

    /// 日时段数超
    DayTimeOverflow = 0x20,

    /// 费率数超
    RateOverflow = 0x40,
}
