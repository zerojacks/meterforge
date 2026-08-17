-- SQLite schema for meter-core persistence
-- Based on design document section 11.3

-- ============================================
-- 冻结数据快照表
-- ============================================
-- 用 UNIQUE 约束实现"环形缓冲"语义：
-- 同一 (address, trigger_type, category, occurrence_idx) 再次写入即覆盖旧记录
CREATE TABLE freeze_snapshots (
    address         TEXT NOT NULL,
    trigger_type    INTEGER NOT NULL,   -- DI2: 00=定时/01=瞬时/02=时区表切换/03=日时段表切换
    category        INTEGER NOT NULL,   -- DI1: 数据类别（00=冻结时间/01=正向有功/02=反向有功/...）
    occurrence_idx  INTEGER NOT NULL,   -- DI0: 第几次快照（01-0C），滚动覆盖
    snapshot_time_ms INTEGER NOT NULL,  -- 快照时刻（Unix毫秒时间戳）
    payload_json    TEXT NOT NULL,      -- 快照数据（JSON格式）
    created_at_ms   INTEGER NOT NULL,   -- 记录创建时刻
    PRIMARY KEY (address, trigger_type, category, occurrence_idx)
) WITHOUT ROWID;

CREATE INDEX idx_freeze_snapshots_address_time 
    ON freeze_snapshots(address, snapshot_time_ms);

-- ============================================
-- 事件记录表
-- ============================================
-- 包含故障事件和编程记录，通过 event_kind (DI2) 区分类型
CREATE TABLE event_records (
    address         TEXT NOT NULL,
    event_kind      INTEGER NOT NULL,   -- DI2: 事件大类（01-2F故障事件，30编程记录，32购电，33购电金额）
    sub_kind        INTEGER NOT NULL,   -- DI1: 子类（00不分相/01-03相别，或编程记录具体类型）
    occurrence_idx  INTEGER NOT NULL,   -- DI0: 01..0A，最近第几次（01=最新）；00=汇总
    start_time_ms   INTEGER NOT NULL,   -- 事件发生时刻（或操作执行时刻）
    end_time_ms     INTEGER,            -- 事件结束时刻（编程记录为NULL）
    payload_json    TEXT NOT NULL,      -- 事件明细数据
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (address, event_kind, sub_kind, occurrence_idx)
) WITHOUT ROWID;

CREATE INDEX idx_event_records_address 
    ON event_records(address);

-- ============================================
-- 事件汇总表
-- ============================================
-- 总次数 + 总持续时长，对应 DI0=00 查询
CREATE TABLE event_summary (
    address           TEXT NOT NULL,
    event_kind        INTEGER NOT NULL,
    sub_kind          INTEGER NOT NULL,
    total_count       INTEGER NOT NULL DEFAULT 0,
    total_duration_min INTEGER NOT NULL DEFAULT 0,  -- 编程记录此字段为0
    updated_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (address, event_kind, sub_kind)
) WITHOUT ROWID;

-- ============================================
-- 负荷记录表
-- ============================================
-- 时间序列表，按时间范围查询
CREATE TABLE load_profile_records (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    address         TEXT NOT NULL,
    channel         INTEGER NOT NULL,    -- 通道标识（01-06=第1-6类负荷，10=总加通道）
    data_type       INTEGER NOT NULL,    -- 数据类型：DI1编码
    recorded_at_ms  INTEGER NOT NULL,    -- 采样时刻
    payload_json    TEXT NOT NULL,       -- 采样数据
    created_at_ms   INTEGER NOT NULL
);

CREATE INDEX idx_load_profile_address_channel_time
    ON load_profile_records(address, channel, data_type, recorded_at_ms);

-- ============================================
-- 表参量配置表（用于重启恢复）
-- ============================================
CREATE TABLE meters (
    address              TEXT PRIMARY KEY,
    meter_constant       INTEGER NOT NULL,
    rated_voltage_mv     INTEGER NOT NULL,       -- 毫伏存储
    rated_current_ma     INTEGER NOT NULL,       -- 毫安存储
    demand_period_min    INTEGER NOT NULL,
    sliding_window_min   INTEGER NOT NULL,
    freeze_mode_word     INTEGER NOT NULL,
    load_record_mode_word INTEGER NOT NULL,
    settlement_days_json TEXT NOT NULL,          -- JSON数组
    tou_config_json      TEXT NOT NULL,          -- 时段表配置
    passwords_json       TEXT NOT NULL,          -- 密码配置
    comm_baud_json       TEXT NOT NULL,          -- 通信速率
    load_model_json      TEXT NOT NULL,          -- 负荷模型
    virtual_time_ms      INTEGER NOT NULL,       -- 虚拟时钟
    time_scale           REAL NOT NULL DEFAULT 1.0,
    updated_at_ms        INTEGER NOT NULL
);

-- ============================================
-- 电能寄存器表
-- ============================================
CREATE TABLE energy_registers (
    address         TEXT NOT NULL,
    energy_kind     INTEGER NOT NULL,   -- 对应DI2编码
    rate_index      INTEGER NOT NULL,   -- 对应DI1，00=总
    settlement_day  INTEGER NOT NULL,   -- 对应DI0，00=当前
    value_fp        INTEGER NOT NULL,   -- 定点数：真实值*100
    updated_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (address, energy_kind, rate_index, settlement_day)
) WITHOUT ROWID;

CREATE INDEX idx_energy_registers_address 
    ON energy_registers(address);

-- ============================================
-- 最大需量表
-- ============================================
CREATE TABLE max_demand (
    address         TEXT NOT NULL,
    demand_kind     INTEGER NOT NULL,
    rate_index      INTEGER NOT NULL,
    value_fp        INTEGER NOT NULL,
    occurred_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (address, demand_kind, rate_index)
) WITHOUT ROWID;
