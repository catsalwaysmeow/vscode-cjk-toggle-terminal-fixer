#![windows_subsystem = "windows"]
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
            DispatchMessageW, GetForegroundWindow, GetMessageW, PostMessageA, PostThreadMessageW,
            TranslateMessage, MSG, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
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
}

fn main() -> Result<()> {
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
    let mut tray: trayicon::TrayIcon<Event> = TrayIconBuilder::new()
        .sender(tx.clone())
        .icon(get_icon())
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

fn get_icon() -> Icon {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let apps_use_light_theme: Option<u32> = hkcu
        .open_subkey(r#"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"#)
        .warn()
        .and_then(|personalize| personalize.get_value(r#"AppsUseLightTheme"#).warn());

    match apps_use_light_theme {
        Some(1) => icon_light(),
        _ => icon_dark(),
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

fn icon_light() -> Icon {
    Icon::from_buffer(
        include_bytes!(r#"..\assets\terminal_box_icon-light.ico"#),
        None,
        None,
    )
    .unwrap() // unwrap: tested
}

fn icon_dark() -> Icon {
    Icon::from_buffer(
        include_bytes!(r#"..\assets\terminal_box_icon-dark.ico"#),
        None,
        None,
    )
    .unwrap() // unwrap: tested
}

#[cfg(test)]
mod test {
    use crate::{icon_dark, icon_light};

    #[test]
    fn check_icons() {
        icon_light();
        icon_dark();
    }
}
