// The tray app: polls usage, watches session files, owns the widget window and the tray menu.
use crate::model::{AgentSession, AgentUsage, History, Limit, SessionState, SessionView, Settings, Snapshot};
use crate::i18n::tr;
use crate::proc::ProcTable;
use crate::sources::{self, Outcome};
use crate::{claude_desktop, installer, jumper, store, trayicon};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent, Wry};

const WIDTH: f64 = 380.0;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "AgentMonitor";

pub struct Shared {
    snapshot: Mutex<Snapshot>,
    waiting: Mutex<Vec<AgentSession>>,
    history: Mutex<History>,
    settings: Mutex<Settings>,
    settings_dirty: AtomicBool,
    loading: AtomicBool,
    refresh_tx: Mutex<mpsc::Sender<bool>>,
    kick_tx: Mutex<mpsc::Sender<()>>,
}
type Sh = Arc<Shared>;

#[derive(Serialize, Clone)]
struct Hooks {
    claude: bool,
    codex: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StatePayload {
    snapshot: Snapshot,
    waiting: Vec<SessionView>,
    hooks: Hooks,
    loading: bool,
    settings: Settings,
    history: History,
    lang: &'static str,
}

fn payload(sh: &Shared) -> StatePayload {
    let settings = sh.settings.lock().unwrap().clone();
    StatePayload {
        snapshot: sh.snapshot.lock().unwrap().clone(),
        waiting: sh.waiting.lock().unwrap().iter().filter(|s| settings.shows(&s.agent)).map(SessionView::from).collect(),
        hooks: Hooks {
            claude: installer::is_installed(&installer::claude()),
            codex: installer::is_installed(&installer::codex()),
        },
        loading: sh.loading.load(Ordering::Relaxed),
        settings,
        history: sh.history.lock().unwrap().recent(store::now()),
        lang: crate::i18n::lang(),
    }
}

fn push(app: &AppHandle, sh: &Shared) {
    let _ = app.emit("state", payload(sh));
}

fn notice(app: &AppHandle, text: &str) {
    let _ = app.emit("notice", text.to_string());
}

// MARK: - usage limits

fn clock(t: f64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(t as i64, 0).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_default()
}

fn usage_loop(app: AppHandle, sh: Sh, rx: mpsc::Receiver<bool>) {
    // Claude's usage endpoint rate-limits aggressively: poll every 5 min, back off on 429.
    const CLAUDE_INTERVAL: f64 = 300.0;
    // Don't re-hit the API on every relaunch if the numbers are fresh.
    let mut claude_next = {
        let s = sh.snapshot.lock().unwrap();
        s.claude_retry_at.or(s.claude.fetched_at.map(|f| f + CLAUDE_INTERVAL)).unwrap_or(0.0)
    };
    let mut backoff = 0.0f64;
    let mut force = false;
    loop {
        sh.loading.store(true, Ordering::Relaxed);
        push(&app, &sh);
        let now = store::now();
        let snap = sh.snapshot.lock().unwrap().clone();

        let mut claude = snap.claude.clone();
        let mut last_good = snap
            .claude_last_good
            .clone()
            .or_else(|| (snap.claude.has_data() && snap.claude.error.is_none()).then(|| snap.claude.clone()));
        // Manual refresh skips the 5-min interval, but not a 429 backoff, and not more than every 20 s.
        let recent = snap.claude.fetched_at.map(|f| now - f < 20.0).unwrap_or(false);
        if now >= claude_next || (force && backoff == 0.0 && !recent) {
            match sources::claude_fetch() {
                Outcome::Ok(u) => {
                    last_good = Some(u.clone());
                    claude = u;
                    backoff = 0.0;
                    claude_next = now + CLAUDE_INTERVAL;
                }
                Outcome::Failed { reason, rate_limited, retry_after, auth } => {
                    if rate_limited {
                        backoff = if backoff == 0.0 { 300.0 } else { (backoff * 2.0).min(1800.0) };
                        claude_next = now + retry_after.unwrap_or(0.0).max(backoff);
                    } else {
                        claude_next = now + 120.0;
                    }
                    // Keep showing the last good numbers (marked "as of …"), unless the user has to act
                    // (auth), or there are none.
                    claude = match (&last_good, auth) {
                        (Some(good), false) => good.failed(&reason),
                        _ if rate_limited => AgentUsage::err(format!("{reason}, {} {}", tr("retry at", "повтор в"), clock(claude_next))),
                        _ => AgentUsage::err(reason),
                    };
                }
            }
        }

        // Codex: live API; on failure whichever is newer, the last known numbers or the local logs
        // (re-read every time: when the API never works, the logs are the only live source).
        let codex = sources::codex_api().unwrap_or_else(|| {
            let last = (snap.codex.has_data() && snap.codex.error.is_none()).then(|| snap.codex.clone());
            let logs = sources::codex_from_logs();
            let last_at = last.as_ref().and_then(|l| l.fetched_at).unwrap_or(0.0);
            let logs_at = logs.as_ref().and_then(|l| l.fetched_at).unwrap_or(0.0);
            match (logs, last) {
                (Some(logs), last) if logs_at > last_at || last.is_none() => logs,
                (_, Some(last)) => if last.stale_reason.is_none() { last.failed(tr("Offline", "Нет сети")) } else { last },
                _ if !sources::codex_signed_in() => AgentUsage::err(tr("Not signed in: run codex login", "Нет входа: выполните codex login")),
                _ => AgentUsage::err(tr("No data", "Нет данных")),
            }
        });

        let next = Snapshot { claude, codex, updated: Some(now), claude_last_good: last_good, claude_retry_at: Some(claude_next) };
        {
            let mut h = sh.history.lock().unwrap();
            h.record("claude", &next.claude, now);
            h.record("codex", &next.codex, now);
            store::save_history(&h);
        }
        store::save_snapshot(&next);
        *sh.snapshot.lock().unwrap() = next;
        sh.loading.store(false, Ordering::Relaxed);
        push(&app, &sh);
        update_tray(&app, &sh);

        force = match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        while rx.try_recv().is_ok() {}
    }
}

// MARK: - sessions

fn sessions_loop(app: AppHandle, sh: Sh, rx: mpsc::Receiver<()>) {
    let mut table = ProcTable::snapshot();
    let mut table_at = store::now();
    let mut shown: Option<Vec<SessionView>> = None;
    loop {
        let now = store::now();
        if now - table_at > 5.0 {
            table = ProcTable::snapshot();
            table_at = now;
        }
        let mut all = store::load_all();
        // Sweep sessions whose agent process is gone (SessionEnd isn't guaranteed, e.g. a closed terminal).
        // Only judge sessions the process list is newer than: the agent may have started after it was taken.
        if !table.is_empty() {
            all.retain(|s| match s.pid {
                Some(pid) if s.updated < table_at - 1.0 && !table.is_alive(pid) => {
                    store::remove_session(&s.id());
                    false
                }
                _ => true,
            });
        }
        // Claude desktop sessions: opening the session in the app after it finished counts as seen.
        for s in all.iter_mut() {
            if s.agent == "claude" && s.state == SessionState::YourTurn && !s.is_seen() {
                let focused = claude_desktop::info(&s.session_id).and_then(|i| i.last_focused_at);
                if focused.map(|f| f > s.since).unwrap_or(false) {
                    store::mark_seen(&s.id());
                    s.seen_at = Some(store::now());
                }
            }
        }
        let visible = store::visible(&all, now);
        let views: Vec<SessionView> = visible.iter().map(SessionView::from).collect();
        if shown.as_ref() != Some(&views) {
            shown = Some(views);
            *sh.waiting.lock().unwrap() = visible;
            push(&app, &sh);
            update_tray(&app, &sh);
        }
        if sh.settings_dirty.swap(false, Ordering::Relaxed) {
            store::save_settings(&sh.settings.lock().unwrap());
        }
        if let Err(RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_millis(700)) {
            return;
        }
    }
}

fn kick(sh: &Shared) {
    let _ = sh.kick_tx.lock().unwrap().send(());
}

// MARK: - tray

fn update_tray(app: &AppHandle, sh: &Shared) {
    let Some(tray) = app.tray_by_id("tray") else { return };
    let st = sh.settings.lock().unwrap().clone();
    let (count, blocking) = {
        let w = sh.waiting.lock().unwrap();
        let shown: Vec<&AgentSession> = w.iter().filter(|s| st.shows(&s.agent)).collect();
        (shown.len(), shown.iter().any(|s| s.state.is_blocking()))
    };
    let snap = sh.snapshot.lock().unwrap().clone();
    let now = store::now();
    let pct = |l: &Limit| match (l.percent, l.resets_at) {
        (Some(_), Some(r)) if r < now => "0%".to_string(),
        (Some(p), _) => format!("{}%", p.round() as i64),
        _ => "—".to_string(),
    };
    let line = |name: &str, u: &AgentUsage| match &u.error {
        Some(e) => format!("{name}: {e}"),
        None => format!("{name}: {} {} · {} {}", tr("5h", "5ч"), pct(&u.five_hour), tr("week", "нед"), pct(&u.week)),
    };
    let head = if count == 0 { tr("No sessions need your attention", "Нет сессий, требующих внимания").to_string() } else { format!("{}: {count}", tr("Waiting on you", "Ждут вас")) };
    let mut tip = head;
    if st.shows("claude") {
        tip.push_str(&format!("\n{}", line("Claude", &snap.claude)));
    }
    if st.shows("codex") {
        tip.push_str(&format!("\n{}", line("Codex", &snap.codex)));
    }
    let tip: String = tip.chars().take(120).collect();
    let _ = tray.set_icon(Some(Image::new_owned(trayicon::render(count, blocking), 32, 32)));
    let _ = tray.set_tooltip(Some(tip));
}

fn build_menu(app: &AppHandle, sh: &Shared) -> tauri::Result<Menu<Wry>> {
    let st = sh.settings.lock().unwrap().clone();
    let none = None::<&str>;
    let toggle = MenuItem::with_id(app, "toggle", if st.hidden { tr("Show Widget", "Показать виджет") } else { tr("Hide Widget", "Скрыть виджет") }, true, none)?;
    let on_top = CheckMenuItem::with_id(app, "on_top", tr("Always on Top", "Поверх всех окон"), true, st.on_top, none)?;
    let check = |id: &str, text: &str, on: bool| CheckMenuItem::with_id(app, id, text, true, on, none);
    let agents = Submenu::with_items(app, tr("Show", "Показывать"), true, &[
        &check("agents_both", tr("Claude and Codex", "Claude и Codex"), st.agents != "claude" && st.agents != "codex")?,
        &check("agents_claude", tr("Claude Only", "Только Claude"), st.agents == "claude")?,
        &check("agents_codex", tr("Codex Only", "Только Codex"), st.agents == "codex")?,
        &PredefinedMenuItem::separator(app)?,
        &check("show_limits", tr("Usage Limits", "Блок лимитов"), st.show_limits)?,
        &check("show_tasks", tr("Waiting on You", "Блок задач"), st.show_tasks)?,
    ])?;
    let layout = Submenu::with_items(app, tr("Session Layout", "Раскладка задач"), true, &[
        &check("view_list", tr("List (two lines)", "Список (две строки)"), st.view != "compact" && st.view != "cards")?,
        &check("view_compact", tr("Compact (one line)", "Компактно (одна строка)"), st.view == "compact")?,
        &check("view_cards", tr("Cards", "Карточки"), st.view == "cards")?,
        &PredefinedMenuItem::separator(app)?,
        &check("rows_3", tr("Show 3", "Показывать 3"), st.max_rows == 3)?,
        &check("rows_6", tr("Show 6", "Показывать 6"), st.max_rows == 6)?,
        &check("rows_12", tr("Show 12", "Показывать 12"), st.max_rows == 12)?,
        &check("rows_0", tr("Show All", "Показывать все"), st.max_rows == 0)?,
    ])?;
    let look = Submenu::with_items(app, tr("Appearance", "Вид"), true, &[
        &check("limits_auto", tr("Limits: Auto", "Лимиты: авто"), !["rings", "bars", "big"].contains(&st.limits_view.as_str()))?,
        &check("limits_rings", tr("Limits: Rings", "Лимиты: кольца"), st.limits_view == "rings")?,
        &check("limits_bars", tr("Limits: Bars", "Лимиты: полоски"), st.limits_view == "bars")?,
        &check("limits_big", tr("Limits: Large with Chart", "Лимиты: крупно с графиком"), st.limits_view == "big")?,
        &PredefinedMenuItem::separator(app)?,
        &check("size_narrow", tr("Size: Narrow", "Размер: узкий"), st.size == "narrow")?,
        &check("size_normal", tr("Size: Regular", "Размер: обычный"), st.size != "narrow" && st.size != "wide")?,
        &check("size_wide", tr("Size: Wide", "Размер: широкий"), st.size == "wide")?,
        &PredefinedMenuItem::separator(app)?,
        &check("theme_auto", tr("Theme: Match Windows", "Тема: как в Windows"), st.theme != "dark" && st.theme != "light")?,
        &check("theme_dark", tr("Theme: Dark", "Тема: тёмная"), st.theme == "dark")?,
        &check("theme_light", tr("Theme: Light", "Тема: светлая"), st.theme == "light")?,
    ])?;
    let refresh = MenuItem::with_id(app, "refresh", tr("Refresh", "Обновить"), true, none)?;
    let clear = MenuItem::with_id(app, "clear", tr("Clear Finished", "Убрать завершённые"), true, none)?;
    let claude = CheckMenuItem::with_id(app, "hook_claude", "Claude Code", true, installer::is_installed(&installer::claude()), none)?;
    let codex = CheckMenuItem::with_id(app, "hook_codex", "Codex", true, installer::is_installed(&installer::codex()), none)?;
    let integrations = Submenu::with_items(app, tr("Integrations (hooks)", "Интеграции (хуки)"), true, &[&claude, &codex])?;
    let autostart = CheckMenuItem::with_id(app, "autostart", tr("Launch at Login", "Запускать при входе в Windows"), true, autostart_enabled(), none)?;
    let quit = MenuItem::with_id(app, "quit", tr("Quit", "Выход"), true, none)?;
    let (sep1, sep2) = (PredefinedMenuItem::separator(app)?, PredefinedMenuItem::separator(app)?);
    Menu::with_items(app, &[&toggle, &on_top, &agents, &look, &layout, &sep1, &refresh, &clear, &integrations, &autostart, &sep2, &quit])
}

fn rebuild_menu(app: &AppHandle, sh: &Shared) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id("tray"), build_menu(app, sh)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    let sh = app.state::<Sh>().inner().clone();
    match id {
        "toggle" => {
            let hidden = {
                let mut s = sh.settings.lock().unwrap();
                s.hidden = !s.hidden;
                s.hidden
            };
            if let Some(win) = app.get_webview_window("main") {
                let _ = if hidden { win.hide() } else { win.show() };
            }
        }
        "on_top" => toggle_on_top(app, &sh),
        "refresh" => {
            let _ = sh.refresh_tx.lock().unwrap().send(true);
            kick(&sh);
        }
        "clear" => clear_finished_impl(&sh),
        "hook_claude" | "hook_codex" => {
            if let Err(e) = toggle_hooks_impl(app, &sh, id.trim_start_matches("hook_")) {
                notice(app, &e);
            }
        }
        "autostart" => {
            if let Err(e) = set_autostart(!autostart_enabled()) {
                notice(app, &format!("{}: {e}", tr("Couldn't change launch at login", "Не удалось изменить автозапуск")));
            }
        }
        "quit" => {
            store::save_settings(&sh.settings.lock().unwrap());
            app.exit(0);
        }
        "show_limits" | "show_tasks" => {
            // Never hide both blocks: turning the last one off brings the other back.
            let mut s = sh.settings.lock().unwrap();
            if id == "show_limits" { s.show_limits = !s.show_limits } else { s.show_tasks = !s.show_tasks }
            if !s.show_limits && !s.show_tasks {
                if id == "show_limits" { s.show_tasks = true } else { s.show_limits = true }
            }
        }
        _ => {
            let mut s = sh.settings.lock().unwrap();
            if let Some(v) = id.strip_prefix("agents_") {
                s.agents = v.to_string();
            } else if let Some(v) = id.strip_prefix("view_") {
                s.view = v.to_string();
            } else if let Some(v) = id.strip_prefix("limits_") {
                s.limits_view = v.to_string();
            } else if let Some(v) = id.strip_prefix("size_") {
                s.size = v.to_string();
            } else if let Some(v) = id.strip_prefix("theme_") {
                s.theme = v.to_string();
            } else if let Some(v) = id.strip_prefix("rows_") {
                s.max_rows = v.parse().unwrap_or(6);
            } else {
                return;
            }
        }
    }
    sh.settings_dirty.store(true, Ordering::Relaxed);
    rebuild_menu(app, &sh);
    push(app, &sh);
    update_tray(app, &sh);
}

