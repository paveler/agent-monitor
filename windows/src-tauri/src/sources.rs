// Usage data sources: the same endpoints the official CLIs use, with the logins they already have.
use crate::hook::tail;
use crate::i18n::tr;
use crate::model::{AgentUsage, Limit};
use crate::store;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub enum Outcome {
    Ok(AgentUsage),
    /// `auth`: the user has to act (sign in, open Claude Code); old numbers would only hide that.
    Failed { reason: String, rate_limited: bool, retry_after: Option<f64>, auth: bool },
}

fn failed(reason: &str) -> Outcome {
    Outcome::Failed { reason: reason.to_string(), rate_limited: false, retry_after: None, auth: false }
}
fn auth(reason: &str) -> Outcome {
    Outcome::Failed { reason: reason.to_string(), rate_limited: false, retry_after: None, auth: true }
}

enum HttpErr {
    Status(u16, Option<f64>),
    Offline,
}

/// Strictly the system's own route: the proxy Windows is set to use (what a VPN client configures), or a
/// plain connection when there is none (a VPN that tunnels the whole machine). Never a second choice:
/// falling back to anything else would send the request around the VPN. Proxies named in the environment
/// or in Claude Code's settings are deliberately ignored.
fn routes() -> Vec<Option<String>> {
    vec![system_proxy()]
}

/// "host:port", or "http=host:port;https=host:port;socks=host:port".
fn system_proxy() -> Option<String> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    if key.get_value::<u32, _>("ProxyEnable").ok()? != 1 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let part = |name: &str| server.split(';').find_map(|p| p.trim().strip_prefix(name).map(str::to_string));
    if !server.contains('=') {
        return Some(format!("http://{}", server.trim()));
    }
    part("https=").or(part("http=")).map(|h| format!("http://{h}")).or(part("socks=").map(|h| format!("socks5://{h}")))
}

/// For the log: a proxy URL without any credentials in it.
fn route_name(route: &Option<String>) -> String {
    match route {
        None => "direct".into(),
        Some(p) => match (p.split_once("://"), p.rsplit_once('@')) {
            (Some((scheme, _)), Some((_, host))) => format!("{scheme}://{host}"),
            _ => p.clone(),
        },
    }
}

/// The route in use, to log it once when it changes.
static GOOD_ROUTE: Mutex<Option<Option<String>>> = Mutex::new(None);

fn send(timeout: u64, call: impl Fn(&ureq::Agent) -> Result<ureq::Response, ureq::Error>) -> Result<Value, HttpErr> {
    for route in routes() {
        let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(timeout));
        if let Some(p) = &route {
            match ureq::Proxy::new(p) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(e) => {
                    store::log(&format!("bad proxy {}: {e}", route_name(&route)));
                    continue;
                }
            }
        }
        let outcome = match call(&builder.build()) {
            Ok(resp) => resp.into_json::<Value>().map_err(|_| HttpErr::Offline),
            Err(ureq::Error::Status(code, resp)) => {
                Err(HttpErr::Status(code, resp.header("Retry-After").and_then(|v| v.trim().parse().ok())))
            }
            Err(e) => {
                store::log(&format!("request failed ({}): {e}", route_name(&route)));
                continue;
            }
        };
        if let Ok(mut g) = GOOD_ROUTE.lock() {
            if g.as_ref() != Some(&route) {
                store::log(&format!("requests go via {}", route_name(&route)));
            }
            *g = Some(route);
        }
        return outcome;
    }
    Err(HttpErr::Offline)
}

fn get_json(url: &str, headers: &[(&str, &str)]) -> Result<Value, HttpErr> {
    send(15, |agent| {
        let mut req = agent.get(url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        req.call()
    })
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn iso(v: &Value) -> Option<f64> {
    let d = chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok()?;
    Some(d.timestamp_millis() as f64 / 1000.0)
}

// MARK: - Claude

/// Claude Code keeps its OAuth credentials in ~/.claude/.credentials.json on Windows.
/// The CLI refreshes them only while it runs, so people who live in the Claude desktop app end up
/// with an expired token. When it has expired, we refresh it the same way the CLI does and write
/// it back, so the CLI keeps working with the rotated refresh token.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
static LAST_REFRESH: Mutex<f64> = Mutex::new(0.0);

enum Token {
    Ok(String),
    Missing,
    Expired,
}

fn credentials_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| store::home().join(".claude"))
        .join(".credentials.json")
}

