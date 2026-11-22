// Agent Monitor for Windows: a tray app with a desktop widget.
// • Fetches Claude/Codex usage limits.
// • Watches session files written by agent hooks and shows who's waiting on you.
// • `agent-monitor --hook <agent>` runs as a hook handler and exits (see hook.rs).
// Always a GUI-subsystem binary, so a hook never flashes a console window.
#![windows_subsystem = "windows"]

mod app;
mod claude_desktop;
mod hook;
mod i18n;
mod installer;
mod jumper;
mod model;
mod proc;
mod sources;
mod store;
mod trayicon;

fn main() {
    // Hook mode: handle one event and exit before any UI starts.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--hook") {
        let agent = if args.get(i + 1).map(String::as_str) == Some("codex") { "codex" } else { "claude" };
        hook::run(agent);
        return;
    }
    // `--connect claude|codex` / `--disconnect …`: the same as the widget's button, for scripts.
    // Exit code 0 on success; the reason for a failure goes to app.log.
    for (flag, connect) in [("--connect", true), ("--disconnect", false)] {
        if let Some(i) = args.iter().position(|a| a == flag) {
            let t = installer::target(args.get(i + 1).map(String::as_str).unwrap_or("claude"));
            let result = if connect { installer::install(&t) } else { installer::uninstall(&t) };
            if let Err(e) = &result {
                store::log(&format!("{flag} {}: {e}", t.agent));
            }
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }
    }
    app::run();
}
