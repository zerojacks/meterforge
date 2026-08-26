-- 清理废弃的 load_profile_samples 表
-- 
-- 该表在 0002 中创建，用于仿真数据测试
-- 新方案采用 load_profile_records 表 (0003)，旧表不再使用

DROP TABLE IF EXISTS load_profile_samples;
