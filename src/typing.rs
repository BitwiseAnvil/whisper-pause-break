use anyhow::{Result, ensure};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::*,
    WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

pub fn normalize(text: &str) -> String {
    // Never synthesize Enter, Tab, or other application commands from model output.
    text.split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn unicode_events(text: &str) -> Vec<INPUT> {
    text.encode_utf16()
        .flat_map(|unit| {
            [0, KEYEVENTF_KEYUP].map(|up| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: KEYEVENTF_UNICODE | up,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            })
        })
        .collect()
}

pub fn insert(text: &str, allowed: &AtomicBool) -> Result<()> {
    ensure!(
        allowed.load(Ordering::SeqCst),
        "Insertion cancelled by session lock or shutdown"
    );
    ensure!(!text.is_empty(), "No text to insert");
    ensure!(
        text.encode_utf16().count() <= 32_000,
        "Transcription exceeds insertion size limit"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    // Held shortcut modifiers can change how an application treats injected text.
    while modifiers_held() {
        ensure!(
            allowed.load(Ordering::SeqCst),
            "Insertion cancelled by session lock or shutdown"
        );
        ensure!(
            Instant::now() < deadline,
            "Release Ctrl, Alt, Shift, and Windows before dictating"
        );
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        let foreground = GetForegroundWindow();
        ensure!(!foreground.is_null(), "No focused application");
        let mut process_id = 0;
        GetWindowThreadProcessId(foreground, &mut process_id);
        ensure!(
            process_id != std::process::id(),
            "Focus a text field in another application"
        );
        let events = unicode_events(text);
        ensure!(
            allowed.load(Ordering::SeqCst),
            "Insertion cancelled by session lock or shutdown"
        );
        // One call queues the complete text without interleaving other injected keystrokes.
        let sent = SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
        ensure!(
            sent as usize == events.len(),
            "Windows accepted {sent}/{} input events. The target may be elevated or reject synthetic input; text is not retried to avoid duplication",
            events.len()
        );
    }
    Ok(())
}

fn modifiers_held() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .iter()
        .any(|key| unsafe { GetAsyncKeyState(i32::from(*key)) < 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locked_or_stopped_session_cannot_insert() {
        assert!(insert("must not type", &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn unicode_pairs_include_surrogates_and_keyups() {
        let events = unicode_events("Aé🦀");
        assert_eq!(events.len(), 8);
        for pair in events.chunks_exact(2) {
            unsafe {
                assert_eq!(pair[0].Anonymous.ki.wScan, pair[1].Anonymous.ki.wScan);
                assert_eq!(pair[0].Anonymous.ki.dwFlags, KEYEVENTF_UNICODE);
                assert_eq!(
                    pair[1].Anonymous.ki.dwFlags,
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                );
            }
        }
    }
    #[test]
    fn text_cannot_submit_forms_or_tab_between_fields() {
        assert_eq!(
            normalize("  hello\r\n\tworld\0 \u{7} 🦀  "),
            "hello world 🦀"
        );
    }
}
