// 启动恢复功能测试
//
// 测试场景：
// 1. 创建VirtualMeter并设置状态
// 2. 保存状态到数据库
// 3. 创建新的VirtualMeter并从数据库恢复
// 4. 验证恢复的状态是否正确

use chrono::Local;
use meter_core::persistence::{
    EnergyRegisterRow, PersistRequest, PersistenceConfig, PersistenceWorker,
};
use meter_core::simulation::{virtual_meter::address_to_string, VirtualMeter, VirtualMeterConfig};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tokio::sync::mpsc;

/// 测试辅助：创建临时数据库
async fn setup_test_db() -> (TempDir, SqlitePool, PersistenceConfig) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir
        .path()
        .join("test_recovery.db")
        .to_str()
        .unwrap()
        .to_string();

    let config = PersistenceConfig {
        db_path: db_path.clone(),
        batch_max_size: 10,
        batch_timeout_ms: 100,
        max_connections: 2,
    };

    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&config.db_path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .unwrap();

    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    (temp_dir, pool, config)
}

#[tokio::test]
async fn test_energy_register_recovery() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    // 创建 PersistenceWorker
    let (persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    // 创建 VirtualMeter 并设置电能值
    let meter_config = VirtualMeterConfig::default();
    let mut meter = VirtualMeter::with_persistence(meter_config.clone(), persist_tx.clone());

    // 设置电能值
    use meter_core::simulation::state::EnergyType;
    meter
        .state_mut()
        .set_energy(EnergyType::ForwardActive, None, 1234.56);
    meter
        .state_mut()
        .set_energy(EnergyType::ForwardActive, Some(1), 300.0);
    meter
        .state_mut()
        .set_energy(EnergyType::ForwardActive, Some(2), 400.0);
    meter
        .state_mut()
        .set_energy(EnergyType::ReverseActive, None, 567.89);

    let address_str = address_to_string(&meter.address());

    // 手动flush电能寄存器
    let row = EnergyRegisterRow {
        meter_address: address_str.clone(),
        timestamp: Local::now(),
        combined_active_positive: meter.state().get_energy(EnergyType::ForwardActive, None),
        combined_active_negative: meter.state().get_energy(EnergyType::ReverseActive, None),
        combined_reactive_positive: 0.0,
        combined_reactive_negative: 0.0,
        rate1_active_positive: meter.state().get_energy(EnergyType::ForwardActive, Some(1)),
        rate2_active_positive: meter.state().get_energy(EnergyType::ForwardActive, Some(2)),
        rate3_active_positive: 0.0,
        rate4_active_positive: 0.0,
    };

    persist_tx
        .send(PersistRequest::WriteEnergyRegister(row))
        .await
        .unwrap();

    // 保存虚拟时钟
    let virtual_time = meter.state().virtual_time;
    PersistenceWorker::save_meter_state(&pool, &address_str, virtual_time, 1.0)
        .await
        .unwrap();

    // 等待写入完成
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // 创建新的VirtualMeter并恢复
    let mut meter2 = VirtualMeter::new(meter_config.clone());
    let restored = meter2.restore_from_database(&pool).await;

    assert!(restored.is_ok(), "恢复应该成功");
    assert!(restored.unwrap(), "应该成功恢复数据");

    // 验证恢复的电能值
    let energy_forward = meter2.state().get_energy(EnergyType::ForwardActive, None);
    let energy_reverse = meter2.state().get_energy(EnergyType::ReverseActive, None);
    let energy_rate1 = meter2
        .state()
        .get_energy(EnergyType::ForwardActive, Some(1));
    let energy_rate2 = meter2
        .state()
        .get_energy(EnergyType::ForwardActive, Some(2));

    assert!(
        (energy_forward - 1234.56).abs() < 0.01,
        "正向有功总电能应该恢复, expected=1234.56, actual={}",
        energy_forward
    );
    assert!(
        (energy_reverse - 567.89).abs() < 0.01,
        "反向有功总电能应该恢复, expected=567.89, actual={}",
        energy_reverse
    );
    assert!(
        (energy_rate1 - 300.0).abs() < 0.01,
        "费率1电能应该恢复, expected=300.0, actual={}",
        energy_rate1
    );
    assert!(
        (energy_rate2 - 400.0).abs() < 0.01,
        "费率2电能应该恢复, expected=400.0, actual={}",
        energy_rate2
    );

    // 验证虚拟时钟
    let time_diff = (meter2.state().virtual_time.timestamp() - virtual_time.timestamp()).abs();
    assert!(
        time_diff < 2,
        "虚拟时钟应该恢复, diff={} seconds",
        time_diff
    );

    println!("✓ 电能寄存器恢复测试通过");
    println!("  正向有功: {:.2} kWh", energy_forward);
    println!("  反向有功: {:.2} kWh", energy_reverse);
    println!("  费率1: {:.2} kWh", energy_rate1);
    println!("  费率2: {:.2} kWh", energy_rate2);
    println!(
        "  虚拟时间: {}",
        meter2.state().virtual_time.format("%Y-%m-%d %H:%M:%S")
    );

    pool.close().await;
}

