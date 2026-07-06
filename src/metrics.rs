//! CPU metrics collection.
//!
//! - **Usage** is sampled in-process with `sysinfo` (no privileges needed).
//! - **Temperature** comes from the bundled sensor sidecar (see
//!   `sidecar/WatchdeskSensors.cs`), which hosts LibreHardwareMonitor headless
//!   and streams JSON lines. We keep the latest value in a shared cell and
//!   restart the sidecar if it dies.

use log::{info, warn};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::System;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Global CPU-usage sampler backed by sysinfo.
pub struct CpuUsage {
    system: System,
}

impl CpuUsage {
    pub fn new() -> Self {
        let mut system = System::new();
        // Prime the first delta; the real reading comes on the next sample,
        // which is a full poll interval later (well beyond sysinfo's minimum).
        system.refresh_cpu_usage();
        Self { system }
    }

    /// Global CPU usage as a percentage (0–100).
    pub fn sample(&mut self) -> f32 {
        self.system.refresh_cpu_usage();
        self.system.global_cpu_usage()
    }
}

/// Latest CPU temperature in °C, or `None` when unavailable.
pub type SharedTemp = Arc<Mutex<Option<f32>>>;

/// Spawn a background task that runs the sensor sidecar and keeps the shared
/// temperature updated. The sidecar is restarted if it exits or errors.
pub fn spawn_temp_reader(exe: PathBuf, interval_secs: u64) -> SharedTemp {
    let shared: SharedTemp = Arc::new(Mutex::new(None));
    let out = shared.clone();
    tokio::spawn(async move {
        loop {
            match read_loop(&exe, interval_secs, &out).await {
                Ok(()) => warn!("sensor sidecar exited; restarting in 15s"),
                Err(e) => warn!("sensor sidecar unavailable: {e}; retrying in 15s"),
            }
            *out.lock().unwrap() = None; // temperature is now unavailable
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
    shared
}

async fn read_loop(exe: &PathBuf, interval_secs: u64, out: &SharedTemp) -> anyhow::Result<()> {
    if !exe.exists() {
        return Err(anyhow::anyhow!(
            "sidecar not found at {} (run `watchdesk install`)",
            exe.display()
        ));
    }

    let mut child = Command::new(exe)
        .arg(interval_secs.to_string())
        .arg("0") // run forever
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture sidecar stdout"))?;
    info!("sensor sidecar started (pid {:?})", child.id());

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Lines look like {"cpu_temp_c":50.4} or {"cpu_temp_c":null}.
        // Ignore anything that doesn't parse (leave the last value in place).
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let temp = v.get("cpu_temp_c").and_then(|x| x.as_f64()).map(|f| f as f32);
            *out.lock().unwrap() = temp;
        }
    }

    let _ = child.wait().await;
    Ok(())
}
