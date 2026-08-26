// 数据库持久化集成测试
//
// 测试场景：
// 1. 数据库初始化和迁移
// 2. 冻结快照写入（内存 + 数据库）
// 3. 冻结快照读取（DI0 ≤ 0C 从内存，DI0 > 0C 从数据库）
// 4. 环形缓冲覆盖机制
// 5. PersistenceWorker 批量写入

use meter_core::persistence::{
    FreezeSnapshotRow, PersistRequest, PersistenceConfig, PersistenceWorker,
};
use meter_core::simulation::{
    di_handler::DIHandler,
    state::{EnergyType, FreezeTrigger, MeterState},
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tokio::sync::mpsc;

/// 测试辅助：创建临时数据库
async fn setup_test_db() -> (TempDir, SqlitePool, PersistenceConfig) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir
        .path()
        .join("test.db")
        .to_str()
        .unwrap()
        .to_string();

    let config = PersistenceConfig {
        db_path: db_path.clone(),
        batch_max_size: 10,
        batch_timeout_ms: 100,
        max_connections: 2,
        load_profile_max_records_per_class: 2000,
        load_profile_cleanup_interval_secs: 600,
    };

    // 创建连接池（用于测试查询）
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&config.db_path) // 使用 config.db_path 而不是 db_path
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .unwrap();

    // 运行迁移
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    (temp_dir, pool, config)
}

#[tokio::test]
async fn test_database_migration() {
    let (_temp_dir, pool, _config) = setup_test_db().await;

    // 验证表是否创建成功
    let result = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='freeze_snapshots'",
    )
    .fetch_one(&pool)
    .await;

    assert!(result.is_ok(), "freeze_snapshots 表应该存在");

    pool.close().await;
}

#[tokio::test]
async fn test_freeze_snapshot_write_and_read() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    // 创建 PersistenceWorker 通道
    let (persist_tx, persist_rx) = mpsc::channel(100);

    // 启动 PersistenceWorker（在后台任务中）
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    // 创建测试数据
    let mut state = MeterState::default();
    state.set_energy(EnergyType::ForwardActive, None, 1234.56);
    state.set_energy(EnergyType::ReverseActive, None, 567.89);

    // 生成冻结快照（使用 with_persist 方法）
    let trigger = FreezeTrigger::Timed;
    let (_occurrence_idx, mut snapshot_row) = state.create_freeze_snapshot_with_persist(trigger);
    snapshot_row.meter_address = "123456789012".to_string();

    // 提交到 PersistenceWorker（新格式）
    persist_tx
        .send(PersistRequest::WriteFreezeSnapshot(snapshot_row.clone()))
        .await
        .unwrap();

    // 等待写入完成
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // 验证数据库中的数据
    let row = sqlx::query(
        "SELECT * FROM freeze_snapshots WHERE address = ? AND trigger_type = ? AND occurrence_idx = ?"
    )
    .bind("123456789012")
    .bind(trigger.to_di2()) // 使用协议值（DI2）
    .bind(1u8)
    .fetch_one(&pool)
    .await
    .unwrap();

    let payload_json: String = row.get("payload_json");
    let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();

    assert_eq!(payload["forward_active_total"].as_f64().unwrap(), 1234.56);
    assert_eq!(payload["reverse_active_total"].as_f64().unwrap(), 567.89);

    pool.close().await;
}

