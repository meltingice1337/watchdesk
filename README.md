# WatchDesk

A Windows Service written in Rust that publishes PC online status and monitor power state to [Home Assistant](https://www.home-assistant.io/) via MQTT.

## Features

- **Windows Service**: auto-starts at boot, runs in the background
- **Monitor power detection**: detects when your display turns on/off
- **CPU metrics**: publishes CPU usage (%) and, on AMD Ryzen with the Ryzen Master SDK installed, CPU temperature (°C)
- **Optional Bluetooth shutdown**: can turn off the Windows Bluetooth radio on startup
- **MQTT with LWT**: Last Will and Testament ensures Home Assistant knows when your PC goes offline, even on crashes or network drops
- **HA auto-discovery**: automatically registers as a device in Home Assistant via MQTT discovery
- **UAC elevation**: install/uninstall commands automatically prompt for admin privileges
- **Foreground mode**: run interactively for debugging

## MQTT Topics

| Purpose | Topic | Values |
|---------|-------|--------|
| Availability (LWT) | `watchdesk/{name}/availability` | `online` / `offline` |
| Monitor state | `watchdesk/{name}/monitor/state` | `ON` / `OFF` |
| CPU usage | `watchdesk/{name}/cpu/usage` | percent, e.g. `12.5` |
| CPU temperature | `watchdesk/{name}/cpu/temperature` | °C, e.g. `50.4` |
| CPU temp availability | `watchdesk/{name}/cpu/temperature/availability` | `online` / `offline` |
| HA Discovery | `homeassistant/{binary_sensor,sensor}/watchdesk_{name}_*/config` | JSON (retained) |

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

[startup]
# Optional: once per Windows boot, turn off the Windows Bluetooth radio.
turn_off_bluetooth = false
```

The `name` field is used as the device name in Home Assistant. It gets slugified for MQTT topics (e.g., `"My Desktop"` becomes `my_desktop`).

### Startup Actions

Configured under `[startup]` in `config.toml`:

```toml
[startup]
turn_off_bluetooth = true
```

When enabled, WatchDesk runs a one-shot startup action once per Windows boot that turns the active Windows Bluetooth radio off using Microsoft's `Windows.Devices.Radios` API. This matches the Windows Settings/Quick Settings Bluetooth toggle more closely than disabling the adapter in Device Manager. Later WatchDesk restarts during the same boot skip the action. If Windows rejects the radio change, WatchDesk logs the error and keeps running.

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

### CPU Metrics

Configured under `[metrics]` in `config.toml` (all optional; defaults shown):

```toml
[metrics]
interval_secs = 5   # how often to sample and publish
cpu_usage = true    # global CPU usage (%)
cpu_temp = true     # CPU temperature (°C)
# ryzen_master_cli = '...\AMDRyzenMasterCLI.exe'   # override the CLI path
```

- **Usage** is sampled in-process with [`sysinfo`](https://crates.io/crates/sysinfo) — no special privileges, works everywhere.
- **Temperature** is read by invoking AMD's **Ryzen Master SDK CLI** (`AMDRyzenMasterCLI.exe --api GetPMTableData`) and parsing its `GetCurrentTemperature` line. Windows exposes no usable public API for CPU package temperature — on Ryzen it lives in the SMU and needs a kernel driver, and ACPI thermal zones aren't populated on AMD desktop platforms.

  This requires an AMD Ryzen CPU with the [Ryzen Master SDK](https://www.amd.com/en/developer/ryzen-master-monitoring-sdk.html) installed; WatchDesk **does not bundle it**, since AMD's licence forbids redistribution. Set `ryzen_master_cli` if it lives somewhere other than the default install path.

  Shelling out to AMD's *signed* CLI is deliberate. Reading the sensor needs a kernel driver, and Windows 11's **Smart App Control** blocks unsigned executables outright — including anything WatchDesk compiles itself. AMD ships both the CLI and its driver signed, so nothing unsigned sits in the temperature path.

  The driver needs elevation, so readings only appear under the **service** (which runs as LocalSystem); a plain `watchdesk run` shell reports unavailable. Whenever there's no reading, the temperature entity is published as **unavailable** rather than holding a stale value, and CPU usage keeps working regardless.

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
├── main.rs       # CLI entry point (install/uninstall/run/status)
├── service.rs    # Windows Service integration (SCM, power events)
├── monitor.rs    # Monitor power state detection (Win32 API)
├── mqtt.rs       # MQTT client, HA auto-discovery, LWT
├── metrics.rs    # CPU usage (sysinfo) + temperature (Ryzen Master CLI)
└── config.rs     # TOML config parsing
```

## License

MIT
