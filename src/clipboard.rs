//! The Windows clipboard, as Unicode text.

use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Opens the clipboard, retrying briefly since another app may hold it.
fn open(owner: Option<HWND>) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

pub fn get_text(owner: Option<HWND>) -> Option<String> {
    if !open(owner) {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|handle| {
            let memory = HGLOBAL(handle.0);
            let ptr = GlobalLock(memory) as *const u16;
            if ptr.is_null() {
                return None;
            }
            // The text is NUL-terminated, but don't trust that past the allocation.
            let capacity = GlobalSize(memory) / 2;
            let units = std::slice::from_raw_parts(ptr, capacity);
            let len = units.iter().position(|&u| u == 0).unwrap_or(capacity);
            let text = String::from_utf16_lossy(&units[..len]);
            let _ = GlobalUnlock(memory);
            Some(text)
        })
    };
    let _ = unsafe { CloseClipboard() };
    text
}

pub fn set_text(owner: Option<HWND>, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    if !open(owner) {
        return false;
    }
    let result = unsafe {
        EmptyClipboard().and_then(|()| {
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let ptr = GlobalLock(memory) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(memory);
            // On success the clipboard owns the memory.
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0))).map(|_| ())
        })
    };
    let _ = unsafe { CloseClipboard() };
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real system clipboard, so it only runs on request:
    /// `cargo test clipboard -- --ignored`.
    #[test]
    #[ignore = "overwrites the system clipboard"]
    fn text_round_trips_through_the_clipboard() {
        let saved = get_text(None);
        assert!(set_text(None, "wisp ✓ 😀\nline two"));
        assert_eq!(get_text(None).as_deref(), Some("wisp ✓ 😀\nline two"));
        if let Some(saved) = saved {
            set_text(None, &saved);
        }
    }
}