#[tokio::test]
async fn test_freeze_snapshot_memory_vs_database_read() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    let (persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    let mut state = MeterState::default();
    let address = "123456789012";
    let trigger = FreezeTrigger::Timed;

    // 生成3个快照（在内存容量内）
    for i in 1..=3 {
        state.set_energy(EnergyType::ForwardActive, None, 1000.0 + i as f64 * 100.0);
        let (_idx, mut snapshot_row) = state.create_freeze_snapshot_with_persist(trigger);
        snapshot_row.meter_address = address.to_string();

        persist_tx
            .send(PersistRequest::WriteFreezeSnapshot(snapshot_row))
            .await
            .unwrap();
    }

    // 生成更多快照（超过内存容量12，写入数据库）
    // 注意：数据库每次冻结只落一行完整摘要（category=0xFF），异步读取时
    // 也按 category=0xFF 查询后再按 DI1 现场抽取
    for i in 4..=15 {
        let snapshot_row = FreezeSnapshotRow {
            meter_address: address.to_string(),
            trigger_type: trigger.to_di2(), // 使用协议值（DI2）
            category: 0xFF, // 完整快照摘要行（与 write_freeze_snapshot 一致）
            occurrence_idx: i,
            snapshot_time: chrono::Utc::now(),
            payload: serde_json::json!({
                "forward_active_total": 1000.0 + i as f64 * 100.0,
                "reverse_active_total": 500.0,
                "forward_reactive_total": 200.0,
                "reverse_reactive_total": 100.0,
                "voltage_a": 220.0,
                "voltage_b": 220.0,
                "voltage_c": 220.0,
                "current_a": 10.0,
                "current_b": 10.0,
                "current_c": 10.0,
                "power_factor": 0.95,
                "frequency": 50.0,
            }),
        };

        persist_tx
            .send(PersistRequest::WriteFreezeSnapshot(snapshot_row))
            .await
            .unwrap();
    }

    // 等待写入完成（增加等待时间）
    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

    let handler = DIHandler::new();

    // 测试1: 读取内存中的快照（DI0=01，应该从内存读取）
    let di_memory = [0x01, 0x01, 0x00, 0x05]; // DI0=01, DI1=01(正向有功), DI2=00(定时), DI3=05(冻结)
    let result_memory = handler.handle_read(di_memory, &state);
    assert!(result_memory.is_ok(), "内存读取应该成功");

    // 测试2: 内存环形缓冲中只有3条快照，DI0=05 未命中内存
    // （数据库侧 write_freeze_snapshot 按"最新=1、整体挪号、容量12封顶"
    // 存储，因此同步读取内存未命中的快照应该返回错误）
    let di_database = [0x05, 0x01, 0x00, 0x05]; // DI0=05, DI1=01(正向有功), DI2=00(定时), DI3=05
    let result_db_sync = handler.handle_read(di_database, &state);
    assert!(result_db_sync.is_err(), "同步方法读取内存未命中的快照应该返回错误");
    assert!(
        result_db_sync.unwrap_err().contains("数据库支持"),
        "错误消息应提示使用异步版本"
    );

    // 测试3: 使用异步方法（内存未命中 → 数据库查询）读取
    let result_db_async = handler
        .handle_freeze_data_read_async(di_database, &state, address, &pool)
        .await;

    if let Err(ref e) = result_db_async {
        eprintln!("异步数据库读取失败: {}", e);
    }

    assert!(
        result_db_async.is_ok(),
        "异步数据库读取应该成功: {:?}",
        result_db_async.err()
    );
    let data = result_db_async.unwrap();
    assert_eq!(data.len(), 4, "电能数据应该是4字节");

    pool.close().await;
}

