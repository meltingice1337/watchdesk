use serde::Deserialize;
use std::path::PathBuf;

/// Default install location of AMD's Ryzen Master SDK CLI, which WatchDesk
/// shells out to for CPU temperature.
const RYZEN_MASTER_CLI: &str =
    r"C:\Program Files\AMD\RyzenMasterSDK\AMDRyzenMasterCLI\bin-prebuilt\AMDRyzenMasterCLI.exe";

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mqtt: MqttConfig,
    pub device: DeviceConfig,
    #[serde(default)]
    pub startup: StartupConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

#[derive(Debug, Deserialize)]
pub struct MqttConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceConfig {
    pub name: String,
}

/// Optional one-shot actions to run when WatchDesk starts.
#[derive(Debug, Deserialize, Clone)]
pub struct StartupConfig {
    /// Turn the Windows Bluetooth radio off on startup. Useful when
    /// auto-starting the service at boot to prevent headphones from being
    /// stolen from a phone.
    #[serde(default, alias = "disable_bluetooth")]
    pub turn_off_bluetooth: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            turn_off_bluetooth: false,
        }
    }
}

/// Optional CPU metrics (usage + temperature). Everything defaults on, so an
/// existing config without a `[metrics]` section keeps working and gains the
/// sensors automatically.
#[derive(Debug, Deserialize, Clone)]
pub struct MetricsConfig {
    /// How often to sample and publish, in seconds.
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// Publish global CPU usage (%). Pure Rust via sysinfo; always works.
    #[serde(default = "default_true")]
    pub cpu_usage: bool,
    /// Publish CPU temperature (°C). Read by invoking AMD's Ryzen Master SDK
    /// CLI, so it needs that SDK installed and an AMD Ryzen CPU. The CLI's
    /// driver requires elevation, which the service satisfies by running as
    /// LocalSystem.
    #[serde(default = "default_true")]
    pub cpu_temp: bool,
    /// Override the path to `AMDRyzenMasterCLI.exe`. Leave unset to use the
    /// SDK's default install location.
    pub ryzen_master_cli: Option<PathBuf>,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            cpu_usage: true,
            cpu_temp: true,
            ryzen_master_cli: None,
        }
    }
}

fn default_port() -> u16 {
    1883
}

fn default_interval() -> u64 {
    5
}

fn default_true() -> bool {
    true
}

impl Config {
    /// The project-root `config.toml`, embedded at build time. Used as the
    /// fallback during `install` when no local `config.toml` is present.
    pub const DEFAULT_CONFIG: &'static str = include_str!("../config.toml");

    pub fn load() -> anyhow::Result<Self> {
        let path = config_path()?;
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read config at {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn programdata_dir() -> PathBuf {
        PathBuf::from(r"C:\ProgramData\WatchDesk")
    }

    pub fn programdata_config_path() -> PathBuf {
        Self::programdata_dir().join("config.toml")
    }

    /// Path to the Ryzen Master SDK CLI used for CPU temperature, or `None`
    /// when the SDK isn't installed (temperature then stays unreported).
    ///
    /// An explicit override wins and is returned even if it doesn't exist, so
    /// a typo surfaces as an error in the log rather than silently disabling
    /// the sensor.
    ///
    /// WatchDesk never bundles the SDK — AMD's licence forbids redistributing
    /// it — so this only ever points at a copy the user installed themselves.
    pub fn ryzen_master_cli_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.metrics.ryzen_master_cli {
            return Some(path.clone());
        }
        let default = PathBuf::from(RYZEN_MASTER_CLI);
        default.exists().then_some(default)
    }
}

fn config_path() -> anyhow::Result<PathBuf> {
    // Check current working directory first (for dev/foreground mode)
    let cwd_path = PathBuf::from("config.toml");
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    // Then check ProgramData (for service mode)
    let pd_path = Config::programdata_config_path();
    if pd_path.exists() {
        return Ok(pd_path);
    }

    Err(anyhow::anyhow!(
        "config.toml not found in current directory or {}",
        pd_path.display()
    ))
}
