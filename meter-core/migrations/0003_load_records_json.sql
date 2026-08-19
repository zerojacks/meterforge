-- 负荷记录表（JSON存储，对齐冻结数据模式）
-- 
-- 设计说明：
-- - 与 freeze_snapshots 同款模式：主键做环形去重，payload 存 JSON，WITHOUT ROWID
-- - class_id 对应协议附录B的第1~6类负荷记录（不是"数据类型"）
-- - 主键 (address, class_id, sample_time_ms) 天然防重复采样
-- - payload_json 存储 LoadRecordData 的完整快照，块级 Option（None块JSON键直接缺省）

-- 清理旧的平铺字段表（0001创建但从未使用）
DROP TABLE IF EXISTS load_profile_records;

-- 负荷记录表：一行 = 某表某类负荷在某时刻的完整快照
CREATE TABLE load_profile_records (
    address        TEXT NOT NULL,
    class_id       INTEGER NOT NULL,      -- 第1~6类负荷记录（1-6）
    sample_time_ms INTEGER NOT NULL,      -- 采样时刻（虚拟时钟）
    payload_json   TEXT NOT NULL,         -- LoadRecordData 序列化 JSON
    created_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (address, class_id, sample_time_ms)
) WITHOUT ROWID;

-- 按地址+时间查询索引（支持时间范围查询）
CREATE INDEX idx_load_profile_records_time
    ON load_profile_records(address, sample_time_ms);
