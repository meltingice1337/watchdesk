use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mqtt: MqttConfig,
    pub device: DeviceConfig,
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
    /// Publish CPU temperature (°C). Read via the bundled LibreHardwareMonitor
    /// sidecar; requires the service (LocalSystem) so its driver can load.
    #[serde(default = "default_true")]
    pub cpu_temp: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            cpu_usage: true,
            cpu_temp: true,
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

    /// Directory holding the sensor sidecar and its bundled DLLs.
    pub fn programdata_sensors_dir() -> PathBuf {
        Self::programdata_dir().join("sensors")
    }

    /// Full path to the sensor sidecar executable the service spawns.
    pub fn sensors_exe_path() -> PathBuf {
        Self::programdata_sensors_dir().join("watchdesk-sensors.exe")
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
