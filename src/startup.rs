use crate::config::Config;
use anyhow::Context;
use log::{error, info, warn};
use std::time::Duration;
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemTimeOfDayInformation,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

const STARTUP_ACTION_MAX_UPTIME: Duration = Duration::from_secs(15 * 60);

pub fn run_once_after_boot(config: &Config) {
    if !config.startup.turn_off_bluetooth {
        info!("Bluetooth startup action disabled by config");
        return;
    }

    let uptime = uptime();
    if uptime > STARTUP_ACTION_MAX_UPTIME {
        info!(
            "Bluetooth startup action skipped because Windows uptime is {}s, above the {}s startup window",
            uptime.as_secs(),
            STARTUP_ACTION_MAX_UPTIME.as_secs()
        );
        return;
    }

    info!(
        "Bluetooth startup action enabled; Windows uptime is {}s",
        uptime.as_secs()
    );

    match claim_once_per_boot("turn-off-bluetooth") {
        Ok(true) => match turn_bluetooth_off() {
            Ok(()) => info!("Bluetooth startup action completed"),
            Err(e) => error!("Bluetooth startup action failed: {e:#}"),
        },
        Ok(false) => info!("Bluetooth startup action skipped because it already ran this boot"),
        Err(e) => {
            warn!(
                "Bluetooth startup action skipped because the boot marker could not be recorded: {e:#}"
            );
        }
    }
}

fn claim_once_per_boot(action: &str) -> anyhow::Result<bool> {
    let path = Config::programdata_dir().join(format!("{action}.boot"));
    let current_boot = current_boot_marker()?;

    info!(
        "Bluetooth startup action boot marker path: {}; current boot marker: {}",
        path.display(),
        current_boot
    );

    if path.exists() {
        let previous_boot = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        info!(
            "Bluetooth startup action previous boot marker: {}",
            previous_boot.trim()
        );
        if previous_boot.trim() == current_boot {
            return Ok(false);
        }
    }

    std::fs::create_dir_all(Config::programdata_dir())?;
    std::fs::write(&path, &current_boot)
        .with_context(|| format!("failed to write {}", path.display()))?;
    info!("Bluetooth startup action claimed for this boot");
    Ok(true)
}

fn current_boot_marker() -> anyhow::Result<String> {
    let mut info = SystemTimeOfDayInformationData::default();
    let status = unsafe {
        NtQuerySystemInformation(
            SystemTimeOfDayInformation,
            (&raw mut info).cast(),
            std::mem::size_of::<SystemTimeOfDayInformationData>() as u32,
            std::ptr::null_mut(),
        )
    };

    if status.0 < 0 {
        return Err(anyhow::anyhow!(
            "NtQuerySystemInformation(SystemTimeOfDayInformation) failed: {status:?}"
        ));
    }

    Ok(info.boot_time_100ns.to_string())
}

fn uptime() -> Duration {
    Duration::from_millis(unsafe { GetTickCount64() })
}

#[repr(C)]
#[derive(Default)]
struct SystemTimeOfDayInformationData {
    boot_time_100ns: i64,
    current_time_100ns: i64,
    time_zone_bias_100ns: i64,
    time_zone_id: u32,
    reserved: u32,
    boot_time_bias_100ns: u64,
    sleep_time_bias_100ns: u64,
}

fn turn_bluetooth_off() -> anyhow::Result<()> {
    let winrt = WinRtApartment::init()?;

    let access = Radio::RequestAccessAsync()?.join()?;
    info!("Bluetooth radio access status: {access:?}");
    if access != RadioAccessStatus::Allowed {
        return Err(anyhow::anyhow!(
            "radio control access was not allowed: {access:?}"
        ));
    }

    let radios = Radio::GetRadiosAsync()?.join()?;
    let mut found = false;
    let mut radio_count = 0u32;

    for radio in &radios {
        radio_count += 1;
        let name = radio.Name()?.to_string_lossy();
        let kind = radio.Kind()?;
        let state = radio.State()?;

        info!("Radio found: name='{name}', kind={kind:?}, state={state:?}");

        if kind != RadioKind::Bluetooth {
            continue;
        }

        found = true;

        if state == RadioState::Off {
            info!("Bluetooth radio already off: {name}");
        } else {
            let result = radio.SetStateAsync(RadioState::Off)?.join()?;
            info!("Bluetooth radio '{name}' set to off: {result:?}");
        }
    }

    if !found {
        info!("No Bluetooth radio found among {radio_count} radio(s)");
    }

    drop(winrt);
    Ok(())
}

struct WinRtApartment {
    uninitialize: bool,
}

impl WinRtApartment {
    fn init() -> anyhow::Result<Self> {
        unsafe {
            match RoInitialize(RO_INIT_MULTITHREADED) {
                Ok(()) => Ok(Self { uninitialize: true }),
                Err(e) if e.code().0 == RPC_E_CHANGED_MODE => {
                    info!("WinRT apartment already initialized with a different mode");
                    Ok(Self {
                        uninitialize: false,
                    })
                }
                Err(e) => Err(e.into()),
            }
        }
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe {
                RoUninitialize();
            }
        }
    }
}

const RPC_E_CHANGED_MODE: i32 = unchecked_hresult(0x80010106);

const fn unchecked_hresult(value: u32) -> i32 {
    value as i32
}
