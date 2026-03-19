# WatchDesk

A Windows Service written in Rust that publishes PC online status and monitor power state to [Home Assistant](https://www.home-assistant.io/) via MQTT.

## Features

- **Windows Service**: auto-starts at boot, runs in the background
- **Monitor power detection**: detects when your display turns on/off
- **MQTT with LWT**: Last Will and Testament ensures Home Assistant knows when your PC goes offline, even on crashes or network drops
- **HA auto-discovery**: automatically registers as a device in Home Assistant via MQTT discovery
- **UAC elevation**: install/uninstall commands automatically prompt for admin privileges
- **Foreground mode**: run interactively for debugging

## MQTT Topics

| Purpose | Topic | Values |
|---------|-------|--------|
| Availability (LWT) | `watchdesk/{name}/availability` | `online` / `offline` |
| Monitor state | `watchdesk/{name}/monitor/state` | `ON` / `OFF` |
| HA Discovery | `homeassistant/binary_sensor/watchdesk_{name}_monitor/config` | JSON (retained) |

## Installation

### Prerequisites

- An MQTT broker (e.g., Mosquitto) accessible from your PC
- Home Assistant with MQTT integration configured

### Build

```sh
cargo build --release
```

### Configure

Create a `config.toml` in your project root (or working directory):

```toml
[mqtt]
host = "192.168.1.100"
port = 1883
# username = "user"
# password = "pass"

[device]
name = "My Desktop"
```

The `name` field is used as the device name in Home Assistant. It gets slugified for MQTT topics (e.g., `"My Desktop"` becomes `my_desktop`).

### Install as a Service

```sh
watchdesk.exe install
```

This will copy `config.toml` from the current directory to `C:\ProgramData\WatchDesk\`, trigger a UAC prompt, install the service with auto-start, and start it immediately. The service reads its config from ProgramData, so you only need to maintain the one in your project root. Re-run install to update the config.

### Uninstall

```sh
watchdesk.exe uninstall
```

Stops and removes the Windows Service.

### Run in Foreground (Debug)

```sh
watchdesk.exe run
```

Runs interactively with console logging. Useful for testing your MQTT connection and monitor detection. Press `Ctrl+C` to stop.

## How It Works

### Monitor Detection

- **Service mode**: Uses `RegisterPowerSettingNotification` with `GUID_MONITOR_POWER_ON` via the service control handler
- **Foreground mode**: Creates a hidden window and listens for `WM_POWERBROADCAST` messages with `GUID_CONSOLE_DISPLAY_STATE`
- Dimmed state (2) is treated as "on"

### Offline Detection

- **Clean shutdown** (`sc stop` / `Ctrl+C`): The service publishes `offline` to the availability topic before disconnecting
- **Crash / network drop**: The MQTT broker publishes the LWT message (`offline`) after ~1.5x the keep-alive interval (~15 seconds with the default 10s keep-alive)

### Reconnection

The MQTT client (`rumqttc`) handles reconnection automatically. On reconnect, the service re-publishes the discovery config, availability status, and current monitor state.

## Home Assistant Example

Automation to control a desk light based on PC state:

```yaml
alias: PC Desk Light
triggers:
  - entity_id: binary_sensor.my_desktop
    trigger: state
actions:
  - choose:
      - conditions:
          - condition: state
            entity_id: binary_sensor.my_desktop
            state: "on"
        sequence:
          - action: light.turn_on
            target:
              entity_id: light.desk_lamp
    default:
      - action: light.turn_off
        target:
          entity_id: light.desk_lamp
```

The `default` branch handles both `off` and `unavailable` states.

## Project Structure

```
src/
├── main.rs       # CLI entry point (install/uninstall/run)
├── service.rs    # Windows Service integration (SCM, power events)
├── monitor.rs    # Monitor power state detection (Win32 API)
├── mqtt.rs       # MQTT client, HA auto-discovery, LWT
└── config.rs     # TOML config parsing
```

## License

MIT
