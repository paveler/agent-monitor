// Adds/removes Agent Monitor hooks in Claude Code and Codex configs.
// Our entries are recognised by their command ("…agent-monitor.exe --hook <agent>"); other hooks are left alone.
use crate::i18n::tr;
use crate::store;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub struct HookTarget {
    pub name: &'static str,
    pub agent: &'static str,
    pub path: PathBuf,
    pub events: &'static [(&'static str, Option<&'static str>)],
}

pub fn claude() -> HookTarget {
    HookTarget {
        name: "Claude Code",
        agent: "claude",
        path: store::home().join(".claude").join("settings.json"),
        events: &[
            ("SessionStart", None),
            ("SessionEnd", None),
            ("UserPromptSubmit", None),
            ("PreToolUse", Some("AskUserQuestion|ExitPlanMode")),
            ("PostToolUse", Some("*")),
            ("PostToolUseFailure", Some("*")),
            ("PermissionRequest", Some("*")),
            ("Notification", None),
            ("Stop", None),
            ("StopFailure", None),
        ],
    }
}

pub fn codex() -> HookTarget {
    HookTarget {
        name: "Codex",
        agent: "codex",
        path: store::home().join(".codex").join("hooks.json"),
        events: &[
            ("SessionStart", None),
            ("SessionEnd", None),
            ("UserPromptSubmit", None),
            ("PermissionRequest", None),
            ("PostToolUse", None),
            ("Stop", None),
            ("Interrupt", None),
        ],
    }
}

pub fn target(agent: &str) -> HookTarget {
    if agent == "codex" { codex() } else { claude() }
}

/// Hooks call a private copy of the app, so the command keeps working when the app is moved or rebuilt.
pub fn hook_exe() -> PathBuf {
    store::data_dir().join("bin").join("agent-monitor.exe")
}

/// Copies the running exe to `hook_exe()` when that copy is missing or differs.
pub fn sync_hook_exe() -> std::io::Result<()> {
    let me = std::env::current_exe()?;
    let dst = hook_exe();
    if me == dst {
        return Ok(());
    }
    let (a, b) = (std::fs::metadata(&me)?, std::fs::metadata(&dst).ok());
    let same = b.map(|b| b.len() == a.len() && b.modified().ok() >= a.modified().ok()).unwrap_or(false);
    if !same {
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(&me, &dst)?;
    }
    Ok(())
}

/// Forward slashes work in Git Bash, cmd and PowerShell alike; quotes only when the path needs them.
fn command(agent: &str) -> String {
    let exe = hook_exe().to_string_lossy().replace('\\', "/");
    if exe.contains(' ') { format!("\"{exe}\" --hook {agent}") } else { format!("{exe} --hook {agent}") }
}

fn is_ours(handler: &Value) -> bool {
    handler["command"].as_str().map(|c| c.to_lowercase().contains("agent-monitor") && c.contains("--hook")).unwrap_or(false)
}

fn read(t: &HookTarget) -> Result<Map<String, Value>, String> {
    match std::fs::read(&t.path) {
        Err(_) => Ok(Map::new()),
        Ok(d) if d.iter().all(|b| b.is_ascii_whitespace()) => Ok(Map::new()),
        // Never overwrite a config we can't parse: that would wipe the user's settings.
        Ok(d) => match serde_json::from_slice::<Value>(strip_bom(&d)) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(format!("{} {}", tr("Couldn't parse", "Не удалось разобрать"), t.path.display())),
        },
    }
}

fn strip_bom(d: &[u8]) -> &[u8] {
    d.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(d)
}

pub fn is_installed(t: &HookTarget) -> bool {
    let Ok(root) = read(t) else { return false };
    root.get("hooks").and_then(Value::as_object).map(|hooks| {
        hooks.values().any(|groups| {
            groups.as_array().map(|gs| gs.iter().any(|g| g["hooks"].as_array().map(|hs| hs.iter().any(is_ours)).unwrap_or(false))).unwrap_or(false)
        })
    }).unwrap_or(false)
}

/// Removes our handlers from every event; drops groups/events left empty.
fn stripped(hooks: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (event, value) in hooks {
        let Some(groups) = value.as_array() else {
            out.insert(event.clone(), value.clone());
            continue;
        };
        let kept: Vec<Value> = groups
            .iter()
            .filter_map(|g| {
                let Some(hs) = g.get("hooks").and_then(Value::as_array) else { return Some(g.clone()) };
                let rest: Vec<Value> = hs.iter().filter(|h| !is_ours(h)).cloned().collect();
                if rest.is_empty() {
                    return None;
                }
                let mut g2 = g.clone();
                g2["hooks"] = Value::Array(rest);
                Some(g2)
            })
            .collect();
        if !kept.is_empty() {
            out.insert(event.clone(), Value::Array(kept));
        }
    }
    out
}

