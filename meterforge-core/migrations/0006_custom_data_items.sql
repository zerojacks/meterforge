-- ============================================
-- 自定义数据项表（数据项自定义功能）
-- ============================================
-- 用户针对某块表自定义指定 DI 的应答内容，读取时按 DI 精确匹配、
-- 原始字节直接回复（不做协议编解码转换）。是否启用、命中失败时如何
-- 处理由 meters.custom_data_mode 开关控制（见 ensure_meter_schema）：
--   0 = 优先使用自定义数据项（未命中则回退模拟数据）
--   1 = 完全使用自定义数据项（未命中则应答"无数据"错误）
--   2 = 使用模拟数据（不查自定义数据项，默认，兼容历史行为）
CREATE TABLE custom_data_items (
    address       TEXT NOT NULL,
    di            TEXT NOT NULL,      -- 8位大写HEX，按 DI3DI2DI1DI0 顺序拼接
    data_hex      TEXT NOT NULL,      -- 应答DATA内容（HEX编码，不含DI本身；按人类正常顺序存储，
                                       -- 回复前由 VirtualMeter 整体逆序，不做其余协议转换）
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (address, di)
) WITHOUT ROWID;

CREATE INDEX idx_custom_data_items_address
    ON custom_data_items(address);