/// On the desktop (under every window) or floating above them all.
fn apply_layer(win: &WebviewWindow, on_top: bool) {
    if on_top {
        let _ = win.set_always_on_bottom(false);
        let _ = win.set_always_on_top(true);
    } else {
        let _ = win.set_always_on_top(false);
        let _ = win.set_always_on_bottom(true);
    }
}

fn toggle_on_top(app: &AppHandle, sh: &Shared) {
    let on_top = {
        let mut s = sh.settings.lock().unwrap();
        s.on_top = !s.on_top || s.hidden;
        s.hidden = false;
        s.on_top
    };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        apply_layer(&win, on_top);
    }
}

fn autostart_enabled() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

fn set_autostart(on: bool) -> std::io::Result<()> {
    let (key, _) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
    if on {
        key.set_value(RUN_VALUE, &format!("\"{}\"", std::env::current_exe()?.display()))
    } else {
        key.delete_value(RUN_VALUE)
    }
}

fn toggle_hooks_impl(app: &AppHandle, sh: &Shared, agent: &str) -> Result<bool, String> {
    let t = installer::target(agent);
    let installed = installer::is_installed(&t);
    if installed {
        installer::uninstall(&t)?;
        notice(app, &format!("{}: {}", t.name, tr("hooks removed", "хуки удалены")));
    } else {
        installer::install(&t)?;
        notice(app, &if agent == "codex" {
            tr("Codex connected. Trust the hooks: /hooks in Codex CLI, or Settings → Hooks", "Codex подключён. Подтвердите хуки: /hooks в Codex CLI или Settings → Hooks").to_string()
        } else {
            tr("Claude Code connected. Restart sessions that are already open", "Claude Code подключён. Перезапустите уже открытые сессии").to_string()
        });
    }
    push(app, sh);
    Ok(!installed)
}

