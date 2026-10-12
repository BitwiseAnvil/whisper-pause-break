use crate::{clipboard, config::OutputMode};
use anyhow::{Result, ensure};
use std::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::*,
    WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

/// How the transcript reached the focused application.
pub enum Method {
    Pasted,
    Typed,
    /// Paste mode could not use the clipboard, so the text was typed.
    TypedInstead(anyhow::Error),
}
impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Pasted => write!(f, "pasted"),
            Self::Typed => write!(f, "typed"),
            Self::TypedInstead(error) => write!(f, "typed because {error:#}"),
        }
    }
}

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

fn key(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Ctrl is pressed first and released last, so V never arrives without it.
pub fn paste_events() -> [INPUT; 4] {
    [
        (VK_CONTROL, 0),
        (VK_V, 0),
        (VK_V, KEYEVENTF_KEYUP),
        (VK_CONTROL, KEYEVENTF_KEYUP),
    ]
    .map(|(vk, flags)| key(vk, flags))
}

fn send(events: &[INPUT]) -> u32 {
    // One call queues the complete batch without interleaving other injected keystrokes.
    unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    }
}

pub fn insert(text: &str, mode: OutputMode, allowed: &AtomicBool) -> Result<Method> {
    ensure!(
        allowed.load(Ordering::SeqCst),
        "Insertion cancelled by session lock or shutdown"
    );
    ensure!(!text.is_empty(), "No text to insert");
    ensure!(
        text.encode_utf16().count() <= 32_000,
        "Transcription exceeds insertion size limit"
    );
    wait_for_modifiers(allowed)?;
    let foreground = unsafe { GetForegroundWindow() };
    ensure!(!foreground.is_null(), "No focused application");
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(foreground, &mut process_id) };
    ensure!(
        process_id != std::process::id(),
        "Focus a text field in another application"
    );
    let mut fallback = None;
    if mode == OutputMode::Paste {
        // The transcript stays on the clipboard afterwards, like an ordinary copy.
        match clipboard::set_text(text) {
            Ok(()) => {
                // Waiting for the clipboard takes time; a modifier pressed meanwhile would turn
                // Ctrl+V into another shortcut.
                wait_for_modifiers(allowed)?;
                let events = paste_events();
                let sent = send(&events);
                if sent > 0 && (sent as usize) < events.len() {
                    // Never leave a synthetic Ctrl held down.
                    send(&[key(VK_CONTROL, KEYEVENTF_KEYUP)]);
                }
                ensure!(
                    sent as usize == events.len(),
                    "Windows accepted {sent}/{} Ctrl+V events. The target may be elevated or reject synthetic input; paste is not retried to avoid duplication",
                    events.len()
                );
                return Ok(Method::Pasted);
            }
            Err(error) => fallback = Some(error),
        }
    }
    let events = unicode_events(text);
    ensure!(
        allowed.load(Ordering::SeqCst),
        "Insertion cancelled by session lock or shutdown"
    );
    let sent = send(&events);
    ensure!(
        sent as usize == events.len(),
        "Windows accepted {sent}/{} input events. The target may be elevated or reject synthetic input; text is not retried to avoid duplication",
        events.len()
    );
    Ok(fallback.map_or(Method::Typed, Method::TypedInstead))
}

fn wait_for_modifiers(allowed: &AtomicBool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    // Held shortcut modifiers can change how an application treats injected text.
    loop {
        ensure!(
            allowed.load(Ordering::SeqCst),
            "Insertion cancelled by session lock or shutdown"
        );
        if !modifiers_held() {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Release Ctrl, Alt, Shift, and Windows before dictating"
        );
        thread::sleep(Duration::from_millis(10));
    }
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
        for mode in [OutputMode::Paste, OutputMode::Type] {
            assert!(insert("must not type", mode, &AtomicBool::new(false)).is_err());
        }
    }
    #[test]
    fn paste_wraps_v_in_ctrl() {
        let events = paste_events();
        let keys: Vec<_> = events
            .iter()
            .map(|event| unsafe { (event.Anonymous.ki.wVk, event.Anonymous.ki.dwFlags) })
            .collect();
        assert_eq!(
            keys,
            [
                (VK_CONTROL, 0),
                (VK_V, 0),
                (VK_V, KEYEVENTF_KEYUP),
                (VK_CONTROL, KEYEVENTF_KEYUP)
            ]
        );
        assert!(events.iter().all(|event| event.r#type == INPUT_KEYBOARD));
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
