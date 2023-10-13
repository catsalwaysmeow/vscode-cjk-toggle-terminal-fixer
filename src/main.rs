#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::{env, mem, path::Path, process, sync::mpsc, thread};

use anyhow::{Context, Result};
use auto_launch::AutoLaunchBuilder;
#[allow(unused_imports)]
use tracing::{debug, error, info, trace, warn};
use trayicon::{Icon, MenuBuilder, TrayIconBuilder};
use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{RegisterHotKey, MOD_CONTROL, VK_OEM_3},
        WindowsAndMessaging::{
            DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextW, PostMessageA,
            PostThreadMessageW, TranslateMessage, MSG, WM_DPICHANGED,
            WM_DWMCOLORIZATIONCOLORCHANGED, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
        },
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

const PACKAGE_NAME: &'static str = env!("CARGO_PKG_NAME");
const PACKAGE_VERSION: &'static str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Exit,
    AutoLaunch,
    SystemDpiChanged,
    SystemColorChanged,
}

fn main() -> Result<()> {
    windows_dpi::enable_dpi();

    let app_path = env::current_exe();
    let file_appender = tracing_appender::rolling::never(
        app_path
            .as_deref()
            .ok()
            .and_then(|app_path| app_path.parent())
            .unwrap_or_else(|| Path::new("")),
        format!("{}-{}.log", PACKAGE_NAME, PACKAGE_VERSION),
    );

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(file_appender)
        .init();

    let result = logged_main(app_path.as_deref().warn());
    if let Err(ref err) = result {
        error!("{err:?}");
    }

    result
}

fn logged_main(app_path: Option<&Path>) -> Result<()> {
    const KEYID_CTRL_OEM_3: usize = 2333; // note: any value is acceptable as here we register only one hotkey.
    unsafe {
        RegisterHotKey(
            HWND(0),
            KEYID_CTRL_OEM_3 as i32,
            MOD_CONTROL,
            VK_OEM_3.0 as _,
        )?;
    }
    let auto_launch = app_path
        .and_then(|app_path| {
            app_path
                .to_str()
                .with_context(|| format!("non-utf8 path: {app_path:?}"))
                .warn()
        })
        .and_then(|app_path| {
            AutoLaunchBuilder::new()
                .set_app_name(PACKAGE_NAME)
                .set_app_path(app_path)
                .build()
                .warn()
        });
    let (tx, rx) = mpsc::channel::<Event>();
    let mut icon_param = get_icon_param();
    let mut tray: trayicon::TrayIcon<Event> = TrayIconBuilder::new()
        .sender(tx.clone())
        .icon(select_icon(icon_param))
        .tooltip("Fixing the issue where 「Ctrl+`」 doesn't work with some CJK keyboards/IMEs in VSCode. ")
        .menu(
            MenuBuilder::new()
                .when(|menu| match auto_launch.as_ref().and_then(|al|al.is_enabled().warn()) {
                    Some(enabled) => menu.checkable("Auto Launch", enabled, Event::AutoLaunch),
                    None => menu,
                })
                .separator()
                .item("Exit", Event::Exit),
        )
        .build()?;

    thread::scope(|s| -> () {
        let tid: u32 = unsafe { GetCurrentThreadId() };

        s.spawn(move || loop {
            let Ok(evt) = rx.recv() else { break };
            match evt {
                Event::Exit => {
                    drop(tray); // dead lock: we MUST drop 'tray' here as it relis on the message pump of main thread.
                    match unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) }.warn() {
                        Some(_) => break,
                        None => process::exit(-1),
                    }
                }
                Event::AutoLaunch => {
                    auto_launch.as_ref().and_then(|al| {
                        if al.is_enabled().warn()? {
                            al.disable().warn().and_then(|_| {
                                tray.set_menu_item_checkable(Event::AutoLaunch, false)
                                    .warn()
                            })
                        } else {
                            al.enable().warn().and_then(|_| {
                                tray.set_menu_item_checkable(Event::AutoLaunch, true).warn()
                            })
                        }
                    });
                }
                Event::SystemDpiChanged | Event::SystemColorChanged => {
                    let new_icon_param = get_icon_param();
                    if icon_param != new_icon_param {
                        icon_param = new_icon_param;
                        tray.set_icon(&select_icon(icon_param)).warn();
                    }
                }
            }
        });

        let mut msg: MSG = unsafe { mem::zeroed() };
        loop {
            let hr = unsafe { GetMessageW(&mut msg, HWND(0), 0, 0) };
            if matches!(hr, BOOL(0 | -1)) {
                // note: -1 is an error state but is unreachable here so we don't handle it.
                break;
            }

            match msg.message {
                WM_HOTKEY if matches!(msg.wParam, WPARAM(KEYID_CTRL_OEM_3)) => {
                    mock_key_press();
                }
                WM_DPICHANGED => {
                    tx.send(Event::SystemDpiChanged).ok();
                }
                WM_DWMCOLORIZATIONCOLORCHANGED => {
                    tx.send(Event::SystemColorChanged).ok();
                }
                _unhandled_message => unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
    });

    Ok(())
}

