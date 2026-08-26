-- 负荷记录采样表（修正）
-- 
-- 设计说明：
-- - 负荷记录采样数据直接落库，不维护内存历史
-- - 按数据类型（DI1）和通道（DI0）分别存储
-- - 使用定点数存储（value_fp = 真实值 * 10000）避免浮点精度问题
-- - 支持按时间范围查询

CREATE TABLE IF NOT EXISTS load_profile_samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    address         TEXT NOT NULL,
    data_type       INTEGER NOT NULL,    -- DI1: 01=电压, 02=电流, 03=有功功率, 04=无功功率, 05=功率因数, 06=电能量, 07=无功电能, 08=需量
    channel         INTEGER NOT NULL,    -- DI0: 00=总, 01=A相, 02=B相, 03=C相
    sample_time_ms  INTEGER NOT NULL,    -- 采样时刻（Unix毫秒时间戳）
    value_fp        INTEGER NOT NULL,    -- 定点数：真实值 * 10000
    created_at_ms   INTEGER NOT NULL     -- 记录创建时刻
);

-- 优化查询性能的索引：按地址、数据类型、通道、时间范围查询
CREATE INDEX IF NOT EXISTS idx_load_profile_samples_query
    ON load_profile_samples(address, data_type, channel, sample_time_ms);

-- 按地址查询的索引
CREATE INDEX IF NOT EXISTS idx_load_profile_samples_address
    ON load_profile_samples(address, sample_time_ms);
