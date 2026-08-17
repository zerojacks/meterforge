use gpui::*;
use meter_core::actor::{AdminCommand, MeterActorHandle};
use meter_core::simulation::SimulationConfig;
use meter_core::ConnectionManager;
use parking_lot::RwLock;
use std::{collections::HashMap, sync::Arc};

/// A presentation-level intent.  It deliberately contains no GPUI types and
/// lets views stay independent from the actor protocol.
#[derive(Debug, Clone)]
pub enum MeterAction {
    SetVirtualTime {
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    },
    ChangePassword {
        level: u8,
        new_password: [u8; 4],
    },
    SetBaudrate {
        baudrate: u8,
    },
    SetTouConfig {
        time_slots: Vec<(u8, u8, u8)>,
    },
    ClearMaxDemand,
    ClearMeter,
    ApplySimulationConfig {
        config: SimulationConfig,
    },
    ApplyFreezeConfig {
        timed_mode: u8,
        instant_mode: u8,
        appointment_mode: u8,
        hourly_mode: u8,
        daily_mode: u8,
        daily_time: [u8; 2],
        hourly_start: [u8; 5],
        hourly_interval_min: u8,
        appointment_time: [u8; 5],
    },
    ApplySettlementDays {
        days: [u8; 3],
        hours: [u8; 3],
    },
    ApplyLoadRecordConfig {
        mode_word: u8,
        start_time: [u8; 4],
        intervals: [u16; 6],
    },
    InjectFault {
        event_type: u8,
        phase: u8,
        active: bool,
    },
}

impl From<MeterAction> for AdminCommand {
    fn from(value: MeterAction) -> Self {
        match value {
            MeterAction::SetVirtualTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            } => AdminCommand::SetVirtualTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            },
            MeterAction::ChangePassword {
                level,
                new_password,
            } => AdminCommand::ChangePassword {
                level,
                new_password,
            },
            MeterAction::SetBaudrate { baudrate } => AdminCommand::SetBaudrate { baudrate },
            MeterAction::SetTouConfig { time_slots } => AdminCommand::SetTouConfig { time_slots },
            MeterAction::ClearMaxDemand => AdminCommand::ClearMaxDemand,
            MeterAction::ClearMeter => AdminCommand::ClearMeter,
            MeterAction::ApplySimulationConfig { config } => {
                AdminCommand::ApplySimulationConfig { config }
            }
            MeterAction::ApplyFreezeConfig {
                timed_mode,
                instant_mode,
                appointment_mode,
                hourly_mode,
                daily_mode,
                daily_time,
                hourly_start,
                hourly_interval_min,
                appointment_time,
            } => AdminCommand::ApplyFreezeConfig {
                timed_mode,
                instant_mode,
                appointment_mode,
                hourly_mode,
                daily_mode,
                daily_time,
                hourly_start,
                hourly_interval_min,
                appointment_time,
            },
            MeterAction::ApplySettlementDays { days, hours } => {
                AdminCommand::ApplySettlementDays { days, hours }
            }
            MeterAction::ApplyLoadRecordConfig {
                mode_word,
                start_time,
                intervals,
            } => AdminCommand::ApplyLoadRecordConfig {
                mode_word,
                start_time,
                intervals,
            },
            MeterAction::InjectFault {
                event_type,
                phase,
                active,
            } => AdminCommand::InjectFault {
                event_type,
                phase,
                active,
            },
        }
    }
}

/// Long-lived backend services exposed to the presentation layer.
///
/// Keeping this as one GPUI global makes views easy to construct and test: no
/// view owns connection tasks, actor handles, or a Tokio runtime.
#[derive(Clone)]
pub struct AppBackend {
    pub connections: ConnectionManager,
    meters: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
}

impl AppBackend {
    pub fn new(
        connections: ConnectionManager,
        meters: Arc<RwLock<HashMap<String, MeterActorHandle>>>,
    ) -> Self {
        Self {
            connections,
            meters,
        }
    }

    /// Dispatch a backend command from GPUI's executor.
    ///
    /// UI callbacks run on the native window thread, which does not have a
    /// Tokio reactor.  Using the GPUI background executor keeps the command
    /// asynchronous without calling `tokio::spawn` outside a Tokio runtime.
    pub fn dispatch(&self, address: String, action: MeterAction, cx: &App) {
        let Some(handle) = self.meters.read().get(&address).cloned() else {
            tracing::warn!(%address, "meter command ignored because actor was not found");
            return;
        };
        let command = action.into();
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = handle.send_admin_command(command).await {
                    tracing::warn!(%address, %error, "meter command failed");
                }
            })
            .detach();
    }
}

impl Global for AppBackend {}
