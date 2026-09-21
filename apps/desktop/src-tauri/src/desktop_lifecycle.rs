use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WindowEvent,
};

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "codex-x-tray";
const SHOW_WINDOW_MENU_ID: &str = "show-main-window";
const QUIT_APP_MENU_ID: &str = "quit-codex-x";
const SHOW_TRAY_MENU_ON_LEFT_CLICK: bool = cfg!(target_os = "macos");

#[cfg(target_os = "macos")]
fn set_macos_tray_mode(app: &tauri::AppHandle, dock_visible: bool) -> tauri::Result<()> {
    let policy = if dock_visible {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    };

    // Attempt both operations: either one alone can leave a stale Dock entry on some macOS versions.
    let dock_result = app.set_dock_visibility(dock_visible);
    let policy_result = app.set_activation_policy(policy);
    dock_result?;
    policy_result
}

fn retain_first_error(first_error: &mut Option<tauri::Error>, result: tauri::Result<()>) {
    if let Err(error) = result {
        if first_error.is_none() {
            *first_error = Some(error);
        }
    }
}

fn show_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };

    let mut first_error = None;

    #[cfg(target_os = "windows")]
    retain_first_error(&mut first_error, window.set_skip_taskbar(false));
    #[cfg(target_os = "macos")]
    retain_first_error(&mut first_error, app.show());

    retain_first_error(&mut first_error, window.unminimize());
    retain_first_error(&mut first_error, window.show());
    retain_first_error(&mut first_error, window.set_focus());

    #[cfg(target_os = "macos")]
    retain_first_error(&mut first_error, set_macos_tray_mode(app, true));

    first_error.map_or(Ok(()), Err)
}

pub(crate) fn restore_main_window(app: &tauri::AppHandle) {
    if let Err(error) = show_main_window(app) {
        eprintln!("failed to restore the AstraX window: {error}");
    }
}

pub(crate) fn setup_system_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_window =
        MenuItem::with_id(app, SHOW_WINDOW_MENU_ID, "Show AstraX", true, None::<&str>)?;
    let quit_app = MenuItem::with_id(app, QUIT_APP_MENU_ID, "Quit AstraX", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_window, &quit_app])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("AstraX")
        .show_menu_on_left_click(SHOW_TRAY_MENU_ON_LEFT_CLICK)
        .on_menu_event(|app, event| match event.id().as_ref() {
            SHOW_WINDOW_MENU_ID => restore_main_window(app),
            QUIT_APP_MENU_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if !SHOW_TRAY_MENU_ON_LEFT_CLICK
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                )
            {
                restore_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }

    tray.build(app)?;
    Ok(())
}

pub(crate) fn handle_window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != MAIN_WINDOW_LABEL {
        return;
    }

    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        if let Err(error) = window.hide() {
            eprintln!("failed to hide the AstraX window: {error}");
            return;
        }

        #[cfg(target_os = "windows")]
        if let Err(error) = window.set_skip_taskbar(true) {
            eprintln!("failed to remove AstraX from the taskbar: {error}");
        }

        #[cfg(target_os = "macos")]
        if let Err(error) = set_macos_tray_mode(window.app_handle(), false) {
            eprintln!("failed to move AstraX to the menu bar: {error}");
        }
    }
}

pub(crate) fn report_failover_lifecycle_error(app: &tauri::AppHandle, title: &str, reason: &str) {
    let title = title.to_string();
    let reason = reason.to_string();
    eprintln!("{title}: {reason}");
    let _ = app.emit("provider-failover-error", &reason);
    tauri::async_runtime::spawn(async move {
        rfd::AsyncMessageDialog::new()
            .set_title(title)
            .set_description(reason)
            .set_level(rfd::MessageLevel::Error)
            .set_buttons(rfd::MessageButtons::Ok)
            .show()
            .await;
    });
}

pub(crate) fn handle_run_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    if let tauri::RunEvent::ExitRequested { api, .. } = &event {
        if let Err(error) = crate::failover::shutdown_all() {
            // Keep the listener alive when restoring the direct route failed.
            // Exiting here would strand Codex on a local address with no server.
            api.prevent_exit();
            restore_main_window(app);
            report_failover_lifecycle_error(
                app,
                "Unable to quit AstraX",
                &format!("Failover routing has not been restored yet, so AstraX will keep running.\n\n{error}\n\nCheck whether the config files are locked, then try quitting again."),
            );
        }
    }

    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Reopen { .. } = event {
        restore_main_window(app);
    }

    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}
