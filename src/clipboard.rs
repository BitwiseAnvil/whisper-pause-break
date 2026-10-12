//! Paste output puts the transcript on the clipboard, where it stays like an ordinary copy.
use anyhow::{Result, bail, ensure};
use std::{ptr::null_mut, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::GlobalFree,
    System::{DataExchange::*, Memory::*},
};

const CF_UNICODETEXT: u32 = 13;

struct Open;
impl Open {
    fn new(attempts: u32) -> Result<Self> {
        for attempt in 0..attempts {
            // Without an owner window nothing can ask this thread to render data later.
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Ok(Self);
            }
            if attempt + 1 < attempts {
                thread::sleep(Duration::from_millis(20));
            }
        }
        bail!("the clipboard is in use by another application")
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

fn unicode_text(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Replaces the clipboard contents with `text`, retrying briefly while another app holds it.
pub fn set_text(text: &str) -> Result<()> {
    let bytes = unicode_text(text);
    let _open = Open::new(10)?;
    unsafe {
        ensure!(EmptyClipboard() != 0, "the clipboard could not be emptied");
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        let target = GlobalLock(memory);
        if target.is_null() {
            GlobalFree(memory);
            bail!("could not allocate clipboard memory");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target as *mut u8, bytes.len());
        GlobalUnlock(memory);
        // On success the clipboard owns the memory.
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            GlobalFree(memory);
            bail!("the transcript could not be placed on the clipboard");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_text_is_nul_terminated_utf16() {
        let units: Vec<u16> = unicode_text("é🦀 ")
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(units, [0xe9, 0xd83e, 0xdd80, 0x20, 0]);
    }
    /// Overwrites the real clipboard: `cargo test -- --ignored clipboard_keeps_the_transcript`.
    #[test]
    #[ignore]
    fn clipboard_keeps_the_transcript() {
        set_text("dictated take ").unwrap();
        let _open = Open::new(10).unwrap();
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        let text = unsafe { GlobalLock(handle) } as *const u16;
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe { GlobalUnlock(handle) };
        assert_eq!(value, "dictated take ");
    }
}
