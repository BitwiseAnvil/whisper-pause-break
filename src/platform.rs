//! Keyboard callbacks filter shortcuts and enqueue events. No audio, model work, or file IO.
use crate::{
    app::{Event, Status},
    config::Hotkey,
    shortcuts::{Decision, Modifiers, VoiceTypingBlocker},
    state::Input,
};
use anyhow::{Result, ensure};
use crossbeam_channel::Sender;
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, RemoteDesktop::*, Threading::*},
    UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const CLASS: &str = "WhisperPauseBreak.TrayWindow";
const TRAY_MESSAGE: u32 = WM_APP + 1;
const EXIT: usize = 100;
const LOG: usize = 101;
const CONFIG: usize = 102;

pub fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

struct Context {
    tx: Sender<Event>,
    status: Status,
    dir: PathBuf,
    hotkey: Hotkey,
    held: bool,
    escape: bool,
    voice_typing: VoiceTypingBlocker,
    block_win_h: bool,
    diagnostic: bool,
    started: Instant,
    taskbar_created: u32,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) };
    }
    let key = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let mut mask_start = false;
    let swallowed = CONTEXT.with(|cell| {
        let Ok(mut context) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(c) = context.as_mut() else {
            return false;
        };
        let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
        let up = wparam as u32 == WM_KEYUP || wparam as u32 == WM_SYSKEYUP;
        if !down && !up {
            return false;
        }
        if c.block_win_h && !c.diagnostic && key.vkCode == u32::from(VK_H) {
            // H's own async state is not updated yet, but previously delivered modifiers are.
            let modifiers = Modifiers {
                windows: key_down(VK_LWIN) || key_down(VK_RWIN),
                control: key_down(VK_CONTROL),
                alt: key_down(VK_MENU),
                shift: key_down(VK_SHIFT),
            };
            match c.voice_typing.h_event(down, modifiers) {
                Decision::BlockAndMaskStart => {
                    mask_start = true;
                    return true;
                }
                Decision::Block => return true,
                Decision::Pass => {}
            }
        }
        // Remapped Win+H is blocked too; injected keys must never start/cancel dictation.
        if key.flags & LLKHF_INJECTED != 0 {
            return false;
        }
        let hotkey = key.vkCode == c.hotkey.vk()
            || (c.hotkey == Hotkey::Pause && key.vkCode == VK_CANCEL as u32);
        if hotkey {
            if c.diagnostic {
                // Printing is done by the receiver thread, never in this callback.
                crate::app::send(
                    &c.tx,
                    Event::Key(if down { Input::Down } else { Input::Up }),
                );
                return false;
            }
            if down && !c.held {
                c.held = true;
                crate::app::send(&c.tx, Event::Key(Input::Down));
            }
            if up && c.held {
                c.held = false;
                crate::app::send(&c.tx, Event::Key(Input::Up));
            }
            return true;
        }
        if key.vkCode == VK_ESCAPE as u32 {
            if down && (c.held || c.diagnostic) {
                if !c.escape {
                    crate::app::send(&c.tx, Event::Key(Input::Escape));
                }
                c.escape = true;
                return !c.diagnostic;
            }
            if up && c.escape {
                c.escape = false;
                return !c.diagnostic;
            }
        }
        false
    });
    if mask_start {
        // Drop the Context borrow before SendInput can reenter the keyboard hook.
        mask_start_menu();
    }
    if swallowed {
        1
    } else {
        unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
    }
}

fn key_down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

fn mask_start_menu() {
    // A swallowed H otherwise looks like a Windows-key tap to the shell. PowerToys uses
    // the same 0xFF dummy key to cancel menu activation without changing modifiers.
    let events = [0, KEYEVENTF_KEYUP].map(|flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0xff,
                dwFlags: flags,
                ..unsafe { std::mem::zeroed() }
            },
        },
    });
    unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

