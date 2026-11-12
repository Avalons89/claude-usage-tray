#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod logs;
mod pricing;
mod settings;
mod tray_icon;

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, WebviewWindow, WindowEvent, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

// Local logs only; the plan limits follow account::CHECK_EVERY.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
const UPDATE_EVERY: Duration = Duration::from_secs(60 * 60);
#[cfg(windows)]
const RAISE_EVERY: Duration = Duration::from_secs(1);
// Windows cuts tray tooltips at 127 characters.
const TOOLTIP_MAX: usize = 127;
const AUTOSTART_LABEL: &str = if cfg!(windows) { "Start with Windows" } else { "Open at login" };

#[derive(Serialize, Clone)]
struct Snapshot {
    usage: logs::Summary,
    /// Last account reading that worked; kept when a later one fails.
    account: Option<account::Account>,
    account_error: Option<String>,
    updated_at: String,
}

struct Shared {
    latest: Mutex<Option<Snapshot>>,
    wake: mpsc::Sender<()>,
    check_update: mpsc::Sender<()>,
    settings: Mutex<settings::Settings>,
    settings_path: PathBuf,
}

#[tauri::command]
fn snapshot(shared: tauri::State<Shared>) -> Option<Snapshot> {
    shared.latest.lock().ok().and_then(|latest| latest.clone())
}

#[tauri::command]
fn get_settings(shared: tauri::State<Shared>) -> settings::Settings {
    shared.settings.lock().map(|choices| choices.clone()).unwrap_or_default()
}

/// The page measures itself and the window follows, so folding and unfolding resize it.
#[tauri::command]
fn fit(window: WebviewWindow, width: f64, height: f64) {
    let _ = window.set_size(LogicalSize::new(width, height));
}

