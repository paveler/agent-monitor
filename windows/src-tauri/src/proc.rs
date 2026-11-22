// Process tree and top-level windows (Win32).
use std::collections::HashMap;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, ShowWindow, GW_OWNER, SW_RESTORE,
};

/// pid → (parent pid, exe name lowercased without ".exe")
pub struct ProcTable(HashMap<u32, (u32, String)>);

impl ProcTable {
    pub fn snapshot() -> ProcTable {
        let mut map = HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return ProcTable(map);
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                    let mut name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
                    if name.ends_with(".exe") {
                        name.truncate(name.len() - 4);
                    }
                    map.insert(e.th32ProcessID, (e.th32ParentProcessID, name));
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        ProcTable(map)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn is_alive(&self, pid: u32) -> bool {
        self.0.contains_key(&pid)
    }
    pub fn parent(&self, pid: u32) -> Option<u32> {
        self.0.get(&pid).map(|p| p.0).filter(|&p| p != 0 && p != pid)
    }
    pub fn name(&self, pid: u32) -> &str {
        self.0.get(&pid).map(|p| p.1.as_str()).unwrap_or("")
    }

    /// pid and its ancestors, nearest first.
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![];
        let mut p = pid;
        while self.is_alive(p) && out.len() < 32 && !out.contains(&p) {
            out.push(p);
            match self.parent(p) {
                Some(pp) => p = pp,
                None => break,
            }
        }
        out
    }

    /// The agent process: walk up from this hook process past the shell(s) that ran it.
    pub fn agent_pid(&self) -> Option<u32> {
        const SHELLS: [&str; 10] = ["sh", "bash", "zsh", "dash", "fish", "env", "cmd", "powershell", "pwsh", "conhost"];
        let mut p = self.parent(std::process::id())?;
        for _ in 0..16 {
            if !SHELLS.contains(&self.name(p)) {
                break;
            }
            match self.parent(p) {
                Some(pp) if self.is_alive(pp) => p = pp,
                _ => break,
            }
        }
        Some(p)
    }
}

unsafe extern "system" fn collect(hwnd: HWND, l: LPARAM) -> BOOL {
    let v = &mut *(l as *mut Vec<(u32, isize)>);
    if IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() && GetWindowTextLengthW(hwnd) > 0 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        v.push((pid, hwnd as isize));
    }
    1
}

/// Visible, titled, unowned top-level windows: (owner pid, hwnd).
pub fn windows() -> Vec<(u32, isize)> {
    let mut v: Vec<(u32, isize)> = vec![];
    unsafe {
        EnumWindows(Some(collect), &mut v as *mut _ as LPARAM);
    }
    v
}

pub fn focus(hwnd: isize) {
    unsafe {
        let h = hwnd as HWND;
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
    }
}
