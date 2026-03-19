use log::{error, info};
use std::sync::OnceLock;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::UI::WindowsAndMessaging::REGISTER_NOTIFICATION_FLAGS;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_POWERBROADCAST, WNDCLASSW,
};
use windows::Win32::System::SystemServices::{GUID_CONSOLE_DISPLAY_STATE, GUID_MONITOR_POWER_ON};
use windows::core::w;

const PBT_POWERSETTINGCHANGE: u32 = 0x8013;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorState {
    On,
    Off,
}

impl MonitorState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "ON",
            Self::Off => "OFF",
        }
    }

    pub const fn from_power_value(value: u32) -> Self {
        match value {
            0 => Self::Off,
            _ => Self::On, // 1 = on, 2 = dimmed (treated as on)
        }
    }
}

// Global sender for the window procedure callback
static MONITOR_TX: OnceLock<mpsc::UnboundedSender<MonitorState>> = OnceLock::new();

/// Parse monitor state from a POWERBROADCAST_SETTING pointer.
///
/// # Safety
/// The caller must ensure `lparam` points to a valid POWERBROADCAST_SETTING structure.
pub unsafe fn parse_power_setting_change(lparam: isize) -> Option<MonitorState> {
    let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };

    let guid = setting.PowerSetting;
    if guid == GUID_CONSOLE_DISPLAY_STATE || guid == GUID_MONITOR_POWER_ON {
        let value = setting.Data[0] as u32;
        let state = MonitorState::from_power_value(value);
        info!("Monitor power event: value={value}, state={state:?}");
        Some(state)
    } else {
        None
    }
}

/// Start monitor detection in foreground mode using a hidden window and message loop.
/// Runs on a dedicated thread; sends state changes to the returned receiver.
pub fn start_foreground_monitor() -> anyhow::Result<mpsc::UnboundedReceiver<MonitorState>> {
    let (tx, rx) = mpsc::unbounded_channel();
    MONITOR_TX
        .set(tx)
        .map_err(|_| anyhow::anyhow!("Monitor sender already initialized"))?;

    std::thread::spawn(|| {
        if let Err(e) = run_message_loop() {
            error!("Message loop error: {e}");
        }
    });

    Ok(rx)
}

fn run_message_loop() -> anyhow::Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;

        let class_name = w!("WatchDeskMonitor");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        let atom = RegisterClassW(&raw const wc);
        if atom == 0 {
            return Err(anyhow::anyhow!("Failed to register window class"));
        }

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("WatchDesk Monitor Window"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )?;

        let handle = RegisterPowerSettingNotification(
            hwnd.into(),
            &GUID_CONSOLE_DISPLAY_STATE,
            REGISTER_NOTIFICATION_FLAGS(0), // DEVICE_NOTIFY_WINDOW_HANDLE
        )?;

        info!("Registered for GUID_CONSOLE_DISPLAY_STATE notifications (handle: {handle:?})");

        let mut msg = std::mem::zeroed();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&raw const msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_POWERBROADCAST && wparam.0 as u32 == PBT_POWERSETTINGCHANGE {
        if let Some(state) = unsafe { parse_power_setting_change(lparam.0) }
            && let Some(tx) = MONITOR_TX.get()
        {
            let _ = tx.send(state);
        }
        return LRESULT(1); // TRUE - handled
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
