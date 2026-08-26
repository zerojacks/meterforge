// MeterRegistry - 电表注册表和地址路由
//
// 职责：
// 1. 维护地址 → MeterHandle 的映射
// 2. 处理精确地址、通配地址、广播地址的路由
// 3. 管理电表的生命周期（添加/删除/更新地址）

use super::messages::EngineMsg;
use super::meter_actor::MeterActorHandle;
use crate::protocol::{
    is_broadcast_address, is_wildcard_address, match_address, validate_broadcast_command, Frame,
};
use std::collections::HashMap;
use tokio::sync::mpsc;

// 地址字符串转换以 protocol::format 为唯一权威实现（低字节先传，反序 human-readable）。
// 此处为兼容历史调用方而保留同名别名。
pub use crate::protocol::format::{
    format_address as address_to_string, parse_address as string_to_address,
};

/// 电表注册表
pub struct MeterRegistry {
    /// 地址 → MeterHandle 映射
    meters: HashMap<String, MeterActorHandle>,
}

impl MeterRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            meters: HashMap::new(),
        }
    }

    /// 注册电表
    ///
    /// # 参数
    /// - `address`: 电表地址
    /// - `handle`: 电表Actor句柄
    ///
    /// # 返回
    /// - `Ok(())`: 注册成功
    /// - `Err(String)`: 地址已存在
    pub fn register(&mut self, address: [u8; 6], handle: MeterActorHandle) -> Result<(), String> {
        let addr_str = address_to_string(&address);

        if self.meters.contains_key(&addr_str) {
            return Err(format!("Address already registered: {}", addr_str));
        }

        self.meters.insert(addr_str, handle);
        Ok(())
    }

    /// 注销电表
    ///
    /// # 参数
    /// - `address`: 电表地址
    ///
    /// # 返回
    /// - `Some(MeterActorHandle)`: 被注销的句柄
    /// - `None`: 地址不存在
    pub fn unregister(&mut self, address: &[u8; 6]) -> Option<MeterActorHandle> {
        let addr_str = address_to_string(address);
        self.meters.remove(&addr_str)
    }

    /// 获取电表句柄（精确匹配）
    ///
    /// # 参数
    /// - `address`: 电表地址
    ///
    /// # 返回
    /// - `Some(&MeterActorHandle)`: 找到的句柄
    /// - `None`: 地址不存在
    pub fn get(&self, address: &[u8; 6]) -> Option<&MeterActorHandle> {
        let addr_str = address_to_string(address);
        self.meters.get(&addr_str)
    }

    /// 查找匹配通配地址的所有电表
    ///
    /// # 参数
    /// - `pattern`: 地址模式（可包含 0xAA 通配符）
    ///
    /// # 返回
    /// - 匹配的电表句柄列表
    pub fn find_wildcard(&self, pattern: &[u8; 6]) -> Vec<&MeterActorHandle> {
        self.meters
            .iter()
            .filter_map(|(addr_str, handle)| {
                if let Ok(actual_addr) = string_to_address(addr_str) {
                    if match_address(pattern, &actual_addr) {
                        return Some(handle);
                    }
                }
                None
            })
            .collect()
    }

    /// 获取所有电表句柄（用于广播）
    pub fn all_meters(&self) -> Vec<&MeterActorHandle> {
        self.meters.values().collect()
    }

    /// 路由协议帧到目标电表
    ///
    /// # 参数
    /// - `frame`: 解码后的协议帧
    /// - `reply_tx`: 响应通道（用于发送响应帧）
    ///
    /// # 返回
    /// - `Ok(usize)`: 成功路由到的电表数量
    /// - `Err(String)`: 路由错误
    pub async fn route_frame(
        &self,
        frame: Frame,
        conn_id: u64,
        reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<usize, String> {
        let address = frame.address;

        // 1. 判断地址类型
        if is_broadcast_address(&address) {
            return self.route_broadcast(frame, conn_id, reply_tx).await;
        }

        if is_wildcard_address(&address) {
            return self.route_wildcard(frame, conn_id, reply_tx).await;
        }

        // 2. 精确地址路由
        self.route_exact(frame, conn_id, reply_tx).await
    }

    /// 路由到精确地址
    async fn route_exact(
        &self,
        frame: Frame,
        conn_id: u64,
        reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<usize, String> {
        let handle = self
            .get(&frame.address)
            .ok_or_else(|| format!("Meter not found: {}", address_to_string(&frame.address)))?;

        let msg = EngineMsg::ProtocolCommand {
            conn_id,
            frame,
            reply_tx,
        };

        handle
            .send_engine_msg(msg)
            .await
            .map_err(|e| format!("Failed to send to meter: {}", e))?;

        Ok(1)
    }

    /// 路由到通配地址（设计 4.3：默认全部匹配表都应答）
    async fn route_wildcard(
        &self,
        frame: Frame,
        conn_id: u64,
        reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<usize, String> {
        let matches = self.find_wildcard(&frame.address);

        if matches.is_empty() {
            // 无匹配电表，静默丢弃（符合真实总线行为）
            return Ok(0);
        }

        let count = matches.len();

        // 每个匹配表各应答一帧（reply_tx 为 mpsc，clone 后并发回传）
        for handle in matches {
            let msg = EngineMsg::ProtocolCommand {
                conn_id,
                frame: frame.clone(),
                reply_tx: reply_tx.clone(),
            };
            let _ = handle.send_engine_msg(msg).await;
        }

        Ok(count)
    }

    /// 路由广播命令
    async fn route_broadcast(
        &self,
        frame: Frame,
        conn_id: u64,
        _reply_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<usize, String> {
        // 验证广播命令是否合法
        if !validate_broadcast_command(frame.control) {
            return Err(format!(
                "Invalid broadcast command: 0x{:02X}",
                frame.control
            ));
        }

        // 广播命令无应答：为每个表创建一次性回复通道并立即丢弃接收端
        let all_handles = self.all_meters();
        let count = all_handles.len();

        for handle in all_handles {
            let (_tx, _rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let msg = EngineMsg::ProtocolCommand {
                conn_id,
                frame: frame.clone(),
                reply_tx: _tx,
            };
            // 忽略发送错误（某些表可能已关闭）
            let _ = handle.send_engine_msg(msg).await;
        }

        Ok(count)
    }

    /// 更新电表地址（用于15H写通信地址命令）
    ///
    /// # 参数
    /// - `old_address`: 旧地址
    /// - `new_address`: 新地址
    ///
    /// # 返回
    /// - `Ok(())`: 更新成功
    /// - `Err(String)`: 错误信息
    pub fn update_address(
        &mut self,
        old_address: &[u8; 6],
        new_address: [u8; 6],
    ) -> Result<(), String> {
        let old_str = address_to_string(old_address);
        let new_str = address_to_string(&new_address);

        // 检查新地址是否已存在
        if self.meters.contains_key(&new_str) {
            return Err(format!("New address already exists: {}", new_str));
        }

        // 移除旧地址，插入新地址
        let handle = self
            .meters
            .remove(&old_str)
            .ok_or_else(|| format!("Old address not found: {}", old_str))?;

        // 通知Actor更新内部地址
        // TODO: 通过AdminCommand通知Actor

        self.meters.insert(new_str, handle);
        Ok(())
    }

    /// 获取注册的电表数量
    pub fn count(&self) -> usize {
        self.meters.len()
    }

    /// 获取所有注册的地址
    pub fn addresses(&self) -> Vec<[u8; 6]> {
        self.meters
            .keys()
            .filter_map(|s| string_to_address(s).ok())
            .collect()
    }
}

impl Default for MeterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn create_test_handle(address: [u8; 6]) -> MeterActorHandle {
        let (tx, _rx) = mpsc::channel(10);
        MeterActorHandle::new(tx, address)
    }

    #[test]
    fn test_address_conversion() {
        let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        // 低字节先传：内存 [A0..A5] 对应 human-readable 字符串反序
        let addr_str = address_to_string(&address);
        assert_eq!(addr_str, "060504030201");

        let parsed = string_to_address(&addr_str).unwrap();
        assert_eq!(parsed, address);
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = MeterRegistry::new();
        let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let handle = create_test_handle(address);

        // 注册
        assert!(registry.register(address, handle).is_ok());
        assert_eq!(registry.count(), 1);

        // 获取
        assert!(registry.get(&address).is_some());

        // 重复注册应该失败
        let handle2 = create_test_handle(address);
        assert!(registry.register(address, handle2).is_err());
    }

    #[test]
    fn test_unregister() {
        let mut registry = MeterRegistry::new();
        let address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let handle = create_test_handle(address);

        registry.register(address, handle).unwrap();
        assert_eq!(registry.count(), 1);

        // 注销
        let removed = registry.unregister(&address);
        assert!(removed.is_some());
        assert_eq!(registry.count(), 0);

        // 再次注销应该返回None
        let removed2 = registry.unregister(&address);
        assert!(removed2.is_none());
    }

    #[test]
    fn test_find_wildcard() {
        let mut registry = MeterRegistry::new();

        // 注册多个电表
        let addr1 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let addr2 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x07];
        let addr3 = [0x01, 0x02, 0xFF, 0x04, 0x05, 0x08];

        registry.register(addr1, create_test_handle(addr1)).unwrap();
        registry.register(addr2, create_test_handle(addr2)).unwrap();
        registry.register(addr3, create_test_handle(addr3)).unwrap();

        // 通配查询：01 02 03 04 05 AA（匹配最后一位）
        let pattern = [0x01, 0x02, 0x03, 0x04, 0x05, 0xAA];
        let matches = registry.find_wildcard(&pattern);
        assert_eq!(matches.len(), 2); // addr1 和 addr2

        // 通配查询：01 02 AA AA AA AA（匹配前两位）
        let pattern2 = [0x01, 0x02, 0xAA, 0xAA, 0xAA, 0xAA];
        let matches2 = registry.find_wildcard(&pattern2);
        assert_eq!(matches2.len(), 3); // 全部匹配
    }

    #[test]
    fn test_update_address() {
        let mut registry = MeterRegistry::new();
        let old_address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let new_address = [0x01, 0x02, 0x03, 0x04, 0x05, 0x07];

        registry
            .register(old_address, create_test_handle(old_address))
            .unwrap();

        // 更新地址
        assert!(registry.update_address(&old_address, new_address).is_ok());

        // 验证旧地址不存在
        assert!(registry.get(&old_address).is_none());

        // 验证新地址存在
        assert!(registry.get(&new_address).is_some());
    }

    #[test]
    fn test_addresses() {
        let mut registry = MeterRegistry::new();

        let addr1 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let addr2 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x07];

        registry.register(addr1, create_test_handle(addr1)).unwrap();
        registry.register(addr2, create_test_handle(addr2)).unwrap();

        let addresses = registry.addresses();
        assert_eq!(addresses.len(), 2);
        assert!(addresses.contains(&addr1));
        assert!(addresses.contains(&addr2));
    }

    #[tokio::test]
    async fn test_route_wildcard_all_respond() {
        let mut registry = MeterRegistry::new();

        let addr1 = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        let addr2 = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00];

        let (cmd_tx1, mut cmd_rx1) = mpsc::channel(10);
        let (cmd_tx2, mut cmd_rx2) = mpsc::channel(10);
        registry
            .register(addr1, MeterActorHandle::new(cmd_tx1, addr1))
            .unwrap();
        registry
            .register(addr2, MeterActorHandle::new(cmd_tx2, addr2))
            .unwrap();

        // 全通配地址：匹配所有表
        let pattern = [0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA];
        let frame = Frame::read(pattern, [0x00, 0x01, 0x01, 0x02]);
        let (reply_tx, _reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let count = registry.route_frame(frame, 1, reply_tx).await.unwrap();
        assert_eq!(count, 2, "通配查询应命中全部 2 个表");

        // 两个表都应收到命令
        assert!(cmd_rx1.recv().await.is_some());
        assert!(cmd_rx2.recv().await.is_some());
    }
}
