use crate::config::Config;
use crate::monitor::MonitorState;
use log::{error, info, warn};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct MqttManager {
    client: AsyncClient,
    device_name: String,
    metrics: crate::config::MetricsConfig,
}

impl MqttManager {
    pub fn new(config: &Config) -> anyhow::Result<(Self, rumqttc::EventLoop)> {
        let client_id = format!("watchdesk_{}", config.device.name);
        let mut options = MqttOptions::new(&client_id, &config.mqtt.host, config.mqtt.port);

        options.set_keep_alive(Duration::from_secs(30));
        options.set_clean_session(true);

        if let (Some(user), Some(pass)) = (&config.mqtt.username, &config.mqtt.password) {
            options.set_credentials(user, pass);
        }

        // Configure Last Will and Testament
        let slug = config
            .device
            .name
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric(), "_");
        let availability_topic = format!("watchdesk/{slug}/availability");
        let last_will =
            rumqttc::LastWill::new(&availability_topic, "offline", QoS::AtLeastOnce, true);
        options.set_last_will(last_will);

        let (client, event_loop) = AsyncClient::new(options, 10);

        let manager = Self {
            client,
            device_name: config.device.name.clone(),
            metrics: config.metrics.clone(),
        };

        Ok((manager, event_loop))
    }

    fn slug(&self) -> String {
        self.device_name
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric(), "_")
    }

    fn availability_topic(&self) -> String {
        format!("watchdesk/{}/availability", self.slug())
    }

    fn state_topic(&self) -> String {
        format!("watchdesk/{}/monitor/state", self.slug())
    }

    fn discovery_topic(&self) -> String {
        format!(
            "homeassistant/binary_sensor/watchdesk_{}_monitor/config",
            self.slug()
        )
    }

    /// Shared Home Assistant `device` block so all entities group under one device.
    fn device_json(&self) -> serde_json::Value {
        let slug = self.slug();
        json!({
            "identifiers": [format!("watchdesk_{slug}")],
            "name": &self.device_name,
            "manufacturer": "WatchDesk - meltingice1337",
            "model": "PC Status & Sensors"
        })
    }

    fn discovery_payload(&self) -> String {
        let slug = self.slug();
        let payload = json!({
            "name": serde_json::Value::Null,
            "icon": "mdi:desktop-tower",
            "state_topic": self.state_topic(),
            "payload_on": "ON",
            "payload_off": "OFF",
            "availability_topic": self.availability_topic(),
            "payload_available": "online",
            "payload_not_available": "offline",
            "unique_id": format!("watchdesk_{slug}_power"),
            "device": self.device_json()
        });
        payload.to_string()
    }

    // --- CPU usage sensor ---

    fn cpu_usage_state_topic(&self) -> String {
        format!("watchdesk/{}/cpu/usage", self.slug())
    }

    fn cpu_usage_discovery_topic(&self) -> String {
        format!(
            "homeassistant/sensor/watchdesk_{}_cpu_usage/config",
            self.slug()
        )
    }

    fn cpu_usage_discovery_payload(&self) -> String {
        let slug = self.slug();
        json!({
            "name": "CPU Usage",
            "icon": "mdi:cpu-64-bit",
            "state_topic": self.cpu_usage_state_topic(),
            "unit_of_measurement": "%",
            "state_class": "measurement",
            "suggested_display_precision": 0,
            "availability_topic": self.availability_topic(),
            "payload_available": "online",
            "payload_not_available": "offline",
            "unique_id": format!("watchdesk_{slug}_cpu_usage"),
            "device": self.device_json()
        })
        .to_string()
    }

    // --- CPU temperature sensor ---

    fn cpu_temp_state_topic(&self) -> String {
        format!("watchdesk/{}/cpu/temperature", self.slug())
    }

    fn cpu_temp_discovery_topic(&self) -> String {
        format!(
            "homeassistant/sensor/watchdesk_{}_cpu_temp/config",
            self.slug()
        )
    }

    fn cpu_temp_discovery_payload(&self) -> String {
        let slug = self.slug();
        json!({
            "name": "CPU Temperature",
            "device_class": "temperature",
            "state_topic": self.cpu_temp_state_topic(),
            "unit_of_measurement": "°C",
            "state_class": "measurement",
            "suggested_display_precision": 1,
            "availability_topic": self.availability_topic(),
            "payload_available": "online",
            "payload_not_available": "offline",
            "unique_id": format!("watchdesk_{slug}_cpu_temp"),
            "device": self.device_json()
        })
        .to_string()
    }

    async fn publish_cpu_discovery(&self) -> anyhow::Result<()> {
        if self.metrics.cpu_usage {
            self.client
                .publish(
                    self.cpu_usage_discovery_topic(),
                    QoS::AtLeastOnce,
                    true,
                    self.cpu_usage_discovery_payload(),
                )
                .await?;
        }
        if self.metrics.cpu_temp {
            self.client
                .publish(
                    self.cpu_temp_discovery_topic(),
                    QoS::AtLeastOnce,
                    true,
                    self.cpu_temp_discovery_payload(),
                )
                .await?;
        }
        Ok(())
    }

    async fn publish_cpu_usage(&self, usage: f32) -> anyhow::Result<()> {
        self.client
            .publish(
                self.cpu_usage_state_topic(),
                QoS::AtLeastOnce,
                true,
                format!("{usage:.1}"),
            )
            .await?;
        Ok(())
    }

    async fn publish_cpu_temp(&self, temp: f32) -> anyhow::Result<()> {
        self.client
            .publish(
                self.cpu_temp_state_topic(),
                QoS::AtLeastOnce,
                true,
                format!("{temp:.1}"),
            )
            .await?;
        Ok(())
    }

    async fn publish_online(&self) -> anyhow::Result<()> {
        self.client
            .publish(self.availability_topic(), QoS::AtLeastOnce, true, "online")
            .await?;
        Ok(())
    }

    async fn publish_discovery(&self) -> anyhow::Result<()> {
        self.client
            .publish(
                self.discovery_topic(),
                QoS::AtLeastOnce,
                true,
                self.discovery_payload(),
            )
            .await?;
        Ok(())
    }

    async fn publish_monitor_state(&self, state: MonitorState) -> anyhow::Result<()> {
        self.client
            .publish(self.state_topic(), QoS::AtLeastOnce, true, state.as_str())
            .await?;
        Ok(())
    }

    /// Run the MQTT manager, processing events and monitor state changes.
    /// Monitor state changes are debounced (3s) to avoid rapid on/off flips during boot.
    pub async fn run(
        self,
        mut event_loop: rumqttc::EventLoop,
        mut monitor_rx: mpsc::UnboundedReceiver<MonitorState>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut current_state = MonitorState::On; // Assume on at startup
        let mut pending_state: Option<MonitorState> = None;
        let debounce = tokio::time::sleep(Duration::MAX);
        tokio::pin!(debounce);

        // CPU metrics: sample usage in-process, and (optionally) read temperature
        // from the sensor sidecar. First tick fires one interval in, so the first
        // usage sample spans a real window rather than a near-zero delta.
        let mut cpu_usage = crate::metrics::CpuUsage::new();
        let latest_temp = if self.metrics.cpu_temp {
            Some(crate::metrics::spawn_temp_reader(
                Config::sensors_exe_path(),
                self.metrics.interval_secs,
            ))
        } else {
            None
        };
        let poll = Duration::from_secs(self.metrics.interval_secs.max(1));
        let mut metrics_tick = tokio::time::interval_at(tokio::time::Instant::now() + poll, poll);

        loop {
            tokio::select! {
                event = event_loop.poll() => {
                    match event {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            info!("MQTT connected");
                            if let Err(e) = self.publish_online().await {
                                error!("Failed to publish online: {e}");
                            }
                            if let Err(e) = self.publish_discovery().await {
                                error!("Failed to publish discovery: {e}");
                            }
                            if let Err(e) = self.publish_cpu_discovery().await {
                                error!("Failed to publish CPU discovery: {e}");
                            }
                            info!("Published HA auto-discovery config");
                            if let Err(e) = self.publish_monitor_state(current_state).await {
                                error!("Failed to publish initial state: {e}");
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("MQTT error (will reconnect): {e}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                Some(state) = monitor_rx.recv() => {
                    if state != current_state {
                        info!("Monitor state event: {current_state:?} -> {state:?} (debouncing)");
                        pending_state = Some(state);
                        debounce.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3));
                    } else {
                        // New event matches current published state, cancel any pending change
                        if pending_state.is_some() {
                            info!("Monitor state reverted to {current_state:?}, cancelling pending change");
                            pending_state = None;
                            debounce.as_mut().reset(tokio::time::Instant::now() + Duration::MAX);
                        }
                    }
                }
                () = &mut debounce, if pending_state.is_some() => {
                    let state = pending_state.take().unwrap();
                    info!("Monitor state confirmed: {current_state:?} -> {state:?}");
                    current_state = state;
                    if let Err(e) = self.publish_monitor_state(state).await {
                        error!("Failed to publish state change: {e}");
                    }
                    debounce.as_mut().reset(tokio::time::Instant::now() + Duration::MAX);
                }
                _ = metrics_tick.tick() => {
                    if self.metrics.cpu_usage {
                        let usage = cpu_usage.sample();
                        if let Err(e) = self.publish_cpu_usage(usage).await {
                            error!("Failed to publish CPU usage: {e}");
                        }
                    }
                    // Publish temperature only when the sidecar has a real reading;
                    // otherwise leave the sensor's last value in place.
                    if let Some(temp) = &latest_temp {
                        let value = *temp.lock().unwrap();
                        if let Some(t) = value {
                            if let Err(e) = self.publish_cpu_temp(t).await {
                                error!("Failed to publish CPU temp: {e}");
                            }
                        }
                    }
                }
                _ = shutdown.changed() => {
                    info!("MQTT manager shutting down");
                    // Publish offline before exiting (best effort)
                    let _ = self.client
                        .publish(self.availability_topic(), QoS::AtLeastOnce, true, "offline")
                        .await;
                    // Give a moment for the message to be sent
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    break;
                }
            }
        }

        Ok(())
    }
}
