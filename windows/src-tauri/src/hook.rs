// Hook handler: `agent-monitor --hook claude|codex` is called by the agent on lifecycle events.
// Reads the event JSON from stdin, updates the session's state file, prints nothing and always
// exits 0, so it never changes the agent's behavior.
use crate::model::{AgentSession, SessionState};
use crate::proc::ProcTable;
use crate::store;
use serde_json::Value;
use std::io::Read;

pub fn run(agent: &str) {
    std::panic::set_hook(Box::new(|_| {}));
    let agent = agent.to_string();
    let result = std::panic::catch_unwind(move || handle(&agent));
    if result.is_err() {
        debug("panic");
    }
    std::process::exit(0);
}

/// Set AGENT_MONITOR_DEBUG=1 to trace hook calls into %LOCALAPPDATA%\AgentMonitor\hook.log.
fn debug(msg: &str) {
    if std::env::var_os("AGENT_MONITOR_DEBUG").is_some() {
        use std::io::Write;
        let path = store::data_dir().join("hook.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{:.0} {msg}", store::now());
        }
    }
}

fn handle(agent: &str) {
    let mut data = Vec::new();
    let read = std::io::stdin().read_to_end(&mut data);
    debug(&format!("{agent}: stdin {read:?}"));
    if read.is_err() {
        return;
    }
    let j = match serde_json::from_slice::<Value>(&data) {
        Ok(j) => j,
        Err(e) => return debug(&format!("bad json: {e}")),
    };
    let (Some(sid), Some(event)) = (j["session_id"].as_str(), j["hook_event_name"].as_str()) else { return };

    let now = store::now();
    let id = format!("{agent}-{sid}");
    if event == "SessionEnd" {
        store::remove_session(&id);
        return;
    }

    let cwd = j["cwd"].as_str().unwrap_or("");
    let mut s = store::load_session(&id).unwrap_or(AgentSession {
        agent: agent.to_string(),
        session_id: sid.to_string(),
        cwd: cwd.to_string(),
        title: None,
        state: SessionState::Idle,
        detail: None,
        since: now,
        updated: now,
        pid: None,
        seen_at: None,
        host_session_id: None,
    });
    if !cwd.is_empty() {
        s.cwd = cwd.to_string();
    }
    if let Some(t) = j["session_title"].as_str().filter(|t| !t.is_empty()) {
        s.title = Some(t.to_string());
    }
    if agent == "codex" {
        if let Some(t) = codex_thread_name(sid) {
            s.title = Some(t);
        }
    }
    // Sessions run by the Claude desktop app carry its session id; it opens the exact session via deep link.
    if let Ok(h) = std::env::var("CLAUDE_CODE_HOST_SESSION_ID") {
        if is_local_id(&h) {
            s.host_session_id = Some(h);
        }
    }
    let table = ProcTable::snapshot();
    if let Some(pid) = table.agent_pid() {
        s.pid = Some(pid);
    }

    let tool = j["tool_name"].as_str();
    let next: Option<(SessionState, Option<String>)> = match event {
        "SessionStart" => Some((SessionState::Idle, None)),
        "UserPromptSubmit" => {
            if s.title.is_none() {
                if let Some(p) = j["prompt"].as_str() {
                    let t = first_line(p, 60);
                    if !t.is_empty() {
                        s.title = Some(t);
                    }
                }
            }
            Some((SessionState::Working, None))
        }
        "PreToolUse" => match tool {
            Some("AskUserQuestion") => Some((SessionState::Question, question_text(&j["tool_input"]))),
            Some("ExitPlanMode") => Some((SessionState::Plan, None)),
            _ => Some((SessionState::Working, None)),
        },
        "PostToolUse" | "PostToolUseFailure" => Some((SessionState::Working, None)),
        "PermissionRequest" => Some((SessionState::Approval, describe(tool, &j["tool_input"]))),
        "Notification" => match j["notification_type"].as_str() {
            Some("permission_prompt") if !s.state.is_blocking() => {
                Some((SessionState::Approval, j["message"].as_str().map(|m| first_line(m, 90))))
            }
            Some("elicitation_dialog") | Some("elicitation_url_dialog") | Some("agent_needs_input") => {
                Some((SessionState::Question, j["message"].as_str().map(|m| first_line(m, 90))))
            }
            _ => None,
        },
        "Stop" => {
            let busy = |k: &str| j[k].as_array().map(|a| !a.is_empty()).unwrap_or(false);
            if busy("background_tasks") || busy("session_crons") {
                Some((SessionState::Working, None))
            } else {
                let msg = j["last_assistant_message"].as_str().unwrap_or("");
                Some((SessionState::YourTurn, Some(first_line(&plain(msg), 90))))
            }
        }
        "StopFailure" => Some((SessionState::YourTurn, Some(crate::i18n::tr("Stopped with an error", "Остановился с ошибкой").to_string()))),
        "Interrupt" => Some((SessionState::Idle, None)),
        _ => None,
    };

    let Some((state, detail)) = next else { return };
    // A new finished turn or permission request is a fresh event even if the state looks the same.
    if state != s.state || event == "Stop" || event == "PermissionRequest" {
        s.since = now;
    }
    s.state = state;
    s.detail = detail.filter(|d| !d.is_empty());
    s.updated = now;
    debug(&format!("{id}: {event} -> {state:?}"));
    store::save_session(&s);
}