fn read_credentials() -> Option<(Vec<u8>, Value)> {
    let raw = std::fs::read(credentials_path()).ok()?;
    let v = serde_json::from_slice(&raw).ok()?;
    Some((raw, v))
}

fn token() -> Token {
    let Some((_, creds)) = read_credentials() else { return Token::Missing };
    let oauth = &creds["claudeAiOauth"];
    let Some(tok) = oauth["accessToken"].as_str() else { return Token::Missing };
    match num(&oauth["expiresAt"]) {
        Some(ms) if ms / 1000.0 < store::now() => refresh().unwrap_or(Token::Expired),
        _ => Token::Ok(tok.to_string()),
    }
}

/// Only after expiry (a running CLI refreshes a few minutes before), at most every 10 minutes.
fn refresh() -> Option<Token> {
    {
        let mut last = LAST_REFRESH.lock().ok()?;
        if store::now() - *last < 600.0 {
            return None;
        }
        *last = store::now();
    }
    // Re-read right before: the CLI may have just refreshed it.
    let (original, mut creds) = read_credentials()?;
    let oauth = creds["claudeAiOauth"].clone();
    let refresh_token = oauth["refreshToken"].as_str()?.to_string();
    if let (Some(ms), Some(tok)) = (num(&oauth["expiresAt"]), oauth["accessToken"].as_str()) {
        if ms / 1000.0 > store::now() {
            return Some(Token::Ok(tok.to_string()));
        }
    }

    let mut body = json!({ "grant_type": "refresh_token", "refresh_token": refresh_token, "client_id": CLIENT_ID });
    if let Some(scopes) = oauth["scopes"].as_array().filter(|s| !s.is_empty()) {
        let joined: Vec<&str> = scopes.iter().filter_map(Value::as_str).collect();
        body["scope"] = json!(joined.join(" "));
    }
    let j = send(30, |agent| agent.post(TOKEN_URL).send_json(body.clone())).ok()?;
    let access = j["access_token"].as_str()?.to_string();
    let expires_in = num(&j["expires_in"])?;

    creds["claudeAiOauth"]["accessToken"] = json!(access);
    if let Some(r) = j["refresh_token"].as_str() {
        creds["claudeAiOauth"]["refreshToken"] = json!(r);
    }
    creds["claudeAiOauth"]["expiresAt"] = json!(((store::now() + expires_in) * 1000.0) as i64);

    // Write the whole file back and check it reads back intact; if not, put the original back.
    let path = credentials_path();
    let saved = serde_json::to_vec(&creds).ok().map(|data| store::write_atomic(&path, &data).is_ok()).unwrap_or(false);
    if !saved || read_credentials().map(|(_, v)| v != creds).unwrap_or(true) {
        let _ = store::write_atomic(&path, &original);
    }
    Some(Token::Ok(access))
}

pub fn claude_fetch() -> Outcome {
    let tok = match token() {
        Token::Ok(t) => t,
        Token::Missing => return auth(tr("Not signed in to Claude Code", "Нет входа в Claude Code")),
        Token::Expired => return auth(tr("Token expired: run claude", "Токен истёк: запустите claude")),
    };
    let bearer = format!("Bearer {tok}");
    let headers = [("Authorization", bearer.as_str()), ("anthropic-beta", "oauth-2025-04-20")];
    match get_json("https://api.anthropic.com/api/oauth/usage", &headers) {
        Ok(j) => {
            let lim = |k: &str| Limit { percent: num(&j[k]["utilization"]), resets_at: iso(&j[k]["resets_at"]) };
            Outcome::Ok(AgentUsage {
                five_hour: lim("five_hour"),
                week: lim("seven_day"),
                fetched_at: Some(store::now()),
                ..Default::default()
            })
        }
        Err(HttpErr::Status(401, _)) => auth(tr("Token expired: run claude", "Токен истёк: запустите claude")),
        Err(HttpErr::Status(429, retry)) => {
            Outcome::Failed { reason: tr("Rate limited", "Лимит запросов").into(), rate_limited: true, retry_after: retry, auth: false }
        }
        Err(HttpErr::Status(code, _)) => failed(&format!("{} {code}", tr("Server error", "Ошибка сервера"))),
        Err(HttpErr::Offline) => failed(tr("Offline", "Нет сети")),
    }
}

