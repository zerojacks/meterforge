//! Application services. This module is the only UI-side boundary that knows
//! about `meter-core`; views submit intents and render snapshots.

mod bootstrap;
mod commands;

pub use bootstrap::initialize;
pub use commands::{AppBackend, MeterAction, NewMeterHandle};