fn mock_key_press() {
    unsafe {
        let h_active_wnd = GetForegroundWindow();
        if matches!(h_active_wnd, HWND(0)) {
            return;
        }

        let window_title = {
            let mut buffer = [0u16; 512];
            let buffer_used_count = GetWindowTextW(h_active_wnd, &mut buffer) as usize;
            String::from_utf16_lossy(&buffer[..buffer_used_count])
        };

        if !matches!(
            window_title.rsplit(" - ").next().map(str::trim),
            Some("Visual Studio Code" | "VS Code")
        ) {
            return;
        }

        for action in [WM_KEYDOWN, WM_KEYUP] {
            PostMessageA(
                h_active_wnd,
                action,
                WPARAM(VK_OEM_3.0 as usize),
                LPARAM(1 | 0b10 << 16),
            )
            .warn();
        }
    }
}

#[derive(Debug, Default, PartialEq, PartialOrd, Clone, Copy)]
struct IconParam {
    light_mode: bool,
    scaling_factor: f32,
}

fn get_icon_param() -> IconParam {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let system_uses_light_theme: Option<u32> = hkcu
        .open_subkey(r#"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"#)
        .warn()
        .and_then(|personalize| personalize.get_value(r#"SystemUsesLightTheme"#).warn());

    IconParam {
        light_mode: matches!(system_uses_light_theme, Some(0)),
        scaling_factor: windows_dpi::desktop_dpi(),
    }
}

#[cfg_attr(test, deny(warnings))]
fn select_icon(
    IconParam {
        light_mode,
        scaling_factor,
    }: IconParam,
) -> Icon {
    let l16: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-light-16x16.ico"#);
    let l24: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-light-24x24.ico"#);
    let l32: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-light-32x32.ico"#);
    let l48: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-light-48x48.ico"#);
    let l256: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-light-256x256.ico"#);

    let d16: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-dark-16x16.ico"#);
    let d24: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-dark-24x24.ico"#);
    let d32: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-dark-32x32.ico"#);
    let d48: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-dark-48x48.ico"#);
    let d256: &[u8] = include_bytes!(r#"..\assets\terminal_box_icon-dark-256x256.ico"#);

    #[cfg(test)]
    {
        let _ = (light_mode, scaling_factor);
        [l16, l24, l32, l48, l256, d16, d24, d32, d48, d256]
            .into_iter()
            .map(|data| Icon::from_buffer(data, None, None).unwrap())
            .collect::<Vec<_>>()
            .pop()
            .unwrap()
    }

    #[cfg(not(test))]
    {
        let icons = if light_mode {
            [(16, l16), (24, l24), (32, l32), (48, l48), (256, l256)]
        } else {
            [(16, d16), (24, d24), (32, d32), (48, d48), (256, d256)]
        };
        let target_size = scaling_factor * 16f32;
        let (_size, data) = icons
            .into_iter()
            .min_by_key(|&(size, ..)| (size as f32 - target_size).abs() as i32)
            .unwrap(); // unwrap: safe as 'icons' is not empty.

        Icon::from_buffer(data, None, None).unwrap() // unwrap: tested.
    }
}

trait LogExt<T> {
    fn warn(self) -> Option<T>;
}

impl<T, E: std::fmt::Debug> LogExt<T> for std::result::Result<T, E> {
    fn warn(self) -> Option<T> {
        if let Err(ref err) = self {
            warn!("{err:?}");
        }
        self.ok()
    }
}

#[cfg(test)]
mod test {
    use crate::select_icon;

    #[test]
    fn check_icons() {
        select_icon(Default::default());
    }
}
