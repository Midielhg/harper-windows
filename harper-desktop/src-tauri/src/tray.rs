use std::time::Duration;

use tauri::image::Image;
use tauri::tray::TrayIconBuilder;
use tokio::time::sleep;
use tracing::error;

use tauri::menu::{Menu, MenuBuilder};
use tauri::{AppHandle, Manager, Runtime, State};
use tokio::runtime::Runtime as AsyncRuntime;

use crate::highlighter_service::HighlighterService;
use crate::windows::{open_issue_report, show_editor_window, show_settings_window};

/// Defines the layout of the tray menu.
fn tray_menu<R: Runtime, M: Manager<R>>(manager: &M) -> tauri::Result<Menu<R>> {
    MenuBuilder::new(manager)
        .text("toggle-service", "Toggle Service")
        .text("open-editor", "Open Editor")
        .text("settings", "Settings")
        .text("report-issue", "Report Issue")
        .text("quit", "Quit")
        .build()
}

pub fn set_up_tray_menu(app: &AppHandle) -> tauri::Result<()> {
    let highlighter_service: State<HighlighterService> = app.state();
    let async_runtime: State<AsyncRuntime> = app.state();

    let initial_is_running = highlighter_service.is_running();

    let tray_icon = TrayIconBuilder::new()
        .icon(menu_bar_icon(initial_is_running)?)
        .menu(&tray_menu(app)?)
        .on_menu_event(move |app, event| {
            let event_id = event.id().0.as_str();

            match event_id {
                "toggle-service" => {
                    let service: State<HighlighterService> = app.state();

                    let _ = service
                        .toggle()
                        .inspect_err(|err| error!("Could not toggle highlighter: {err}"));
                }
                "open-editor" => {
                    let _ = show_editor_window(app)
                        .inspect_err(|err| error!("Could not open the editor window: {err}"));
                }
                "settings" => {
                    let _ = show_settings_window(app)
                        .inspect_err(|err| error!("Could not open the settings window: {err}"));
                }
                "report-issue" => open_issue_report(app),
                "quit" => app.exit(0),
                _ => error!("Encountered unexpected event: `{event_id}`"),
            };
        })
        .build(app)?;

    let app = app.clone();
    async_runtime.spawn(async move {
        let mut shown_is_running = initial_is_running;

        loop {
            sleep(Duration::from_millis(250)).await;

            let is_running = {
                let highlighter_service: State<HighlighterService> = app.state();
                highlighter_service.is_running()
            };

            // Only touch the tray when the service state actually changes.
            // Re-decoding the icon and calling set_icon four times a second
            // regardless is pure waste, and on Windows it eventually starts
            // failing: after several days the shell began rejecting every
            // update with ERROR_TIMEOUT, filling the log and leaving the tray
            // icon unresponsive.
            if is_running == shown_is_running {
                continue;
            }

            let Ok(new_icon) = menu_bar_icon(is_running) else {
                error!("Unable to generate new menu bar icon.");
                continue;
            };

            match tray_icon.set_icon(Some(new_icon)) {
                Ok(()) => shown_is_running = is_running,
                // Leave the remembered state alone so the next tick retries.
                Err(err) => error!("Unable to set new icon: {err}"),
            }
        }
    });

    Ok(())
}

fn service_status_color(is_running: bool) -> [u8; 4] {
    match is_running {
        true => [34, 197, 94, 255],
        false => [239, 68, 68, 255],
    }
}

pub fn menu_bar_icon(is_running: bool) -> tauri::Result<Image<'static>> {
    let icon = Image::from_bytes(include_bytes!("../icons/menu-bar-icon.png"))?;
    let width = icon.width();
    let height = icon.height();
    let mut rgba = icon.rgba().to_vec();

    recolor_for_tray(&mut rgba);
    draw_status_line(&mut rgba, width, height, service_status_color(is_running));

    Ok(Image::new_owned(rgba, width, height))
}

/// Darkens the tray glyph when the taskbar behind it is light.
///
/// The artwork is drawn for the macOS menu bar, which means it is pure white throughout: every
/// opaque pixel of it is white, and only the alpha channel carries the shape. On a dark taskbar
/// that reads perfectly, and on a light one it is white on white — the icon is there, sized and
/// placed correctly, and simply cannot be seen.
///
/// Only the colour is replaced. The alpha channel is what draws the glasses, so leaving it alone
/// keeps every edge and curve exactly as it was.
fn recolor_for_tray(rgba: &mut [u8]) {
    if !taskbar_is_light() {
        return;
    }

    // Not pure black: the tray's own icons are a dark grey, and matching them keeps Harper from
    // looking heavier than everything beside it.
    const DARK: [u8; 3] = [32, 32, 32];

    for pixel in rgba.chunks_exact_mut(4) {
        pixel[..3].copy_from_slice(&DARK);
    }
}

/// Whether Windows is drawing the taskbar in its light theme.
///
/// Windows keeps the taskbar's brightness separate from the one apps use, so this reads the
/// taskbar's own setting rather than the app one. A missing or unreadable value means the default,
/// which is the dark taskbar the white artwork already suits.
#[cfg(windows)]
fn taskbar_is_light() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;

    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };

    status == ERROR_SUCCESS && value == 1
}

#[cfg(not(windows))]
fn taskbar_is_light() -> bool {
    false
}

fn draw_status_line(rgba: &mut [u8], width: u32, height: u32, color: [u8; 4]) {
    let line_height = (height.min(width) / 14).max(3);
    let start_y = height.saturating_sub(line_height);

    for y in start_y..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            rgba[index..index + 4].copy_from_slice(&color);
        }
    }
}