#[tauri::command]
fn start_drag(window: WebviewWindow, shared: tauri::State<Shared>) {
    let locked = shared.settings.lock().map(|choices| choices[settings::LOCKED.0]).unwrap_or(false);
    if !locked {
        let _ = window.start_dragging();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_window_state::Builder::default().with_state_flags(StateFlags::POSITION).build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![snapshot, get_settings, fit, start_drag])
        .on_window_event(|window, event| match event {
            // ponytail: saves on every move event while dragging; debounce if the disk writes ever matter.
            WindowEvent::Moved(_) => {
                let _ = window.app_handle().save_window_state(StateFlags::POSITION);
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .setup(|app| {
            // Menu bar only, no Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let (wake, woken) = mpsc::channel();
            let (check_update, update_asked) = mpsc::channel();
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let choices = settings::load(&settings_path);
            if let Some(window) = app.get_webview_window("main") {
                window.set_always_on_top(choices[settings::KEEP_ON_TOP.0])?;
            }
            app.manage(Shared { latest: Mutex::new(None), wake, check_update, settings: Mutex::new(choices), settings_path });
            let update_item = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
            let tray = build_tray(app.handle(), &update_item)?;
            let handle = app.handle().clone();
            std::thread::spawn(move || refresh_loop(handle, tray, woken));
            let handle = app.handle().clone();
            std::thread::spawn(move || update_loop(handle, update_item, update_asked));
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || keep_raised(handle));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start Claude usage");
}

fn build_tray(app: &AppHandle, update_item: &MenuItem<Wry>) -> tauri::Result<TrayIcon> {
    let toggle = MenuItem::with_id(app, "toggle", "Show or hide the strip", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let starts_at_login = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(app, "autostart", AUTOSTART_LABEL, true, starts_at_login, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let saved = app.state::<Shared>().settings.lock().map(|choices| choices.clone()).unwrap_or_default();
    let tick = |&(key, label, _): &(&str, &str, bool)| {
        CheckMenuItem::with_id(app, key, label, true, saved.get(key).copied().unwrap_or(false), None::<&str>)
    };
    let strip_ticks = settings::STRIP.iter().map(tick).collect::<tauri::Result<Vec<_>>>()?;
    let strip_items: Vec<&dyn IsMenuItem<Wry>> = strip_ticks.iter().map(|t| t as &dyn IsMenuItem<Wry>).collect();
    let show_on_strip = Submenu::with_items(app, "Show on strip", true, &strip_items)?;
    let keep_on_top = tick(&settings::KEEP_ON_TOP)?;
    let locked = tick(&settings::LOCKED)?;
    let separator = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &refresh,
            &separator()?,
            &show_on_strip,
            &keep_on_top,
            &locked,
            &separator()?,
            &autostart,
            update_item,
            &separator()?,
            &quit,
        ],
    )?;
    let ticks: Vec<CheckMenuItem<Wry>> = strip_ticks.into_iter().chain([keep_on_top, locked]).collect();

    TrayIconBuilder::with_id("usage")
        .icon(icon(None))
        .tooltip("Claude usage: reading logs")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => toggle_window(app),
            "refresh" => {
                let _ = app.state::<Shared>().wake.send(());
            }
            "update" => {
                let _ = app.state::<Shared>().check_update.send(());
            }
            "autostart" => {
                let launcher = app.autolaunch();
                let _ = if autostart.is_checked().unwrap_or(false) { launcher.enable() } else { launcher.disable() };
                let _ = autostart.set_checked(launcher.is_enabled().unwrap_or(false));
            }
            "quit" => {
                let _ = app.save_window_state(StateFlags::POSITION);
                app.exit(0);
            }
            id => {
                if let Some(tick) = ticks.iter().find(|t| t.id().as_ref() == id) {
                    set_choice(app, id, tick.is_checked().unwrap_or(false));
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)
}

fn set_choice(app: &AppHandle, key: &str, on: bool) {
    let shared = app.state::<Shared>();
    let Ok(mut choices) = shared.settings.lock() else { return };
    choices.insert(key.to_string(), on);
    settings::save(&shared.settings_path, &choices);
    if key == settings::KEEP_ON_TOP.0 {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_always_on_top(on);
        }
    }
    let _ = app.emit("settings", &*choices);
}

/// Windows lets the taskbar and other always-on-top windows cover the strip until it is raised again.
/// It is raised as soon as another window comes to the front, such as the taskbar when clicked,
/// and once a second for windows that appear on top without taking focus.
#[cfg(windows)]
fn keep_raised(app: AppHandle) {
    use windows::Win32::UI::Accessibility::SetWinEventHook;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };
    let _ = RAISE_APP.set(app.clone());
    std::thread::spawn(move || loop {
        std::thread::sleep(RAISE_EVERY);
        raise_strip(&app);
    });
    // SAFETY: the callback only reads RAISE_APP. Out-of-context hooks are delivered through this
    // thread's message loop, which runs for the life of the app.
    unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

/// WinEvent callbacks carry no context, so the hook finds the app here.
#[cfg(windows)]
static RAISE_APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

#[cfg(windows)]
unsafe extern "system" fn on_foreground(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    _hwnd: windows::Win32::Foundation::HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if let Some(app) = RAISE_APP.get() {
        raise_strip(app);
    }
}

#[cfg(windows)]
fn raise_strip(app: &AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let on_top = app.state::<Shared>().settings.lock().map(|choices| choices[settings::KEEP_ON_TOP.0]).unwrap_or(false);
    let shown = app.get_webview_window("main").filter(|w| on_top && w.is_visible().unwrap_or(false));
    if let Some(hwnd) = shown.and_then(|w| w.hwnd().ok()) {
        // SAFETY: the handle belongs to the live strip window, and the call only changes its z-order.
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
        }
    }
}

fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    let _ = if window.is_visible().unwrap_or(false) { window.hide() } else { window.show() };
}

fn refresh_loop(app: AppHandle, tray: TrayIcon, woken: mpsc::Receiver<()>) {
    let claude_dirs = claude_dirs(&app);
    let mut ledger = logs::Ledger::default();
    let mut last_account = None;
    let mut account_error = None;
    let mut next_account_check = Instant::now();
    loop {
        let now = Local::now();
        for dir in &claude_dirs {
            ledger.tail(&dir.join("projects"), logs::scan_cutoff(&now));
        }
        if Instant::now() >= next_account_check {
            let wait = match account::fetch(&claude_dirs) {
                Ok(account) => {
                    last_account = Some(account);
                    account_error = None;
                    account::CHECK_EVERY
                }
                Err(error) => {
                    account_error = Some(error.message);
                    error.retry_in
                }
            };
            next_account_check = Instant::now() + wait;
        }
        let snapshot = Snapshot {
            usage: ledger.summary(&now),
            account: last_account.clone(),
            account_error: account_error.clone(),
            updated_at: now.format("%H:%M").to_string(),
        };

        let session = snapshot.account.as_ref().and_then(|a| a.limits.iter().find(|l| l.kind == "session"));
        let _ = tray.set_icon(Some(icon(session.map(|l| l.percent))));
        let _ = tray.set_tooltip(Some(tooltip(&snapshot, Utc::now())));
        if let Ok(mut latest) = app.state::<Shared>().latest.lock() {
            *latest = Some(snapshot.clone());
        }
        let _ = app.emit("snapshot", &snapshot);

        if let Err(RecvTimeoutError::Disconnected) = woken.recv_timeout(REFRESH_EVERY) {
            return;
        }
    }
}

/// Checks at launch, every hour and from the tray menu. Debug builds only check when asked,
/// so a dev copy doesn't replace itself with the published release.
fn update_loop(app: AppHandle, item: MenuItem<Wry>, asked: mpsc::Receiver<()>) {
    let automatic = !cfg!(debug_assertions);
    let mut due = automatic;
    loop {
        if due {
            let label = tauri::async_runtime::block_on(install_update(&app, &item));
            let _ = item.set_text(label);
        }
        due = match asked.recv_timeout(UPDATE_EVERY) {
            Ok(()) => true,
            Err(RecvTimeoutError::Timeout) => automatic,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

/// Installs a newer release and restarts into it. Returns the menu label when nothing was installed.
/// On Windows the installer closes the app itself and starts the new version.
async fn install_update(app: &AppHandle, item: &MenuItem<Wry>) -> String {
    let _ = item.set_text("Checking for updates…");
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    match found {
        Ok(None) => format!("Up to date (v{})", app.package_info().version),
        Err(_) => "Couldn't check for updates. Try again".into(),
        Ok(Some(update)) => {
            let _ = item.set_text(format!("Installing v{}…", update.version));
            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => app.restart(),
                Err(_) => "Update failed. Try again".into(),
            }
        }
    }
}

/// Claude Code keeps its data in ~/.claude, or ~/.config/claude on some Linux and macOS installs.
fn claude_dirs(app: &AppHandle) -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return vec![PathBuf::from(dir)];
    }
    let home = app.path().home_dir().unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    vec![home.join(".claude"), config.join("claude")]
}

fn icon(percent: Option<f64>) -> Image<'static> {
    Image::new_owned(tray_icon::render(percent), tray_icon::SIZE, tray_icon::SIZE)
}

fn tooltip(snapshot: &Snapshot, now: DateTime<Utc>) -> String {
    let mut lines = Vec::new();
    let limits = snapshot.account.as_ref().map(|a| a.limits.as_slice()).unwrap_or_default();
    for limit in limits.iter().filter(|l| l.kind == "session") {
        let reset = limit.resets_at.as_deref().and_then(|at| time_left(at, now));
        lines.push(format!("Session {:.0}%{}", limit.percent, reset.map(|t| format!(" · resets in {t}")).unwrap_or_default()));
    }
    let weekly: Vec<String> =
        limits.iter().filter(|l| l.kind != "session").map(|l| format!("{} {:.0}%", l.label, l.percent)).collect();
    if !weekly.is_empty() {
        lines.push(weekly.join(" · "));
    }
    let usage = &snapshot.usage;
    lines.push(format!("Today {} tok · ${:.2} at API prices", compact(usage.today.total()), usage.cost_today));
    if let Some(error) = &snapshot.account_error {
        lines.push(error.clone());
    }
    lines.join("\n").chars().take(TOOLTIP_MAX).collect()
}

fn time_left(resets_at: &str, now: DateTime<Utc>) -> Option<String> {
    let resets_at = DateTime::parse_from_rfc3339(resets_at).ok()?.with_timezone(&Utc);
    let minutes = (resets_at - now).num_minutes().max(0);
    Some(format!("{}h{:02}m", minutes / 60, minutes % 60))
}

fn compact(tokens: u64) -> String {
    match tokens {
        t if t >= 1_000_000 => format!("{:.1}M", t as f64 / 1e6),
        t if t >= 1_000 => format!("{:.0}K", t as f64 / 1e3),
        t => t.to_string(),
    }
}
