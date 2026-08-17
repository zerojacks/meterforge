// DL/T 645-2007 协议基础定义模块
//
// 本模块只包含协议层的基础定义：
// - 帧结构定义和编解码
// - 控制码定义
// - BCD/格式转换工具
//
// 实际的电表状态管理和DI处理在 simulation 模块中实现

pub mod codec;
pub mod control_code;
pub mod format;
pub mod frame;

pub use codec::{
    decode_frame, encode_data_block_response, encode_error_response, encode_frame,
    encode_frame_raw, is_broadcast_address, is_wildcard_address, match_address, parse_di,
    validate_broadcast_command, ErrorInfoWord,
};
pub use control_code::ControlCode;
pub use format::*;
pub use frame::{Frame, FrameType};
