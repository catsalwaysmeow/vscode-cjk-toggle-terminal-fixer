#![windows_subsystem = "windows"]
use std::{
    env, mem, process,
    sync::mpsc::{self, TryRecvError},
    thread,
    time::Duration,
};

use auto_launch::AutoLaunchBuilder;
use trayicon::{MenuBuilder, TrayIconBuilder};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{RegisterHotKey, MOD_CONTROL, VK_OEM_3},
        WindowsAndMessaging::{
            DispatchMessageW, GetForegroundWindow, GetMessageW, PostMessageA, MSG, WM_HOTKEY,
            WM_KEYDOWN, WM_KEYUP,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Exit,
    AutoLaunch,
}

fn main() {
    let (stop, stopped) = mpsc::channel::<()>();
    let launch_config = AutoLaunchBuilder::new()
        .set_app_name(env!("CARGO_PKG_NAME"))
        .set_app_path(env::current_exe().unwrap().to_str().unwrap())
        .set_use_launch_agent(true)
        .build()
        .unwrap();
    let icon = include_bytes!("../assets/terminal_box_icon.ico");
    let (tx, rx) = mpsc::channel::<Event>();
    let mut tray = TrayIconBuilder::new()
        .sender(tx)
        .icon_from_buffer(icon)
        .tooltip("A utility to enable ctrl+` (for Visual Studio Code) in CJK environment")
        .menu(
            MenuBuilder::new()
                .checkable(
                    "Auto Launch",
                    launch_config.is_enabled().unwrap(),
                    Event::AutoLaunch,
                )
                .separator()
                .item("Exit", Event::Exit),
        )
        .build()
        .unwrap();

    thread::spawn(move || {
        let _stop_guard = stop;
        loop {
            let evt = rx.recv().unwrap();
            match evt {
                Event::Exit => process::exit(0),
                Event::AutoLaunch => {
                    if launch_config.is_enabled().unwrap() {
                        launch_config.disable().unwrap();
                        tray.set_menu_item_checkable(Event::AutoLaunch, false)
                            .unwrap();
                    } else {
                        launch_config.enable().unwrap();
                        tray.set_menu_item_checkable(Event::AutoLaunch, true)
                            .unwrap();
                    }
                }
            }
        }
    });

    unsafe {
        let keyid = 2333;
        let hr = RegisterHotKey(HWND(0), keyid, MOD_CONTROL, VK_OEM_3.0 as _);

        if !hr.as_bool() {
            panic!("RegisterHotKey failed")
        }

        let mut msg: MSG = mem::zeroed();
        while matches!(stopped.try_recv(), Err(TryRecvError::Empty)) {
            let res = GetMessageW(&mut msg, HWND(0), 0, 0);

            assert_ne!(res.0, -1);
            if !res.as_bool() {
                break;
            }

            if msg.message == WM_HOTKEY && msg.wParam.0 == keyid as usize {
                mock_key()
            } else {
                DispatchMessageW(&msg);
            }
        }

        thread::sleep(Duration::from_millis(200));
    }
}

fn mock_key() {
    unsafe {
        let h_active_wnd = GetForegroundWindow();
        if matches!(h_active_wnd, HWND(0)) {
            return;
        }

        for action in [WM_KEYDOWN, WM_KEYUP] {
            println!("{action}");
            PostMessageA(
                h_active_wnd,
                action,
                WPARAM(VK_OEM_3.0 as usize),
                LPARAM(1 | 0b10 << 16),
            );
        }
    }
}
