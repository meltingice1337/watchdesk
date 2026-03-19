use crate::config::Config;
use crate::monitor::MonitorState;
use windows::Win32::System::SystemServices::GUID_MONITOR_POWER_ON;
use crate::mqtt::MqttManager;
use log::{error, info};
use std::ffi::OsString;
use std::os::windows::io::AsRawHandle;
use std::time::Duration;
use tokio::sync::mpsc;
use windows::Win32::System::Power::RegisterPowerSettingNotification;
use windows::Win32::UI::WindowsAndMessaging::REGISTER_NOTIFICATION_FLAGS;
use windows_service::service::{
    PowerBroadcastSetting, PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode,
    ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

const SERVICE_NAME: &str = "WatchDesk";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

pub fn dispatch() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

windows_service::define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = init_file_logger() {
        eprintln!("Failed to init logger: {e}");
    }
    if let Err(e) = run_service() {
        error!("Service failed: {e}");
    }
}

fn init_file_logger() -> anyhow::Result<()> {
    use std::fs::OpenOptions;

    let exe = std::env::current_exe()?;
    let log_path = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine exe directory"))?
        .join("watchdesk.log");

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(file)))
        .init();

    info!("Logging to {}", log_path.display());
    Ok(())
}

fn run_service() -> anyhow::Result<()> {
    let (monitor_tx, monitor_rx) = mpsc::unbounded_channel::<MonitorState>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Register service control handler FIRST so SCM knows we're alive
    let status_handle = service_control_handler::register(
        SERVICE_NAME,
        move |control| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    info!("Service stop/shutdown requested");
                    let _ = shutdown_tx.send(true);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::PowerEvent(param) => {
                    if let PowerEventParam::PowerSettingChange(setting) = param {
                        let state = match setting {
                            PowerBroadcastSetting::MonitorPowerOn(s) => {
                                Some(MonitorState::from_power_value(s.to_raw()))
                            }
                            PowerBroadcastSetting::ConsoleDisplayState(s) => {
                                Some(MonitorState::from_power_value(s.to_raw() as u32))
                            }
                            _ => None,
                        };
                        if let Some(state) = state {
                            info!("Monitor power event from service: {state:?}");
                            let _ = monitor_tx.send(state);
                        }
                    }
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        },
    )?;

    // Set service to running
    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP
            | ServiceControlAccept::SHUTDOWN
            | ServiceControlAccept::POWER_EVENT,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    info!("Service is running");

    // Load config after we've reported Running
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load config: {e}");
            status_handle.set_service_status(ServiceStatus {
                service_type: SERVICE_TYPE,
                current_state: ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::Win32(1),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })?;
            return Err(e);
        }
    };

    // Register for monitor power notifications in service mode
    unsafe {
        let handle = windows::Win32::Foundation::HANDLE(status_handle.as_raw_handle() as *mut _);
        match RegisterPowerSettingNotification(
            handle,
            &GUID_MONITOR_POWER_ON,
            REGISTER_NOTIFICATION_FLAGS(1), // DEVICE_NOTIFY_SERVICE_HANDLE
        ) {
            Ok(h) => info!("Registered for GUID_MONITOR_POWER_ON (handle: {h:?})"),
            Err(e) => error!("Failed to register power notification: {e}"),
        }
    }

    // Run async main loop
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let (mqtt_manager, event_loop) = MqttManager::new(&config)?;
        mqtt_manager.run(event_loop, monitor_rx, shutdown_rx).await
    });

    if let Err(e) = &result {
        error!("Service error: {e}");
    }

    // Set service to stopped
    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    info!("Service stopped");
    Ok(())
}
