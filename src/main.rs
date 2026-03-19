mod config;
mod monitor;
mod mqtt;
mod service;

use clap::{Parser, Subcommand};
use log::info;
use std::ffi::{OsStr, OsString};
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::core::PCWSTR;
use windows::core::w;
use windows_service::service::{
    ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "WatchDesk";
const DISPLAY_NAME: &str = "WatchDesk PC Presence Monitor";

#[derive(Parser)]
#[command(
    name = "watchdesk",
    about = "PC & Monitor Presence Service for Home Assistant"
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Install) => cmd_install(),
        Some(Command::Uninstall) => cmd_uninstall(),
        Some(Command::Run) => cmd_run(),
        None => {
            // No subcommand — assume launched by SCM
            service::dispatch()
        }
    }
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

fn cmd_install() -> anyhow::Result<()> {
    if !is_elevated() {
        return run_elevated("install");
    }

    let manager = ServiceManager::local_computer(
        None::<&OsStr>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let exe_path = std::env::current_exe()?;

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };

    let service = manager.create_service(
        &service_info,
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
    )?;
    service
        .set_description("Monitors PC presence and display state for Home Assistant via MQTT")?;

    println!("Service '{SERVICE_NAME}' installed successfully.");

    // Start the service immediately
    match service.start::<OsString>(&[]) {
        Ok(()) => println!("Service '{SERVICE_NAME}' started."),
        Err(e) => eprintln!("Warning: failed to start service: {e}"),
    }

    Ok(())
}

fn cmd_uninstall() -> anyhow::Result<()> {
    if !is_elevated() {
        return run_elevated("uninstall");
    }

    let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;

    let service =
        manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE)?;

    // Try to stop the service first (ignore errors if already stopped)
    let _ = service.stop();
    // Brief pause for stop to take effect
    std::thread::sleep(Duration::from_secs(1));

    service.delete()?;
    println!("Service '{SERVICE_NAME}' uninstalled successfully.");
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

        mqtt_manager.run(event_loop, monitor_rx, shutdown_rx).await
    })
}