pub fn is_local_id(s: &str) -> bool {
    match s.strip_prefix("local_") {
        Some(rest) => (1..=64).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        None => false,
    }
}

/// Strips common Markdown so a summary reads as plain text:
/// [text](url) → text, **bold**, `code`, # headings, list bullets.
pub fn plain(s: &str) -> String {
    // Links and images.
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let start = if chars[i] == '!' && chars.get(i + 1) == Some(&'[') { i + 1 } else { i };
        if chars[start] == '[' {
            if let Some(close) = chars[start + 1..].iter().position(|&c| c == ']').map(|p| p + start + 1) {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = chars[close + 2..].iter().position(|&c| c == ')').map(|p| p + close + 2) {
                        out.extend(&chars[start + 1..close]);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    let out = out.replace("**", "").replace("__", "").replace('`', "");
    // Heading marks, bullets and list numbers at the start of a line.
    out.lines()
        .map(|line| {
            let indent = line.len() - line.trim_start_matches(' ').len();
            if indent > 3 {
                return line.to_string();
            }
            let t = &line[indent..];
            let hashes = t.chars().take_while(|&c| c == '#').count();
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            let rest = if (1..=6).contains(&hashes) {
                &t[hashes..]
            } else if t.starts_with(['-', '*', '+']) {
                &t[1..]
            } else if digits > 0 && t[digits..].starts_with('.') {
                &t[digits + 1..]
            } else {
                return line.to_string();
            };
            if rest.starts_with(char::is_whitespace) { rest.trim_start().to_string() } else { line.to_string() }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// First non-empty line, trimmed and cut to `max` characters.
pub fn first_line(s: &str, max: usize) -> String {
    let t = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if t.chars().count() > max {
        let cut: String = t.chars().take(max - 1).collect();
        format!("{cut}…")
    } else {
        t.to_string()
    }
}

fn describe(tool: Option<&str>, input: &Value) -> Option<String> {
    let tool = tool?;
    let mut name = tool.to_string();
    if tool.starts_with("mcp__") {
        // mcp__server__tool → server · tool
        let parts: Vec<&str> = tool.split('_').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 3 {
            name = format!("{} · {}", parts[1], parts[2..].join("_"));
        }
    }
    if let Some(cmd) = input["command"].as_str() {
        return Some(format!("{name}: {}", first_line(cmd, 70)));
    }
    if let Some(p) = input["file_path"].as_str().or(input["path"].as_str()) {
        let file = p.rsplit(['/', '\\']).next().unwrap_or(p);
        return Some(format!("{name}: {file}"));
    }
    if let Some(u) = input["url"].as_str() {
        return Some(format!("{name}: {}", first_line(u, 70)));
    }
    if let Some(d) = input["description"].as_str() {
        return Some(format!("{name}: {}", first_line(d, 70)));
    }
    Some(name)
}

fn question_text(input: &Value) -> Option<String> {
    input["questions"][0]["question"].as_str().map(|q| first_line(q, 90))
}

/// Codex keeps thread names in ~/.codex/session_index.jsonl ({"id", "thread_name"}); the latest entry wins.
fn codex_thread_name(id: &str) -> Option<String> {
    let text = tail(&store::home().join(".codex").join("session_index.jsonl"), 512_000)?;
    for line in text.lines().rev().filter(|l| l.contains(id)) {
        if let Ok(o) = serde_json::from_str::<Value>(line) {
            if o["id"].as_str() == Some(id) {
                if let Some(name) = o["thread_name"].as_str().filter(|n| !n.is_empty()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// The last `max` bytes of a file as text.
pub fn tail(path: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    if size > max {
        f.seek(SeekFrom::Start(size - max)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_strips_markdown() {
        assert_eq!(first_line(&plain("## Готово\n**Строки** теперь [кликабельны](http://x)"), 90), "Готово");
        assert_eq!(plain("- see `code` and ![img](a.png)"), "see code and img");
        assert_eq!(plain("2. second [x] item"), "second [x] item");
    }

    #[test]
    fn first_line_cuts_by_characters() {
        assert_eq!(first_line("\n  привет мир  \nвторая", 6), "приве…");
        assert_eq!(first_line("", 10), "");
    }

    #[test]
    fn describes_tool_calls() {
        let input = serde_json::json!({ "command": "npm run deploy\necho done" });
        assert_eq!(describe(Some("Bash"), &input).unwrap(), "Bash: npm run deploy");
        let input = serde_json::json!({ "file_path": r"C:\work\app\main.rs" });
        assert_eq!(describe(Some("Edit"), &input).unwrap(), "Edit: main.rs");
        assert_eq!(describe(Some("mcp__github__create_issue"), &Value::Null).unwrap(), "github · create_issue");
    }

    #[test]
    fn validates_desktop_session_ids() {
        assert!(is_local_id("local_e22ca7e5-0bb1-4b34"));
        assert!(!is_local_id("local_"));
        assert!(!is_local_id("local_a&b=c"));
        assert!(!is_local_id("remote_abc"));
    }
}
