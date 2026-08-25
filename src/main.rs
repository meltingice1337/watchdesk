mod config;
mod metrics;
mod monitor;
mod mqtt;
mod service;
mod startup;

use clap::{CommandFactory, Parser, Subcommand};
use log::info;
use std::ffi::{OsStr, OsString};
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::PCWSTR;
use windows::core::w;
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "WatchDesk";
const DISPLAY_NAME: &str = "WatchDesk PC Status & Sensors";

#[derive(Parser)]
#[command(
    name = "watchdesk",
    about = "PC status & sensors for Home Assistant (monitor state, CPU usage & temperature)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install as a Windows Service (auto-start)
    Install,
    /// Uninstall the Windows Service
    Uninstall,
    /// Run in foreground mode (for debugging)
    Run,
    /// Show service state, active config, and recent log lines
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Install) => cmd_install(),
        Some(Command::Uninstall) => cmd_uninstall(),
        Some(Command::Run) => cmd_run(),
        Some(Command::Status) => cmd_status(),
        None => match service::dispatch() {
            Ok(()) => Ok(()),
            // ERROR_FAILED_SERVICE_CONTROLLER_CONNECT (1063): we were started from a
            // shell, not by the Service Control Manager — show help instead of erroring.
            Err(windows_service::Error::Winapi(ref e)) if e.raw_os_error() == Some(1063) => {
                print_help_hint();
                Ok(())
            }
            Err(e) => Err(e.into()),
        },
    }
}

/// Hint + CLI help shown when the exe is run directly instead of by the SCM.
fn print_help_hint() {
    eprintln!(
        "WatchDesk is a background Windows service and has no default interactive action.\n\
         Use one of the commands below (try `watchdesk status` or `watchdesk install`).\n"
    );
    let _ = Cli::command().print_help();
    println!();
}

/// Re-launch the current exe elevated via UAC with the given subcommand.
/// Waits for the elevated process to finish and returns its exit code.
fn run_elevated(subcommand: &str) -> anyhow::Result<()> {
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};

    let exe = std::env::current_exe()?;
    let exe_wide: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let args_wide: Vec<u16> = subcommand
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND::default(),
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe_wide.as_ptr()),
        lpParameters: PCWSTR(args_wide.as_ptr()),
        nShow: windows::Win32::UI::WindowsAndMessaging::SW_HIDE.0 as i32,
        ..Default::default()
    };

    unsafe {
        ShellExecuteExW(&mut sei)?;
    }

    let process = sei.hProcess;
    if process.is_invalid() {
        return Err(anyhow::anyhow!("Failed to get elevated process handle"));
    }

    // Wait for the elevated process and relay its exit code
    unsafe {
        let _ = WaitForSingleObject(process, INFINITE);
        let mut exit_code = 0u32;
        let _ = GetExitCodeProcess(process, &mut exit_code);
        let _ = windows::Win32::Foundation::CloseHandle(process);
        if exit_code != 0 {
            return Err(anyhow::anyhow!(
                "Elevated process exited with code {exit_code}"
            ));
        }
    }

    Ok(())
}

use std::os::windows::ffi::OsStrExt;

fn is_elevated() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Path the service binary is installed to and run from (kept separate from the
/// build output so a running service never locks `target\release\watchdesk.exe`).
fn installed_exe_path() -> std::path::PathBuf {
    config::Config::programdata_dir().join("watchdesk.exe")
}

/// Remove the LibreHardwareMonitor sensor sidecar bundle that earlier versions
/// installed. Temperature now comes from AMD's signed Ryzen Master CLI, so these
/// files are dead weight — and on machines with Smart App Control the sidecar was
/// blocked from launching anyway.
fn remove_legacy_sensor_dir() {
    let dir = config::Config::programdata_dir().join("sensors");
    if !dir.exists() {
        return;
    }

    // A sidecar from an earlier run may still be alive (Windows doesn't kill
    // child processes with their parent) and would hold these files locked.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "watchdesk-sensors.exe", "/T"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    std::thread::sleep(Duration::from_millis(400));

    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!("Removed legacy sensor sidecar at {}", dir.display()),
        Err(e) => eprintln!(
            "Note: could not remove {} ({e}). Delete it manually if needed.",
            dir.display()
        ),
    }
}

/// Where the elevated install/uninstall half records its outcome so the
/// (non-elevated) parent — which can't see the hidden window's output — can
/// surface the real error instead of just "exited with code 1".
fn install_log_path() -> std::path::PathBuf {
    config::Config::programdata_dir().join("install.log")
}

fn log_elevated_result(action: &str, result: &anyhow::Result<()>) {
    let msg = match result {
        Ok(()) => format!("{action}: success\n"),
        Err(e) => format!("{action}: FAILED\n{e:#}\n"),
    };
    let _ = std::fs::write(install_log_path(), msg);
}

