// Brings the user to an agent session:
// • Claude desktop app → that session (claude://code/continue?session=local_…)
// • Codex app → that thread (codex://threads/<id>)
// • anything else (Windows Terminal, VS Code, …) → the window that hosts the agent comes to the front.
use crate::claude_desktop;
use crate::model::AgentSession;
use crate::proc::{self, ProcTable};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn open(s: &AgentSession) {
    if s.agent == "claude" {
        let local = s.host_session_id.clone().or_else(|| claude_desktop::info(&s.session_id).map(|i| i.local_id));
        if let Some(local) = local {
            shell_open(&format!("claude://code/continue?session={local}"));
            return;
        }
    }
    let Some(pid) = s.pid else { return };
    let table = ProcTable::snapshot();
    let windows = proc::windows();
    // The nearest ancestor that owns a window is the app hosting the agent.
    let host = table.ancestors(pid).into_iter().find_map(|p| windows.iter().find(|w| w.0 == p).copied());
    let Some((host_pid, hwnd)) = host else { return };
    if s.agent == "codex" && table.name(host_pid) == "codex" && is_uuid(&s.session_id) {
        shell_open(&format!("codex://threads/{}", s.session_id.to_lowercase()));
        return;
    }
    proc::focus(hwnd);
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn shell_open(url: &str) {
    let (verb, target) = (wide("open"), wide(url));
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}