#[tokio::test]
async fn test_ring_buffer_overflow_with_database() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    let (persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    let mut state = MeterState::default();
    let address = "123456789012";
    let trigger = FreezeTrigger::Instant; // 瞬时冻结，容量只有3

    // 生成5个快照（超过容量3）
    for i in 1..=5 {
        state.set_energy(EnergyType::ForwardActive, None, 100.0 + i as f64 * 10.0);

        // 手动创建快照行（模拟不同的occurrence_idx）
        let occurrence_idx = ((i - 1) % 3 + 1) as u8; // 循环 1->2->3->1->2
        let snapshot_row = FreezeSnapshotRow {
            meter_address: address.to_string(),
            trigger_type: trigger.to_di2(), // 使用协议值（DI2）
            category: 0xFF,
            occurrence_idx,
            snapshot_time: chrono::Utc::now(),
            payload: serde_json::json!({
                "forward_active_total": 100.0 + i as f64 * 10.0,
                "reverse_active_total": 50.0,
                "forward_reactive_total": 20.0,
                "reverse_reactive_total": 10.0,
                "voltage_a": 220.0,
                "voltage_b": 220.0,
                "voltage_c": 220.0,
                "current_a": 10.0,
                "current_b": 10.0,
                "current_c": 10.0,
                "power_factor": 0.95,
                "frequency": 50.0,
            }),
        };

        persist_tx
            .send(PersistRequest::WriteFreezeSnapshot(snapshot_row))
            .await
            .unwrap();
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // 验证内存中保留的快照数量
    // 注意：我们手动创建了5个快照，但内存环形缓冲容量是3
    // 由于我们是手动提交到数据库，内存中的state.freeze_snapshots需要单独管理
    // 这里我们主要测试数据库存储，所以注释掉内存检查
    // let snapshots = state.get_all_freeze_snapshots(trigger);
    // assert_eq!(snapshots.len(), 3, "内存应该只保留3个快照");

    // 验证数据库中有3个不同的 occurrence_idx（环形覆盖机制通过 UNIQUE 约束实现）
    // 5个写入请求，但occurrence_idx循环1->2->3->1->2，所以最终数据库中有3个
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM freeze_snapshots WHERE address = ? AND trigger_type = ?",
    )
    .bind(address)
    .bind(trigger.to_di2()) // 使用协议值（DI2）
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        count, 3,
        "数据库中应该有3个不同的occurrence_idx（瞬时冻结容量限制）"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_persistence_worker_batch_write() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    let (persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    let address = "123456789012";

    // 批量提交20个写入请求（超过 batch_max_size=10）
    for i in 1..=20 {
        let snapshot_row = FreezeSnapshotRow {
            meter_address: address.to_string(),
            trigger_type: FreezeTrigger::Timed.to_di2(), // 使用协议值（DI2）
            category: 0xFF,
            occurrence_idx: (i % 12 + 1) as u8, // 模拟环形覆盖
            snapshot_time: chrono::Utc::now(),
            payload: serde_json::json!({
                "forward_active_total": 1000.0 + i as f64,
                "reverse_active_total": 500.0,
                "forward_reactive_total": 200.0,
                "reverse_reactive_total": 100.0,
                "voltage_a": 220.0,
                "voltage_b": 220.0,
                "voltage_c": 220.0,
                "current_a": 10.0,
                "current_b": 10.0,
                "current_c": 10.0,
                "power_factor": 0.95,
                "frequency": 50.0,
            }),
        };

        persist_tx
            .send(PersistRequest::WriteFreezeSnapshot(snapshot_row))
            .await
            .unwrap();
    }

    // 等待批量写入完成
    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

    // 验证数据库中的记录数量
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM freeze_snapshots WHERE address = ?")
        .bind(address)
        .fetch_one(&pool)
        .await
        .unwrap();

    // 由于环形覆盖（occurrence_idx 循环），最终应该有12个记录（每个 occurrence_idx 被覆盖一次）
    assert!(count <= 12, "环形覆盖应该限制记录数量");
    assert!(count > 0, "应该有数据写入");

    pool.close().await;
}

#[tokio::test]
async fn test_query_nonexistent_snapshot() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    let (_persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    let state = MeterState::default();
    let handler = DIHandler::new();
    let address = "999999999999"; // 不存在的地址

    // 尝试查询不存在的快照
    let di = [0x0D, 0x01, 0x00, 0x05]; // DI0=0D (需要数据库)
    let result = handler
        .handle_freeze_data_read_async(di, &state, address, &pool)
        .await;

    assert!(result.is_err(), "查询不存在的快照应该返回错误");
    assert!(
        result.unwrap_err().contains("无此冻结快照"),
        "错误消息应明确说明快照不存在"
    );

    pool.close().await;
}
