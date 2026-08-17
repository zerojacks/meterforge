use chrono::Local;
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct CommunicationLogEntry {
    pub timestamp_ms: i64,
    pub direction: &'static str,
    pub channel: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CommunicationLogService {
    entries: Arc<Mutex<VecDeque<CommunicationLogEntry>>>,
    events: broadcast::Sender<CommunicationLogEntry>,
    file_tx: mpsc::Sender<CommunicationLogEntry>,
}

impl CommunicationLogService {
    pub fn new(path: PathBuf) -> Self {
        let entries = Arc::new(Mutex::new(VecDeque::with_capacity(1_000)));
        let (events, _) = broadcast::channel(1_000);
        let (file_tx, file_rx) = mpsc::channel::<CommunicationLogEntry>();
        thread::spawn(move || {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            while let Ok(entry) = file_rx.recv() {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
                    let bytes = entry
                        .data
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = writeln!(
                        file,
                        "{} {} {} {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                        entry.direction,
                        entry.channel,
                        bytes
                    );
                }
            }
        });
        Self {
            entries,
            events,
            file_tx,
        }
    }

    pub fn record(&self, direction: &'static str, channel: impl Into<String>, data: &[u8]) {
        let entry = CommunicationLogEntry {
            timestamp_ms: Local::now().timestamp_millis(),
            direction,
            channel: channel.into(),
            data: data.to_vec(),
        };
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == 1_000 {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }
        let _ = self.file_tx.send(entry.clone());
        let _ = self.events.send(entry);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<CommunicationLogEntry> {
        self.events.subscribe()
    }
    pub fn entries(&self) -> Vec<CommunicationLogEntry> {
        self.entries
            .lock()
            .map(|items| items.iter().cloned().collect())
            .unwrap_or_default()
    }
}
