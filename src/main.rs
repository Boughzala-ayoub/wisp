//! Wisp: a fast Windows terminal built on libghostty-vt, Ghostty's terminal core.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bindings;
mod clipboard;
mod config;
mod font;
mod input;
mod mouse;
mod pty;
mod render;
mod sprite;
mod term;
mod window;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW};
use windows::core::{HSTRING, w};

fn main() {
    let config = config::Config::load();
    window::report(&config);
    let result = (|| {
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
        window::Window::open(config)
    })();
    if let Err(e) = result {
        unsafe { MessageBoxW(None, &HSTRING::from(format!("Wisp failed to start:\n\n{e}")), w!("Wisp"), MB_ICONERROR) };
        std::process::exit(1);
    }
    window::run_message_loop();
}

/// Milliseconds since the OS created this process, loader time included.
pub fn ms_since_process_start() -> f64 {
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    let now = unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
        GetSystemTimePreciseAsFileTime()
    };
    let ticks = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
    ticks(now).saturating_sub(ticks(created)) as f64 / 10_000.0 // 100ns ticks
}
