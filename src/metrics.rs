//! CPU metrics collection.
//!
//! - **Usage** is sampled in-process with `sysinfo` (no privileges needed).
//! - **Temperature** is polled from AMD's Ryzen Master SDK command line tool
//!   (`AMDRyzenMasterCLI.exe --api GetPMTableData`), whose output carries a
//!   `GetCurrentTemperature ..... NN.NN Celsius` line.
//!
//! Reading Ryzen Tctl requires a kernel driver, so we shell out to AMD's own
//! *signed* CLI instead of hosting a hardware-monitoring library ourselves.
//! Windows systems with Smart App Control enabled block unsigned executables
//! outright — including anything we compile — while AMD ships both the CLI and
//! `AMDRyzenMasterDriver.sys` signed, so this path adds no unsigned code.
//!
//! The driver needs elevation, which the service satisfies by running as
//! LocalSystem; under a plain `watchdesk run` shell the CLI reports
//! "User is not admin" and temperature stays unavailable.

use log::{info, warn};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::System;
use tokio::process::Command;

/// Give up on a single CLI invocation that hasn't returned by now.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
/// First retry delay after a failed read, doubled up to `MAX_BACKOFF`.
const INITIAL_BACKOFF: Duration = Duration::from_secs(15);
/// Ceiling for the retry delay, so a persistent failure stays quiet.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

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

/// Spawn a background task that polls `cli` and keeps the shared temperature
/// updated. Failures back off exponentially and are logged only when the error
/// changes, so an unreadable sensor can't flood the log.
pub fn spawn_temp_reader(cli: PathBuf, interval_secs: u64) -> SharedTemp {
    let shared: SharedTemp = Arc::new(Mutex::new(None));
    let out = shared.clone();
    let interval = Duration::from_secs(interval_secs.max(1));

    tokio::spawn(async move {
        info!("Reading CPU temperature via {}", cli.display());
        let mut backoff = INITIAL_BACKOFF;
        let mut last_err: Option<String> = None;
        let mut reported_ok = false;

        loop {
            match read_temperature(&cli).await {
                Ok(temp) => {
                    if !reported_ok {
                        info!("CPU temperature available: {temp:.1} °C");
                        reported_ok = true;
                    }
                    *out.lock().unwrap() = Some(temp);
                    backoff = INITIAL_BACKOFF;
                    last_err = None;
                    tokio::time::sleep(interval).await;
                }
                Err(e) => {
                    // Drop the stale reading so we never publish a value the
                    // sensor is no longer producing.
                    *out.lock().unwrap() = None;
                    reported_ok = false;

                    let msg = e.to_string();
                    if last_err.as_deref() != Some(msg.as_str()) {
                        warn!(
                            "CPU temperature unavailable: {msg} \
                             (retrying, backing off to at most {}s)",
                            MAX_BACKOFF.as_secs()
                        );
                        last_err = Some(msg);
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });

    shared
}

/// Run the CLI once and pull the CPU temperature out of its report.
async fn read_temperature(cli: &Path) -> anyhow::Result<f32> {
    if !cli.exists() {
        return Err(anyhow::anyhow!("{} not found", cli.display()));
    }

    let run = Command::new(cli)
        .args(["--api", "GetPMTableData"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();

    let output = tokio::time::timeout(CLI_TIMEOUT, run)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "{} did not return within {}s",
                cli.display(),
                CLI_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", cli.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_temperature(&stdout).ok_or_else(|| {
        // Surface whatever the tool actually said — typically
        // "User is not admin..." or a "Platform init failed" line.
        let detail = stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no output");
        anyhow::anyhow!("no temperature in CLI output ({detail})")
    })
}

/// Pull the reading out of a `GetCurrentTemperature ..... 47.41 Celsius` line.
fn parse_temperature(stdout: &str) -> Option<f32> {
    for line in stdout.lines() {
        let Some(rest) = line.trim_start().strip_prefix("GetCurrentTemperature") else {
            continue;
        };
        // `rest` looks like " ............................. 47.41 Celsius".
        let temp: f32 = rest
            .trim_matches(|c: char| c == '.' || c.is_whitespace())
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        // The SDK leaves its temperature field at -1 when unpopulated, and a
        // running CPU is never at 0 °C.
        if temp > 0.0 && temp < 150.0 {
            return Some(temp);
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_temperature;

    #[test]
    fn reads_the_temperature_line() {
        let out = "\
GetPMTableData .................................... cHTC Current Limit: 95.000000 celsius\r
GetPMTableData .................................... cHTC Current Value: 47.413761 celsius\r
GetCurrentTemperature ............................. 47.41 Celsius\r
GetAverageCoreVoltage ............................. 1.187500 V\r
";
        assert_eq!(parse_temperature(out), Some(47.41));
    }

    #[test]
    fn rejects_the_deprecated_api_response() {
        let out = "GetCurrentTemperature ............................. Deprecated API. Use GetPMTableData";
        assert_eq!(parse_temperature(out), None);
    }

    #[test]
    fn rejects_unpopulated_and_missing_values() {
        assert_eq!(
            parse_temperature("GetCurrentTemperature ......... -1.00 Celsius"),
            None
        );
        assert_eq!(parse_temperature("User is not admin..."), None);
        assert_eq!(parse_temperature(""), None);
    }
}