fn clear_finished_impl(sh: &Shared) {
    let ids: Vec<String> = sh.waiting.lock().unwrap().iter().filter(|s| s.state == SessionState::YourTurn).map(|s| s.id()).collect();
    for id in ids {
        store::mark_seen(&id);
    }
    kick(sh);
}

// MARK: - commands called by the widget

#[tauri::command]
fn get_state(sh: State<Sh>) -> StatePayload {
    payload(&sh)
}

#[tauri::command]
fn refresh(sh: State<Sh>) {
    let _ = sh.refresh_tx.lock().unwrap().send(true);
    kick(&sh);
}

/// Jump to the session; a finished turn counts as seen and leaves the list.
#[tauri::command]
fn open_session(sh: State<Sh>, id: String) {
    let found = store::load_session(&id).or_else(|| sh.waiting.lock().unwrap().iter().find(|s| s.id() == id).cloned());
    if let Some(s) = found {
        jumper::open(&s);
        store::mark_seen(&id);
        kick(&sh);
    }
}

#[tauri::command]
fn clear_finished(sh: State<Sh>) {
    clear_finished_impl(&sh);
}

#[tauri::command]
fn toggle_hooks(app: AppHandle, sh: State<Sh>, agent: String) -> Result<bool, String> {
    let r = toggle_hooks_impl(&app, &sh, if agent == "codex" { "codex" } else { "claude" });
    rebuild_menu(&app, &sh);
    r
}

