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
            "device": {
                "identifiers": [format!("watchdesk_{slug}")],
                "name": &self.device_name,
                "manufacturer": "WatchDesk - meltingice1337",
                "model": "PC Presence Monitor"
            }
        });
        payload.to_string()
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
    pub async fn run(
        self,
        mut event_loop: rumqttc::EventLoop,
        mut monitor_rx: mpsc::UnboundedReceiver<MonitorState>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut current_state = MonitorState::On; // Assume on at startup

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
                        info!("Monitor state changed: {current_state:?} -> {state:?}");
                        current_state = state;
                        if let Err(e) = self.publish_monitor_state(state).await {
                            error!("Failed to publish state change: {e}");
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
