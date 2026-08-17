// DL/T 645-2007 帧结构定义

use serde::{Deserialize, Serialize};

/// 帧类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameType {
    /// 主站查询
    Read,
    /// 主站写入
    Write,
    /// 主站广播校时
    BroadcastTime,
    /// 主站冻结命令
    Freeze,
    /// 主站改通信速率
    ChangeBaud,
    /// 主站改密码
    ChangePassword,
    /// 主站清需量
    ClearDemand,
    /// 主站清电量
    ClearEnergy,
    /// 主站清事件
    ClearEvent,
    /// 从站正常应答
    Response,
    /// 从站异常应答
    Error,
}

/// DL/T 645-2007 帧结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// 地址域（6 字节，BCD 码）
    pub address: [u8; 6],

    /// 控制码
    pub control: u8,

    /// 数据域（原始字节，已去除 +33H 偏移）
    pub data: Vec<u8>,
}

impl Frame {
    /// 创建查询帧
    pub fn read(address: [u8; 6], di: [u8; 4]) -> Self {
        Frame {
            address,
            control: 0x11, // 读数据
            data: di.to_vec(),
        }
    }

    /// 创建写入帧
    pub fn write(address: [u8; 6], di: [u8; 4], value: Vec<u8>) -> Self {
        let mut data = di.to_vec();
        data.extend_from_slice(&value);
        Frame {
            address,
            control: 0x14, // 写数据
            data,
        }
    }

    /// 创建响应帧
    pub fn response(address: [u8; 6], di: [u8; 4], value: Vec<u8>) -> Self {
        let mut data = di.to_vec();
        data.extend_from_slice(&value);
        Frame {
            address,
            control: 0x91, // 从站正常应答
            data,
        }
    }

    /// 创建错误帧
    pub fn error(address: [u8; 6], error_code: u8) -> Self {
        Frame {
            address,
            control: 0xD1, // 从站异常应答
            data: vec![error_code],
        }
    }

    /// 判断是否为广播地址
    pub fn is_broadcast(&self) -> bool {
        self.address == [0x99, 0x99, 0x99, 0x99, 0x99, 0x99]
    }

    /// 判断是否为通配地址
    pub fn is_wildcard(&self) -> bool {
        self.address.iter().any(|&b| b == 0xAA)
    }

    /// 获取帧类型
    pub fn frame_type(&self) -> FrameType {
        // 先检查应答位（bit7=1）和异常位（bit6=1）
        if (self.control & 0x80) != 0 {
            return FrameType::Response;
        }
        if (self.control & 0x40) != 0 {
            return FrameType::Error;
        }

        // 再检查命令码（低5位）
        match self.control & 0x1F {
            0x11 => FrameType::Read,
            0x14 => FrameType::Write,
            0x08 => FrameType::BroadcastTime,
            0x16 => FrameType::Freeze,
            0x17 => FrameType::ChangeBaud,
            0x18 => FrameType::ChangePassword,
            0x19 => FrameType::ClearDemand,
            0x1A => FrameType::ClearEnergy,
            0x1B => FrameType::ClearEvent,
            _ => FrameType::Read, // 默认
        }
    }
}