#[tokio::test]
async fn test_recovery_nonexistent_meter() {
    let (_temp_dir, pool, _config) = setup_test_db().await;

    // 创建一个不存在的地址的VirtualMeter
    let mut config = VirtualMeterConfig::default();
    config.address = [0x99, 0x99, 0x99, 0x99, 0x99, 0x99];
    let mut meter = VirtualMeter::new(config);

    // 尝试恢复（应该返回Ok(false)，表示数据库中没有数据）
    let restored = meter.restore_from_database(&pool).await;

    assert!(restored.is_ok(), "恢复操作应该成功");
    assert!(!restored.unwrap(), "应该返回false（没有数据可恢复）");

    println!("✓ 不存在的电表恢复测试通过（正确返回false）");

    pool.close().await;
}

#[tokio::test]
async fn test_full_lifecycle_with_recovery() {
    let (_temp_dir, pool, config) = setup_test_db().await;

    // 启动 PersistenceWorker
    let (persist_tx, persist_rx) = mpsc::channel(100);
    let worker = PersistenceWorker::new(config.clone(), persist_rx)
        .await
        .unwrap();
    tokio::spawn(async move {
        worker.run().await;
    });

    // 第一阶段：创建电表并运行一段时间
    let meter_config = VirtualMeterConfig::default();
    let mut meter1 = VirtualMeter::with_persistence(meter_config.clone(), persist_tx.clone());

    use meter_core::simulation::state::EnergyType;
    meter1
        .state_mut()
        .set_energy(EnergyType::ForwardActive, None, 5000.0);

    let address_str = address_to_string(&meter1.address());

    // 模拟几个tick
    for _ in 0..5 {
        meter1.tick(std::time::Duration::from_secs(1), 1.0);
    }

    // 保存状态
    let row = EnergyRegisterRow {
        meter_address: address_str.clone(),
        timestamp: Local::now(),
        combined_active_positive: meter1.state().get_energy(EnergyType::ForwardActive, None),
        combined_active_negative: 0.0,
        combined_reactive_positive: 0.0,
        combined_reactive_negative: 0.0,
        rate1_active_positive: 0.0,
        rate2_active_positive: 0.0,
        rate3_active_positive: 0.0,
        rate4_active_positive: 0.0,
    };
    persist_tx
        .send(PersistRequest::WriteEnergyRegister(row))
        .await
        .unwrap();

    let virtual_time1 = meter1.state().virtual_time;
    PersistenceWorker::save_meter_state(&pool, &address_str, virtual_time1, 1.0)
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // 第二阶段：模拟重启，恢复状态
    let mut meter2 = VirtualMeter::with_persistence(meter_config.clone(), persist_tx.clone());
    let restored = meter2.restore_from_database(&pool).await.unwrap();
    assert!(restored, "应该成功恢复");

    let energy_before = meter2.state().get_energy(EnergyType::ForwardActive, None);
    println!("恢复后的电能值: {:.2} kWh", energy_before);

    // 继续运行
    for _ in 0..5 {
        meter2.tick(std::time::Duration::from_secs(1), 1.0);
    }

    let energy_after = meter2.state().get_energy(EnergyType::ForwardActive, None);
    println!("继续运行后的电能值: {:.2} kWh", energy_after);

    // 验证电能继续累加（应该>=恢复的值）
    assert!(
        energy_after >= energy_before,
        "电能应该继续累加, before={}, after={}",
        energy_before,
        energy_after
    );

    println!("✓ 完整生命周期测试通过（运行->保存->重启->恢复->继续运行）");

    pool.close().await;
}