fn hooks_of(root: &Map<String, Value>) -> Map<String, Value> {
    root.get("hooks").and_then(Value::as_object).cloned().unwrap_or_default()
}

pub fn install(t: &HookTarget) -> Result<(), String> {
    sync_hook_exe().map_err(|e| format!("{}: {e}", tr("Couldn't copy the hook handler", "Не удалось скопировать обработчик хуков")))?;
    add_hooks(t)
}

fn add_hooks(t: &HookTarget) -> Result<(), String> {
    let mut root = read(t)?;
    let mut hooks = stripped(&hooks_of(&root));
    for (event, matcher) in t.events {
        let mut group = json!({ "hooks": [{ "type": "command", "command": command(t.agent), "timeout": 5 }] });
        if let Some(m) = matcher {
            group["matcher"] = json!(m);
        }
        let mut list = hooks.get(*event).and_then(Value::as_array).cloned().unwrap_or_default();
        list.push(group);
        hooks.insert(event.to_string(), Value::Array(list));
    }
    root.insert("hooks".into(), Value::Object(hooks));
    write(&root, t)
}

pub fn uninstall(t: &HookTarget) -> Result<(), String> {
    let mut root = read(t)?;
    let hooks = stripped(&hooks_of(&root));
    if hooks.is_empty() {
        root.remove("hooks");
    } else {
        root.insert("hooks".into(), Value::Object(hooks));
    }
    write(&root, t)
}

fn write(root: &Map<String, Value>, t: &HookTarget) -> Result<(), String> {
    if t.path.exists() {
        let mut backup = t.path.clone().into_os_string();
        backup.push(".agentmonitor-backup");
        std::fs::copy(&t.path, &backup).map_err(|e| format!("{}: {e}", tr("Couldn't save the backup", "Не удалось сохранить бэкап")))?;
    }
    let mut data = serde_json::to_vec_pretty(root).map_err(|e| e.to_string())?;
    data.push(b'\n');
    store::write_atomic(&t.path, &data).map_err(|e| format!("{} {}: {e}", tr("Couldn't write", "Не удалось записать"), t.path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_target(name: &str, content: Option<&str>) -> HookTarget {
        let path = std::env::temp_dir().join(format!("agent-monitor-test-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        if let Some(c) = content {
            std::fs::write(&path, c).unwrap();
        }
        HookTarget { path, ..claude() }
    }

    fn load(t: &HookTarget) -> Value {
        serde_json::from_slice(&std::fs::read(&t.path).unwrap()).unwrap()
    }

    #[test]
    fn keeps_other_hooks_and_settings() {
        let original = r#"{
  "model": "opus",
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "node brain.js stop" }] }],
    "PreCompact": [{ "hooks": [{ "type": "command", "command": "node brain.js compact" }] }]
  },
  "permissions": { "allow": ["Bash(ls:*)"] }
}"#;
        let t = temp_target("keep", Some(original));
        assert!(!is_installed(&t));

        add_hooks(&t).unwrap();
        let v = load(&t);
        assert!(is_installed(&t));
        assert_eq!(v["model"], "opus");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        // Key order survives, the user's Stop hook stays first, ours is added after it.
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["model", "hooks", "permissions"]);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "node brain.js stop");
        assert!(is_ours(&v["hooks"]["Stop"][1]["hooks"][0]));
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion|ExitPlanMode");
        assert_eq!(v["hooks"]["PreCompact"].as_array().unwrap().len(), 1);

        // Connecting twice doesn't duplicate our hooks.
        add_hooks(&t).unwrap();
        assert_eq!(load(&t)["hooks"]["Stop"].as_array().unwrap().len(), 2);

        // Disconnecting leaves exactly what the user had.
        uninstall(&t).unwrap();
        assert!(!is_installed(&t));
        assert_eq!(load(&t), serde_json::from_str::<Value>(original).unwrap());

        let mut backup = t.path.clone().into_os_string();
        backup.push(".agentmonitor-backup");
        assert!(std::path::Path::new(&backup).exists());
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_file(&t.path);
    }

    #[test]
    fn creates_a_missing_config_and_removes_only_its_own_key() {
        let t = temp_target("new", None);
        add_hooks(&t).unwrap();
        assert!(is_installed(&t));
        uninstall(&t).unwrap();
        assert_eq!(load(&t), serde_json::json!({}));
        let _ = std::fs::remove_file(&t.path);
    }

    #[test]
    fn refuses_to_overwrite_a_config_it_cannot_parse() {
        let t = temp_target("broken", Some("{ not json"));
        assert!(add_hooks(&t).is_err());
        assert_eq!(std::fs::read_to_string(&t.path).unwrap(), "{ not json");
        let _ = std::fs::remove_file(&t.path);
    }
}
