// Files in %LOCALAPPDATA%\AgentMonitor: snapshot.json, settings.json and sessions\<id>.json.
// Hooks write one file per session; the app reads them and shows the ones waiting on the user.
use crate::model::{AgentSession, SessionState, Settings, Snapshot};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const BLOCKING_TTL: f64 = 24.0 * 3600.0;
pub const YOUR_TURN_TTL: f64 = 6.0 * 3600.0;

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
}

pub fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local"))
}

pub fn data_dir() -> PathBuf {
    local_app_data().join("AgentMonitor")
}

pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

/// Write to a temp file and rename over the target, so readers never see half a file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    // A reader may hold the target open for a moment: retry briefly.
    let mut last = Ok(());
    for _ in 0..10 {
        last = std::fs::rename(&tmp, path);
        if last.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let _ = std::fs::remove_file(&tmp);
    last
}

/// Appends a line to app.log (kept small: it starts over past 200 KB).
pub fn log(msg: &str) {
    use std::io::Write;
    let path = data_dir().join("app.log");
    if std::fs::metadata(&path).map(|m| m.len() > 200_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(data) = serde_json::to_vec(value) {
        let _ = write_atomic(path, &data);
    }
}

pub fn load_snapshot() -> Option<Snapshot> {
    read_json(&data_dir().join("snapshot.json"))
}
pub fn save_snapshot(s: &Snapshot) {
    write_json(&data_dir().join("snapshot.json"), s)
}
pub fn load_history() -> crate::model::History {
    read_json(&data_dir().join("history.json")).unwrap_or_default()
}
pub fn save_history(h: &crate::model::History) {
    write_json(&data_dir().join("history.json"), h)
}
pub fn load_settings() -> Settings {
    read_json(&data_dir().join("settings.json")).unwrap_or_default()
}
pub fn save_settings(s: &Settings) {
    write_json(&data_dir().join("settings.json"), s)
}

fn session_file(id: &str) -> PathBuf {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    sessions_dir().join(format!("{safe}.json"))
}

pub fn load_session(id: &str) -> Option<AgentSession> {
    read_json(&session_file(id))
}

pub fn save_session(s: &AgentSession) {
    write_json(&session_file(&s.id()), s)
}

pub fn remove_session(id: &str) {
    let _ = std::fs::remove_file(session_file(id));
}

/// Hide a finished turn. Blockers stay until the agent reports they're resolved.
pub fn mark_seen(id: &str) {
    if let Some(mut s) = load_session(id) {
        if s.state == SessionState::YourTurn {
            s.seen_at = Some(now());
            save_session(&s);
        }
    }
}

pub fn load_all() -> Vec<AgentSession> {
    let Ok(dir) = std::fs::read_dir(sessions_dir()) else { return vec![] };
    dir.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|p| read_json(&p))
        .collect()
}

/// Sessions worth showing: blockers first, then finished turns; oldest (most stuck) first.
pub fn visible(all: &[AgentSession], now: f64) -> Vec<AgentSession> {
    let mut out: Vec<AgentSession> = all
        .iter()
        .filter(|s| {
            let age = now - s.since;
            if s.state.is_blocking() {
                age < BLOCKING_TTL
            } else if s.state == SessionState::YourTurn {
                age < YOUR_TURN_TTL && !s.is_seen()
            } else {
                false
            }
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| (a.state.rank(), a.since).partial_cmp(&(b.state.rank(), b.since)).unwrap_or(std::cmp::Ordering::Equal));
    out
}