/// Terminate any leftover `watchdesk.exe` running in session 0 (the services
/// session) so the copy below can overwrite the installed binary.
///
/// A prior service process can keep the binary locked after outliving its SCM
/// registration (a shutdown hang leaves it running once the entry is gone) or
/// when the SCM restarts a crash-looping service. Callers should disarm the
/// service's restart actions first (see `disarm_failure_actions`) so a fresh
/// one can't respawn.
///
/// We shell out to `taskkill /F` rather than calling `TerminateProcess`
/// (directly or via sysinfo) because taskkill enables `SeDebugPrivilege`, which
/// our elevated-but-not-SYSTEM install needs to terminate a LocalSystem process
/// — a plain terminate silently fails on one. The `SESSION eq 0` filter targets
/// only service processes: our own elevated process and the launching shell run
/// in the interactive session (>= 1), so they're never matched.
fn kill_stale_installed_processes() {
    let killed = std::process::Command::new("taskkill")
        .args([
            "/F",
            "/FI",
            "IMAGENAME eq watchdesk.exe",
            "/FI",
            "SESSION eq 0",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if killed {
        // A stuck service can take a moment to drain its threads after being
        // terminated; give Windows time to unmap the image and free the handle.
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Overwrite the installed binary, clearing any process that holds it locked
/// before each attempt. Retries because a service the SCM is mid-restart can
/// respawn a fresh lock-holder in the gap between the kill and the copy, and a
/// just-stopped service can hold its exe open for a moment as it exits.
fn replace_installed_binary(from: &std::path::Path, to: &std::path::Path) -> anyhow::Result<()> {
    let mut last_err = None;
    for _ in 0..10 {
        kill_stale_installed_processes();
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    Err(anyhow::anyhow!(
        "Failed to copy binary to {}: {}",
        to.display(),
        last_err.expect("loop runs at least once")
    ))
}

/// Clear a service's automatic-restart failure actions so the Service Control
/// Manager won't respawn its (possibly crash-looping) process the instant we
/// stop or kill it — otherwise a fresh zombie grabs the binary lock before we
/// can copy. Best-effort; the real actions are restored when we reconfigure.
fn disarm_failure_actions(service: &Service) {
    let _ = service.set_failure_actions_on_non_crash_failures(false);
    let _ = service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::Never,
        reboot_msg: None,
        command: None,
        actions: Some(vec![]), // empty (with a non-null ptr) deletes the actions
    });
}

/// Stop a service if running and wait (up to ~10s) until it reports Stopped, so
/// its binary is unlocked and can be replaced.
fn stop_and_wait(service: &Service) {
    if service.stop().is_ok() {
        println!("Stopping running service...");
    }
    for _ in 0..40 {
        match service.query_status() {
            Ok(status) if status.current_state == ServiceState::Stopped => return,
            Ok(_) => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => return,
        }
    }
}

fn cmd_install() -> anyhow::Result<()> {
    // Copy config to ProgramData before elevating (CWD changes after elevation)
    let config_source = std::path::PathBuf::from("config.toml");
    let config_dest = config::Config::programdata_config_path();

    std::fs::create_dir_all(config::Config::programdata_dir())?;
    if config_source.exists() {
        std::fs::copy(&config_source, &config_dest)?;
        println!("Config copied to {}", config_dest.display());
    } else if !config_dest.exists() {
        std::fs::write(&config_dest, config::Config::DEFAULT_CONFIG)?;
        println!("Wrote built-in default config to {}", config_dest.display());
    } else {
        println!("Using existing config at {}", config_dest.display());
    }

    if !is_elevated() {
        return match run_elevated("install") {
            Ok(()) => {
                println!("Service '{SERVICE_NAME}' installed and started.");
                Ok(())
            }
            Err(e) => Err(with_elevated_log(e)),
        };
    }

    // Elevated half: do the work and record the outcome so the parent — which
    // can't read this hidden window's output — can surface it.
    let result = install_service();
    log_elevated_result("install", &result);
    result
}

/// Enrich an elevation failure with whatever the elevated half logged.
fn with_elevated_log(e: anyhow::Error) -> anyhow::Error {
    match std::fs::read_to_string(install_log_path()) {
        Ok(details) if !details.trim().is_empty() => anyhow::anyhow!(
            "{e}\n\n--- elevated log ({}) ---\n{}",
            install_log_path().display(),
            details.trim()
        ),
        _ => e,
    }
}

/// The elevated portion of installation: stop any existing service, swap the
/// binary, drop the sidecar bundle, then (re)create and start the service.
fn install_service() -> anyhow::Result<()> {
    // Install from a stable location, not the build output. Running the service
    // directly from target\release would lock watchdesk.exe and break `cargo build`.
    let install_exe = installed_exe_path();
    let current_exe = std::env::current_exe()?;

    let manager = ServiceManager::local_computer(
        None::<&OsStr>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    // If a previous install exists, disarm its restart actions and stop it so
    // its binary is unlocked and the SCM won't fight us by respawning it.
    let existing = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS
                | ServiceAccess::STOP
                | ServiceAccess::CHANGE_CONFIG
                | ServiceAccess::START,
        )
        .ok();
    if let Some(svc) = &existing {
        // Order matters: disarm before stopping/killing, or a crash-looping
        // service is instantly restarted by the SCM and re-locks the binary.
        disarm_failure_actions(svc);
        stop_and_wait(svc);
    }

    // Copy ourselves into the install location (skip if we already are it). A
    // previous service process may still hold the target exe locked (shutdown
    // hang, or an SCM restart of a crashing service); this clears it and retries.
    if current_exe != install_exe {
        replace_installed_binary(&current_exe, &install_exe)?;
        println!("Installed binary to {}", install_exe.display());
    }

    // Older versions extracted a LibreHardwareMonitor sidecar here; temperature
    // now comes from AMD's signed CLI, so clear out the leftovers.
    remove_legacy_sensor_dir();

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: install_exe.clone(),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };

    // Reuse the existing service (repoint it) or create a fresh one.
    let service = match existing {
        Some(svc) => {
            svc.change_config(&service_info)?;
            svc
        }
        None => manager.create_service(
            &service_info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
        )?,
    };
    service.set_description(
        "Publishes PC status (monitor state, CPU usage & temperature) to Home Assistant via MQTT",
    )?;

    let failure_actions = ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5),
            },
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(10),
            },
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(30),
            },
        ]),
    };
    service.update_failure_actions(failure_actions)?;
    service.set_failure_actions_on_non_crash_failures(true)?;

    // Start the service immediately
    service.start::<OsString>(&[])?;

    Ok(())
}

