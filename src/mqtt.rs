use crate::config::Config;
use crate::monitor::MonitorState;
use log::{error, info, warn};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct MqttManager {
    client: AsyncClient,
    device_name: String,
    metrics: crate::config::MetricsConfig,
    /// Resolved path to AMD's Ryzen Master CLI, or `None` if the SDK is absent.
    temp_cli: Option<PathBuf>,
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
            temp_cli: config.ryzen_master_cli_path(),
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
            // Deliberately NOT tied to the availability topic. The retained
            // state already says what the monitor is doing, and an unreachable
            // PC means the monitor is off — so "unavailable" adds nothing here
            // and actively hurts: it turns every sleep into an
            // off -> unavailable -> off -> on burst in Home Assistant, which
            // races automations instead of giving them one clean transition.
            // The CPU sensors still use the availability topic, so a dead
            // service shows them as Unavailable rather than stale.
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

    /// Per-sensor availability, so temperature can go unavailable on its own
    /// while the rest of the device stays online.
    fn cpu_temp_availability_topic(&self) -> String {
        format!("watchdesk/{}/cpu/temperature/availability", self.slug())
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
            // Unavailable when the service is down *or* when the sensor has no
            // reading, so HA shows "Unavailable" instead of a stale value.
            "availability_mode": "all",
            "availability": [
                { "topic": self.availability_topic() },
                { "topic": self.cpu_temp_availability_topic() }
            ],
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

    /// Sync and non-blocking: `try_publish` drops the sample when the request
    /// channel is full instead of awaiting. Awaiting here deadlocks the select
    /// loop, because the only thing that drains that channel is
    /// `event_loop.poll()` — a sibling branch that cannot run while this body is
    /// suspended. A stale metrics sample is worthless anyway, so dropping is right.
    fn publish_cpu_usage(&self, usage: f32) -> anyhow::Result<()> {
        self.client.try_publish(
            self.cpu_usage_state_topic(),
            QoS::AtLeastOnce,
            true,
            format!("{usage:.1}"),
        )?;
        Ok(())
    }

    fn publish_cpu_temp(&self, temp: f32) -> anyhow::Result<()> {
        self.client.try_publish(
            self.cpu_temp_state_topic(),
            QoS::AtLeastOnce,
            true,
            format!("{temp:.1}"),
        )?;
        Ok(())
    }

    fn publish_cpu_temp_available(&self, available: bool) -> anyhow::Result<()> {
        self.client.try_publish(
            self.cpu_temp_availability_topic(),
            QoS::AtLeastOnce,
            true,
            if available { "online" } else { "offline" },
        )?;
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
        // Track whether we're connected to the broker. Publishing while
        // disconnected backs up the client's bounded request channel; once it's
        // full, `publish().await` blocks the whole select loop and we can never
        // observe shutdown — which is exactly what wedges the service on stop.
        let mut connected = false;
        let debounce = tokio::time::sleep(Duration::MAX);
        tokio::pin!(debounce);

        // CPU metrics: sample usage in-process, and (optionally) read temperature
        // from the sensor sidecar. First tick fires one interval in, so the first
        // usage sample spans a real window rather than a near-zero delta.
        let mut cpu_usage = crate::metrics::CpuUsage::new();
        let latest_temp = match (self.metrics.cpu_temp, self.temp_cli.clone()) {
            (true, Some(cli)) => Some(crate::metrics::spawn_temp_reader(
                cli,
                self.metrics.interval_secs,
            )),
            (true, None) => {
                warn!(
                    "CPU temperature is enabled but the AMD Ryzen Master SDK was not \
                     found; install it or set [metrics] ryzen_master_cli in config.toml"
                );
                None
            }
            (false, _) => None,
        };
        // `None` until published once, so the first tick always sends it.
        let mut temp_available: Option<bool> = None;
        let poll = Duration::from_secs(self.metrics.interval_secs.max(1));
        let mut metrics_tick = tokio::time::interval_at(tokio::time::Instant::now() + poll, poll);
        // Suspend/resume leaves hours of missed deadlines behind. The default
        // `Burst` behaviour replays every one of them back-to-back, which keeps
        // this branch permanently ready and starves `event_loop.poll()`. We only
        // ever want the *current* reading, so collapse the backlog into one tick.
        metrics_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                event = event_loop.poll() => {
                    match event {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            info!("MQTT connected");
                            connected = true;
                            // Re-send the temperature sensor's availability on a
                            // fresh session rather than relying on retention.
                            temp_available = None;
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
                            connected = false;
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
                        }
                    }
                }
                // `pending_state` is the only thing that arms or disarms this branch.
                // Never `reset()` to a `Duration::MAX` deadline to "park" the timer:
                // `Instant + Duration::MAX` overflows and panics (unlike `sleep()`,
                // which saturates internally). The guard disables the branch instead,
                // so an already-elapsed timer is simply never polled.
                () = &mut debounce, if pending_state.is_some() => {
                    let state = pending_state.take().unwrap();
                    info!("Monitor state confirmed: {current_state:?} -> {state:?}");
                    current_state = state;
                    // Only publish when connected; the ConnAck handler republishes
                    // current_state on reconnect, so nothing is lost by skipping here.
                    if connected {
                        if let Err(e) = self.publish_monitor_state(state).await {
                            error!("Failed to publish state change: {e}");
                        }
                    }
                }
                _ = metrics_tick.tick() => {
                    // Keep sampling so the usage delta window stays current, but only
                    // publish while connected — otherwise these ticks back up the
                    // request channel and eventually block the loop (wedging stop).
                    if self.metrics.cpu_usage {
                        let usage = cpu_usage.sample();
                        if connected {
                            if let Err(e) = self.publish_cpu_usage(usage) {
                                error!("Failed to publish CPU usage: {e}");
                            }
                        }
                    }
                    // Publish temperature when there's a reading, and mark the
                    // sensor unavailable when there isn't — otherwise HA keeps
                    // showing the last value indefinitely. This runs whenever
                    // the sensor is configured, including when no reader could
                    // be started, so a retained "online" from an earlier run
                    // can't resurrect a stale reading.
                    if connected && self.metrics.cpu_temp {
                        let value = latest_temp.as_ref().and_then(|t| *t.lock().unwrap());
                        if let Some(t) = value {
                            if let Err(e) = self.publish_cpu_temp(t) {
                                error!("Failed to publish CPU temp: {e}");
                            }
                        }
                        // Only on change; the topic is retained.
                        let available = value.is_some();
                        if temp_available != Some(available) {
                            match self.publish_cpu_temp_available(available) {
                                Ok(()) => {
                                    info!(
                                        "CPU temperature sensor is now {}",
                                        if available { "available" } else { "unavailable" }
                                    );
                                    temp_available = Some(available);
                                }
                                Err(e) => {
                                    error!("Failed to publish CPU temp availability: {e}")
                                }
                            }
                        }
                    }
                }
                _ = shutdown.changed() => {
                    info!("MQTT manager shutting down");
                    // Best-effort retained "offline", but hard-bounded by a timeout so a
                    // stalled or unreachable broker can never delay shutdown. When we're
                    // not connected, the broker's Last Will already marks us offline, so
                    // there's nothing to send.
                    if connected {
                        let _ = tokio::time::timeout(Duration::from_millis(500), async {
                            // The monitor sensor has no availability topic, so
                            // leave it holding a truthful retained value: a PC
                            // that is shutting down is a monitor that is off.
                            let _ = self
                                .client
                                .publish(self.state_topic(), QoS::AtLeastOnce, true, "OFF")
                                .await;
                            let _ = self
                                .client
                                .publish(self.availability_topic(), QoS::AtLeastOnce, true, "offline")
                                .await;
                            // Poll once so the packet is flushed before we drop the loop.
                            let _ = event_loop.poll().await;
                        })
                        .await;
                    }
                    break;
                }
            }
        }

        Ok(())
    }
}
