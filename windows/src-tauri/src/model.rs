// Shared model. Times are seconds since the Unix epoch.
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Limit {
    pub percent: Option<f64>, // 0...100
    pub resets_at: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentUsage {
    pub five_hour: Limit,
    pub week: Limit,
    /// Shown instead of the rings: there's no data at all.
    pub error: Option<String>,
    /// When these numbers were current (API fetch or Codex log time).
    pub fetched_at: Option<f64>,
    /// Not live: the last fetch failed, or the numbers came from Codex logs.
    pub stale_reason: Option<String>,
}

impl AgentUsage {
    pub fn err(reason: impl Into<String>) -> Self {
        AgentUsage { error: Some(reason.into()), ..Default::default() }
    }

    pub fn has_data(&self) -> bool {
        self.five_hour.percent.is_some() || self.week.percent.is_some()
    }

    /// Keep the last good numbers when a fetch fails; only show an error if there's nothing to show.
    pub fn failed(&self, reason: &str) -> AgentUsage {
        if !self.has_data() || self.error.is_some() {
            return AgentUsage::err(reason);
        }
        let mut u = self.clone();
        u.stale_reason = Some(reason.to_string());
        u
    }
}

/// Usage samples for the chart, oldest first: [time, 5-hour %, week %].
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct History {
    pub claude: Vec<[f64; 3]>,
    pub codex: Vec<[f64; 3]>,
}

impl History {
    const KEEP: f64 = 7.0 * 86400.0;

    /// Adds a sample when these numbers are newer than the last one kept.
    pub fn record(&mut self, agent: &str, u: &AgentUsage, now: f64) {
        let list = if agent == "codex" { &mut self.codex } else { &mut self.claude };
        list.retain(|p| p[0] > now - Self::KEEP);
        let Some(at) = u.fetched_at.filter(|_| u.error.is_none() && u.has_data()) else { return };
        if list.last().map(|p| at <= p[0]).unwrap_or(false) {
            return;
        }
        // A window that has already reset counts as 0%.
        let pct = |l: &Limit| if l.resets_at.map(|r| r < at).unwrap_or(false) { 0.0 } else { l.percent.unwrap_or(0.0) };
        list.push([at, pct(&u.five_hour), pct(&u.week)]);
    }

    /// The last 24 hours, which is what the widget draws.
    pub fn recent(&self, now: f64) -> History {
        let cut = |l: &Vec<[f64; 3]>| l.iter().filter(|p| p[0] > now - 86400.0).copied().collect();
        History { claude: cut(&self.claude), codex: cut(&self.codex) }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    pub claude: AgentUsage,
    pub codex: AgentUsage,
    pub updated: Option<f64>,
    /// Last successful Claude fetch: a failure never wipes the numbers.
    pub claude_last_good: Option<AgentUsage>,
    /// Next allowed Claude fetch, survives relaunches (429 backoff).
    pub claude_retry_at: Option<f64>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            claude: AgentUsage::err(crate::i18n::tr("No data yet", "Пока нет данных")),
            codex: AgentUsage::err(crate::i18n::tr("No data yet", "Пока нет данных")),
            updated: None,
            claude_last_good: None,
            claude_retry_at: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Idle,     // open, nothing happening (fresh session, interrupted)
    Working,  // agent is busy
    Approval, // permission dialog is up
    Question, // agent asked the user something
    Plan,     // plan is ready for review
    YourTurn, // turn finished, waiting for the next prompt
}

impl SessionState {
    pub fn is_blocking(self) -> bool {
        matches!(self, SessionState::Approval | SessionState::Question | SessionState::Plan)
    }
    pub fn rank(self) -> u8 {
        if self.is_blocking() { 0 } else { 1 }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub agent: String, // "claude" | "codex"
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub title: Option<String>,
    pub state: SessionState,
    #[serde(default)]
    pub detail: Option<String>,
    /// When the current state began.
    pub since: f64,
    pub updated: f64,
    /// Agent process, for liveness checks and for finding its window.
    #[serde(default)]
    pub pid: Option<u32>,
    /// The user opened this session from Agent Monitor; hides a finished turn until the next one.
    #[serde(default)]
    pub seen_at: Option<f64>,
    /// Claude desktop app's own session id ("local_…"), for deep links.
    #[serde(default)]
    pub host_session_id: Option<String>,
}

impl AgentSession {
    pub fn id(&self) -> String {
        format!("{}-{}", self.agent, self.session_id)
    }
    /// Seen after the current state began (a new finished turn resets this).
    pub fn is_seen(&self) -> bool {
        self.seen_at.map(|s| s >= self.since).unwrap_or(false)
    }
    pub fn project(&self) -> String {
        let name = Path::new(&self.cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.is_empty() {
            if self.agent == "claude" { "Claude Code".into() } else { "Codex".into() }
        } else {
            name
        }
    }
}

/// What the widget draws for one session.
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub project: String,
    pub state: SessionState,
    pub blocking: bool,
    pub detail: Option<String>,
    pub since: f64,
    pub cwd: String,
}

impl From<&AgentSession> for SessionView {
    fn from(s: &AgentSession) -> Self {
        let project = s.project();
        SessionView {
            id: s.id(),
            agent: s.agent.clone(),
            title: s.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| project.clone()),
            project,
            state: s.state,
            blocking: s.state.is_blocking(),
            detail: s.detail.clone(),
            since: s.since,
            cwd: s.cwd.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Float above all windows instead of sitting on the desktop.
    pub on_top: bool,
    pub hidden: bool,
    /// Which agents to show: "both" | "claude" | "codex".
    pub agents: String,
    /// How many waiting sessions to list; 0 = all of them.
    pub max_rows: u32,
    /// How waiting sessions are laid out: "list" | "compact" | "cards".
    pub view: String,
    /// Widget width: "narrow" | "normal" | "wide".
    pub size: String,
    /// How limits are drawn: "auto" (rings, or the big view for a single agent) | "rings" | "bars" | "big".
    pub limits_view: String,
    /// "auto" (follows Windows) | "dark" | "light".
    pub theme: String,
    pub show_limits: bool,
    pub show_tasks: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            x: None,
            y: None,
            on_top: false,
            hidden: false,
            agents: "both".into(),
            max_rows: 6,
            view: "list".into(),
            size: "normal".into(),
            limits_view: "auto".into(),
            theme: "auto".into(),
            show_limits: true,
            show_tasks: true,
        }
    }
}

impl Settings {
    pub fn shows(&self, agent: &str) -> bool {
        self.agents != "claude" && self.agents != "codex" || self.agents == agent
    }
    /// Widget width in logical pixels.
    pub fn width(&self) -> f64 {
        match self.size.as_str() {
            "narrow" => 240.0,
            "wide" => 560.0,
            _ => 380.0,
        }
    }
}