#[tauri::command]
fn start_drag(win: WebviewWindow) {
    let _ = win.start_dragging();
}

/// The widget grows and shrinks with its content.
#[tauri::command]
fn set_height(win: WebviewWindow, sh: State<Sh>, height: f64) {
    let width = sh.settings.lock().unwrap().width();
    let _ = win.set_size(LogicalSize::new(width, height.clamp(60.0, 1600.0)));
}

// MARK: - startup

fn place_window(win: &WebviewWindow, st: &Settings) {
    let monitors = win.available_monitors().unwrap_or_default();
    let on_screen = |x: i32, y: i32| {
        monitors.iter().any(|m| {
            let (p, s) = (m.position(), m.size());
            x >= p.x - 40 && x < p.x + s.width as i32 - 80 && y >= p.y && y < p.y + s.height as i32 - 60
        })
    };
    match (st.x, st.y) {
        (Some(x), Some(y)) if on_screen(x, y) => {
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
        _ => {
            // Top right corner of the primary monitor.
            if let Ok(Some(m)) = win.primary_monitor() {
                let f = m.scale_factor();
                let x = m.position().x + m.size().width as i32 - ((WIDTH + 24.0) * f) as i32;
                let y = m.position().y + (24.0 * f) as i32;
                let _ = win.set_position(PhysicalPosition::new(x, y));
            }
        }
    }
}

pub fn run() {
    let (refresh_tx, refresh_rx) = mpsc::channel::<bool>();
    let (kick_tx, kick_rx) = mpsc::channel::<()>();
    let sh: Sh = Arc::new(Shared {
        snapshot: Mutex::new(store::load_snapshot().unwrap_or_default()),
        waiting: Mutex::new(vec![]),
        history: Mutex::new(store::load_history()),
        settings: Mutex::new(store::load_settings()),
        settings_dirty: AtomicBool::new(false),
        loading: AtomicBool::new(false),
        refresh_tx: Mutex::new(refresh_tx),
        kick_tx: Mutex::new(kick_tx),
    });
    let setup_sh = sh.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch just brings the widget up.
            let sh = app.state::<Sh>().inner().clone();
            sh.settings.lock().unwrap().hidden = false;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
            }
            rebuild_menu(app, &sh);
        }))
        .manage(sh)
        .invoke_handler(tauri::generate_handler![
            get_state, refresh, open_session, clear_finished, toggle_hooks, start_drag, set_height
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                // Alt+F4 hides the widget; the app lives in the tray.
                api.prevent_close();
                let _ = window.hide();
                let sh = window.state::<Sh>().inner().clone();
                sh.settings.lock().unwrap().hidden = true;
                sh.settings_dirty.store(true, Ordering::Relaxed);
                rebuild_menu(window.app_handle(), &sh);
            }
            WindowEvent::Moved(pos) if pos.x > -30000 && pos.y > -30000 => {
                let sh = window.state::<Sh>();
                let mut s = sh.settings.lock().unwrap();
                (s.x, s.y) = (Some(pos.x), Some(pos.y));
                sh.settings_dirty.store(true, Ordering::Relaxed);
            }
            _ => {}
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let sh = setup_sh;
            let st = sh.settings.lock().unwrap().clone();

            TrayIconBuilder::with_id("tray")
                .icon(Image::new_owned(trayicon::render(0, false), 32, 32))
                .tooltip("Agent Monitor")
                .menu(&build_menu(&handle, &sh)?)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    // Left click: bring the widget above the windows, click again to send it back.
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        let app = tray.app_handle();
                        let sh = app.state::<Sh>().inner().clone();
                        toggle_on_top(app, &sh);
                        sh.settings_dirty.store(true, Ordering::Relaxed);
                        rebuild_menu(app, &sh);
                    }
                })
                .build(app)?;
            app.on_menu_event(|app, event| on_menu(app, event.id().as_ref()));

            if let Some(win) = app.get_webview_window("main") {
                place_window(&win, &st);
                apply_layer(&win, st.on_top);
                if !st.hidden {
                    let _ = win.show();
                }
            }

            // Keep the hooks' private copy of the app in step with this build.
            std::thread::spawn(|| {
                if installer::is_installed(&installer::claude()) || installer::is_installed(&installer::codex()) {
                    let _ = installer::sync_hook_exe();
                }
            });
            let (a, s) = (handle.clone(), sh.clone());
            std::thread::spawn(move || usage_loop(a, s, refresh_rx));
            std::thread::spawn(move || sessions_loop(handle, sh, kick_rx));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Agent Monitor");
}
