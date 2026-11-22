// Claude desktop app's own session records:
// <Claude data>\claude-code-sessions\<account>\<org>\local_<id>.json
// Each maps the CLI session id (what hooks send) to the app's session id (what its deep links take),
// and tracks when the user last focused the session.
// <Claude data> is %APPDATA%\Claude, or under %LOCALAPPDATA%\Packages\Claude_*\ for the Store build.
use crate::hook::is_local_id;
use crate::store;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

pub struct Info {
    pub local_id: String,
    pub last_focused_at: Option<f64>,
}

struct Index {
    path_by_cli: HashMap<String, PathBuf>,
    last_scan: f64,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

fn roots() -> Vec<PathBuf> {
    let mut out = vec![];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(PathBuf::from(appdata).join("Claude").join("claude-code-sessions"));
    }
    if let Ok(rd) = std::fs::read_dir(store::local_app_data().join("Packages")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("Claude_") {
                out.push(e.path().join("LocalCache").join("Roaming").join("Claude").join("claude-code-sessions"));
            }
        }
    }
    out
}

pub fn info(cli_session_id: &str) -> Option<Info> {
    let mut guard = INDEX.lock().ok()?;
    let idx = guard.get_or_insert(Index { path_by_cli: HashMap::new(), last_scan: 0.0 });
    if !idx.path_by_cli.contains_key(cli_session_id) && store::now() - idx.last_scan > 60.0 {
        idx.last_scan = store::now();
        let cutoff = SystemTime::now() - Duration::from_secs(7 * 86400);
        for root in roots() {
            scan(&root, 0, cutoff, &mut idx.path_by_cli);
        }
    }
    let o = read(idx.path_by_cli.get(cli_session_id)?)?;
    if o["cliSessionId"].as_str() != Some(cli_session_id) {
        return None;
    }
    let local_id = o["sessionId"].as_str().filter(|s| is_local_id(s))?.to_string();
    Some(Info { local_id, last_focused_at: o["lastFocusedAt"].as_f64().map(|ms| ms / 1000.0) })
}

/// Indexes records touched in the last week (active sessions); there can be thousands of old ones.
fn scan(dir: &Path, depth: u8, cutoff: SystemTime, out: &mut HashMap<String, PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            if depth < 4 {
                scan(&p, depth + 1, cutoff, out);
            }
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("local_") || !name.ends_with(".json") {
            continue;
        }
        if meta.modified().map(|m| m < cutoff).unwrap_or(true) {
            continue;
        }
        if let Some(cli) = read(&p).and_then(|o| o["cliSessionId"].as_str().map(str::to_string)) {
            out.insert(cli, p);
        }
    }
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