// MARK: - Codex

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| store::home().join(".codex"))
}

pub fn codex_signed_in() -> bool {
    codex_home().join("auth.json").exists()
}

/// Live data from the ChatGPT backend (the same one Codex CLI uses).
pub fn codex_api() -> Option<AgentUsage> {
    let obj: Value = serde_json::from_slice(&std::fs::read(codex_home().join("auth.json")).ok()?).ok()?;
    let tokens = &obj["tokens"];
    let bearer = format!("Bearer {}", tokens["access_token"].as_str()?);
    let mut headers = vec![("Authorization", bearer.as_str()), ("User-Agent", "codex_cli_rs")];
    if let Some(acc) = tokens["account_id"].as_str() {
        headers.push(("ChatGPT-Account-Id", acc));
    }
    let j = get_json("https://chatgpt.com/backend-api/wham/usage", &headers).ok()?;
    let rl = j.get("rate_limit")?.as_object()?;
    let lim = |k: &str| {
        let w = rl.get(k).cloned().unwrap_or(Value::Null);
        let reset = num(&w["reset_at"]).or_else(|| num(&w["reset_after_seconds"]).map(|a| store::now() + a));
        Limit { percent: num(&w["used_percent"]), resets_at: reset }
    };
    let u = AgentUsage {
        five_hour: lim("primary_window"),
        week: lim("secondary_window"),
        fetched_at: Some(store::now()),
        ..Default::default()
    };
    u.has_data().then_some(u)
}

/// Fallback: the latest rate_limits event from local Codex session logs.
pub fn codex_from_logs() -> Option<AgentUsage> {
    let mut files: Vec<(PathBuf, f64)> = vec![];
    collect_jsonl(&codex_home().join("sessions"), 0, &mut files);
    files.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (path, modified) in files.into_iter().take(5) {
        let Some(text) = tail(&path, 1_000_000) else { continue };
        for line in text.lines().rev().filter(|l| l.contains("\"rate_limits\"")) {
            let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };
            let Some(rl) = find_rate_limits(&obj) else { continue };
            let lim = |k: &str| Limit { percent: num(&rl[k]["used_percent"]), resets_at: num(&rl[k]["resets_at"]) };
            // "as of" is the event's own time: later unrelated writes move the file's mtime,
            // which would make old numbers look newer than the last API fetch.
            return Some(AgentUsage {
                five_hour: lim("primary"),
                week: lim("secondary"),
                fetched_at: Some(iso(&obj["timestamp"]).unwrap_or(modified)),
                stale_reason: Some(tr("from local logs", "из локальных логов").into()),
                ..Default::default()
            });
        }
    }
    None
}

fn collect_jsonl(dir: &std::path::Path, depth: u8, out: &mut Vec<(PathBuf, f64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            if depth < 6 {
                collect_jsonl(&p, depth + 1, out);
            }
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            let m = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
            out.push((p, m.unwrap_or(0.0)));
        }
    }
}

fn find_rate_limits(v: &Value) -> Option<&Value> {
    match v {
        Value::Object(d) => {
            if let Some(rl) = d.get("rate_limits").filter(|r| r.is_object()) {
                return Some(rl);
            }
            d.values().find_map(find_rate_limits)
        }
        Value::Array(a) => a.iter().find_map(find_rate_limits),
        _ => None,
    }
}
