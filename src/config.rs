use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mqtt: MqttConfig,
    pub device: DeviceConfig,
    #[serde(default)]
    pub settings: SettingsConfig,
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

#[derive(Debug, Deserialize)]
pub struct SettingsConfig {
    #[serde(default = "default_heartbeat")]
    pub heartbeat_interval_secs: u64,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: default_heartbeat(),
        }
    }
}

fn default_port() -> u16 {
    1883
}

fn default_heartbeat() -> u64 {
    60
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path()?;
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read config at {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}

fn config_path() -> anyhow::Result<PathBuf> {
    // Check current working directory first, then next to the executable
    let cwd_path = PathBuf::from("config.toml");
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine executable directory"))?;
    Ok(dir.join("config.toml"))
}