fn cmd_uninstall() -> anyhow::Result<()> {
    if !is_elevated() {
        return match run_elevated("uninstall") {
            Ok(()) => {
                println!("Service '{SERVICE_NAME}' uninstalled.");
                Ok(())
            }
            Err(e) => Err(with_elevated_log(e)),
        };
    }

    let result = uninstall_service();
    log_elevated_result("uninstall", &result);
    result
}

fn uninstall_service() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;

    match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => {
            stop_and_wait(&service);
            service.delete()?;
            println!("Service '{SERVICE_NAME}' removed.");
        }
        Err(_) => println!("Service '{SERVICE_NAME}' was not installed."),
    }

    // Best effort: remove the installed binary (ignored if it's the exe we're running).
    let install_exe = installed_exe_path();
    if install_exe.exists() {
        match std::fs::remove_file(&install_exe) {
            Ok(()) => println!("Removed {}", install_exe.display()),
            Err(e) => eprintln!(
                "Note: could not remove {} ({e}). Delete it manually if needed.",
                install_exe.display()
            ),
        }
    }

    remove_legacy_sensor_dir();
    Ok(())
}

fn cmd_status() -> anyhow::Result<()> {
    println!("WatchDesk status");
    println!("================");

    // Service state (query access doesn't require elevation)
    match ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT) {
        Ok(manager) => match manager.open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
        ) {
            Ok(service) => {
                match service.query_status() {
                    Ok(status) => {
                        println!("Service    : installed");
                        println!("State      : {:?}", status.current_state);
                        if let Some(pid) = status.process_id {
                            println!("PID        : {pid}");
                        }
                    }
                    Err(e) => println!("Service    : installed (status query failed: {e})"),
                }
                if let Ok(config) = service.query_config() {
                    println!("Start type : {:?}", config.start_type);
                    println!("Exe path   : {}", config.executable_path.display());
                }
            }
            Err(_) => println!("Service    : NOT installed"),
        },
        Err(e) => println!("Service    : unknown (cannot open service manager: {e})"),
    }

    // Active config
    println!();
    println!(
        "Config     : {}",
        config::Config::programdata_config_path().display()
    );
    match config::Config::load() {
        Ok(c) => {
            println!("  MQTT     : {}:{}", c.mqtt.host, c.mqtt.port);
            println!("  Device   : {}", c.device.name);
            println!(
                "  Startup  : turn_off_bluetooth={}",
                c.startup.turn_off_bluetooth
            );
            println!(
                "  Auth     : {}",
                if c.mqtt.username.is_some() {
                    "credentials set"
                } else {
                    "anonymous"
                }
            );
        }
        Err(e) => println!("  (not loaded: {e})"),
    }

    // Recent log lines
    println!();
    let log_path = config::Config::programdata_dir().join("watchdesk.log");
    if let Ok(contents) = std::fs::read_to_string(&log_path) {
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(10);
        println!("Recent log ({}):", log_path.display());
        for line in &lines[start..] {
            println!("  {line}");
        }
    } else {
        println!("Log        : none at {}", log_path.display());
    }

    Ok(())
}

fn cmd_run() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting WatchDesk in foreground mode");

    let config = config::Config::load()?;
    let monitor_rx = monitor::start_foreground_monitor()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (mqtt_manager, event_loop) = mqtt::MqttManager::new(&config)?;

        // Set up Ctrl+C handler
        let shutdown_tx = shutdown_tx;
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Ctrl+C received, shutting down...");
            let _ = shutdown_tx.send(true);
        });

        // Foreground mode receives no power broadcasts, so nothing ever signals
        // suspend here; the branch simply stays disabled.
        let (_suspend_tx, suspend_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        mqtt_manager
            .run(event_loop, monitor_rx, suspend_rx, shutdown_rx)
            .await
    })
}