fn tray(hwnd: HWND, operation: u32, status: &str) {
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of_val(&data) as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        data.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
        let tip = wide(format!("Whisper Pause/Break — {status}"));
        for (dest, src) in data.szTip.iter_mut().take(127).zip(tip.iter()) {
            *dest = *src;
        }
        Shell_NotifyIconW(operation, &data);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER => {
            CONTEXT.with(|cell| {
                if let Some(c) = cell.borrow().as_ref() {
                    if c.diagnostic && c.started.elapsed().as_secs() >= 30 {
                        unsafe {
                            PostMessageW(hwnd, WM_CLOSE, 0, 0);
                        }
                    } else {
                        tray(hwnd, NIM_MODIFY, &c.status.get());
                    }
                }
            });
            0
        }
        TRAY_MESSAGE if lparam as u32 == WM_RBUTTONUP || lparam as u32 == WM_LBUTTONUP => {
            // Drop the RefCell borrow before TrackPopupMenu runs a nested Windows message loop.
            let status = CONTEXT.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|c| c.status.get())
                    .unwrap_or_default()
            });
            unsafe {
                let menu = CreatePopupMenu();
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(status).as_ptr());
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
                AppendMenuW(
                    menu,
                    MF_STRING,
                    CONFIG,
                    wide("Open configuration (restart to apply)").as_ptr(),
                );
                AppendMenuW(menu, MF_STRING, LOG, wide("Open log").as_ptr());
                AppendMenuW(menu, MF_STRING, EXIT, wide("Quit").as_ptr());
                let mut point = std::mem::zeroed();
                GetCursorPos(&mut point);
                SetForegroundWindow(hwnd);
                let chosen = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    0,
                    hwnd,
                    null(),
                );
                DestroyMenu(menu);
                PostMessageW(hwnd, WM_NULL, 0, 0);
                if chosen as usize == EXIT {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                if chosen as usize == LOG || chosen as usize == CONFIG {
                    CONTEXT.with(|cell| {
                        if let Some(c) = cell.borrow().as_ref() {
                            let path = c.dir.join(if chosen as usize == LOG {
                                "app.log"
                            } else {
                                "config.toml"
                            });
                            // Open as text rather than relying on a .toml file association.
                            ShellExecuteW(
                                hwnd,
                                null(),
                                wide("notepad.exe").as_ptr(),
                                wide(format!("\"{}\"", path.display())).as_ptr(),
                                null(),
                                SW_SHOWNORMAL,
                            );
                        }
                    });
                }
            }
            0
        }
        WM_WTSSESSION_CHANGE => {
            CONTEXT.with(|cell| {
                if let Some(c) = cell.borrow_mut().as_mut() {
                    if wparam == WTS_SESSION_LOCK as usize
                        || wparam == WTS_CONSOLE_DISCONNECT as usize
                        || wparam == WTS_REMOTE_DISCONNECT as usize
                    {
                        c.held = false;
                        c.escape = false;
                        c.voice_typing = VoiceTypingBlocker::default();
                        c.status.allow_insertion(false);
                        crate::app::send(&c.tx, Event::Suspend);
                    } else if wparam == WTS_SESSION_UNLOCK as usize
                        || wparam == WTS_CONSOLE_CONNECT as usize
                        || wparam == WTS_REMOTE_CONNECT as usize
                    {
                        c.voice_typing = VoiceTypingBlocker::new(key_down(VK_H));
                        c.status.allow_insertion(true);
                        crate::app::send(&c.tx, Event::Resume);
                    }
                }
            });
            0
        }
        WM_POWERBROADCAST => {
            CONTEXT.with(|cell| {
                if let Some(c) = cell.borrow_mut().as_mut() {
                    if wparam == PBT_APMSUSPEND as usize {
                        c.held = false;
                        c.escape = false;
                        c.voice_typing = VoiceTypingBlocker::default();
                        c.status.allow_insertion(false);
                        crate::app::send(&c.tx, Event::Suspend);
                    }
                    if wparam == PBT_APMRESUMEAUTOMATIC as usize {
                        c.voice_typing = VoiceTypingBlocker::new(key_down(VK_H));
                        c.status.allow_insertion(true);
                        crate::app::send(&c.tx, Event::Resume);
                    }
                }
            });
            1
        }
        WM_CLOSE => {
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            tray(hwnd, NIM_DELETE, "");
            CONTEXT.with(|cell| {
                if let Some(c) = cell.borrow().as_ref() {
                    c.status.allow_insertion(false);
                    crate::app::send(&c.tx, Event::Quit);
                }
            });
            unsafe {
                WTSUnRegisterSessionNotification(hwnd);
                PostQuitMessage(0);
            }
            0
        }
        _ => {
            CONTEXT.with(|cell| {
                if let Some(c) = cell.borrow().as_ref()
                    && message == c.taskbar_created
                {
                    tray(hwnd, NIM_ADD, &c.status.get());
                }
            });
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

pub struct Instance(HANDLE);
impl Instance {
    pub fn acquire() -> Result<Self> {
        unsafe {
            let handle = CreateMutexW(
                null(),
                0,
                wide("Local\\WhisperPauseBreak.Instance").as_ptr(),
            );
            ensure!(!handle.is_null(), "Could not create instance mutex");
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                anyhow::bail!("Whisper Pause/Break is already running (check the tray)");
            }
            Ok(Self(handle))
        }
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub fn stop() -> Result<()> {
    unsafe {
        let hwnd = FindWindowW(wide(CLASS).as_ptr(), null());
        if !hwnd.is_null() {
            ensure!(
                PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0,
                "Could not stop application"
            );
        }
    }
    Ok(())
}

pub fn run(
    tx: Sender<Event>,
    status: Status,
    dir: PathBuf,
    hotkey: Hotkey,
    block_win_h: bool,
    diagnostic: bool,
) -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        ensure!(
            RegisterClassW(&class) != 0,
            "Could not register tray window"
        );
        CONTEXT.with(|cell| {
            *cell.borrow_mut() = Some(Context {
                tx,
                status: status.clone(),
                dir,
                hotkey,
                held: false,
                escape: false,
                voice_typing: VoiceTypingBlocker::new(key_down(VK_H)),
                block_win_h,
                diagnostic,
                started: Instant::now(),
                taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            });
        });
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("Whisper Pause/Break").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        ensure!(!hwnd.is_null(), "Could not create tray window");
        let hook_handle = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), instance, 0);
        if hook_handle.is_null() {
            DestroyWindow(hwnd);
            anyhow::bail!(
                "Global keyboard hook failed: {}",
                std::io::Error::last_os_error()
            );
        }
        tray(hwnd, NIM_ADD, &status.get());
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        SetTimer(hwnd, 1, 250, None);
        let mut message: MSG = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        UnhookWindowsHookEx(hook_handle);
        CONTEXT.with(|cell| {
            *cell.borrow_mut() = None;
        });
        Ok(())
    }
}

pub fn error_dialog(text: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(text).as_ptr(),
            wide("Whisper Pause/Break").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
