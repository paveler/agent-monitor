// The widget: draws the state the Rust side sends and reports clicks back.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// English, or Russian when the app says Windows is in Russian.
const L = (en, ru) => (state && state.lang === 'ru' ? ru : en);
const LABELS = {
  get approval() { return L('Needs approval', 'Нужно подтверждение'); },
  get question() { return L('Question for you', 'Вопрос к вам'); },
  get plan() { return L('Plan to review', 'План на ревью'); },
  get yourTurn() { return L('Your turn', 'Ваш ход'); },
};
const U = {
  get d() { return L('d', 'д'); },
  get h() { return L('h', 'ч'); },
  get m() { return L('m', 'м'); },
};
const AGENTS = { claude: 'Claude', codex: 'Codex' };

let state = null;
let pendingRefresh = false;

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
};
const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

function remaining(resetsAt, now) {
  if (resetsAt == null) return '—';
  const s = Math.floor(resetsAt - now);
  if (s <= 0) return L('reset', 'сброс');
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}${U.d} ${h}${U.h}`;
  if (h > 0) return `${h}${U.h} ${m}${U.m}`;
  return `${m}${U.m}`;
}

function ago(t, now) {
  const s = Math.max(0, Math.floor(now - t));
  if (s < 60) return L('now', 'сейчас');
  if (s < 3600) return `${Math.floor(s / 60)}${U.m}`;
  if (s < 86400) return `${Math.floor(s / 3600)}${U.h}`;
  return `${Math.floor(s / 86400)}${U.d}`;
}

// If the window has already reset, usage is 0%.
function effective(limit, now) {
  if (limit.percent == null) return null;
  if (limit.resetsAt != null && limit.resetsAt < now) return 0;
  return limit.percent;
}

function level(p, tint) {
  if (p == null) return css('--track');
  if (p >= 90) return css('--danger');
  if (p >= 75) return css('--warn');
  return tint;
}

function ring(limit, label, tint, now) {
  const p = effective(limit, now);
  const r = 24, c = 2 * Math.PI * r;
  const wrap = el('div', 'ring');
  const gauge = el('div', 'gauge');
  gauge.innerHTML =
    `<svg viewBox="0 0 58 58"><circle class="track" cx="29" cy="29" r="${r}"/>` +
    `<circle class="value" cx="29" cy="29" r="${r}" stroke="${level(p, tint)}" ` +
    `stroke-dasharray="${c}" stroke-dashoffset="${c * (1 - Math.min(100, p ?? 0) / 100)}"/></svg>`;
  gauge.append(el('div', 'pct', p == null ? '—' : `${Math.round(p)}%`));
  const text = el('div', 'label', label);
  text.append(el('span', 'left', remaining(limit.resetsAt, now)));
  wrap.append(gauge, text);
  return wrap;
}

const WINDOWS = { fiveHour: 5 * 3600, week: 7 * 86400 };

function duration(sec) {
  const s = Math.max(0, Math.floor(sec));
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}${U.d} ${h}${U.h}`;
  if (h > 0) return `${h}${U.h} ${m}${U.m}`;
  return `${Math.max(1, m)}${U.m}`;
}

// Where usage is heading: compares what is spent with how much of the window has passed.
function forecast(p, limit, length, now) {
  if (p == null || limit.resetsAt == null) return null;
  const left = limit.resetsAt - now;
  if (left <= 0) return { text: L('The window has reset', 'Окно обновилось'), elapsed: 0 };
  const elapsed = Math.min(1, Math.max(0, (length - left) / length));
  if (p >= 100) return { text: `${L('Limit reached, resets in', 'Лимит исчерпан, сброс через')} ${duration(left)}`, elapsed, bad: true };
  if (elapsed < 0.04 || p < 1) return { text: L('The window has just started', 'Окно только началось'), elapsed };
  const rate = p / (elapsed * length); // % per second
  const projected = p + rate * left;
  if (projected >= 100) {
    const runsOut = (100 - p) / rate;
    return {
      text: L(
        `At this pace it lasts ${duration(runsOut)} — runs out ${duration(left - runsOut)} before the reset`,
        `В таком темпе хватит на ${duration(runsOut)} — кончится за ${duration(left - runsOut)} до сброса`
      ),
      elapsed,
      bad: true,
    };
  }
  return { text: L(`At this pace about ${Math.round(projected)}% by the reset`, `В таком темпе к сбросу будет около ${Math.round(projected)}%`), elapsed };
}

function meter(limit, key, label, tint, now) {
  const p = effective(limit, now);
  const f = forecast(p, limit, WINDOWS[key], now);
  const box = el('div', 'meter');
  const top = el('div', 'mtop');
  const num = el('span', 'num', p == null ? '—' : `${Math.round(p)}%`);
  if (p != null && p >= 75) num.style.color = level(p, tint);
  const left = remaining(limit.resetsAt, now);
  const reset = limit.resetsAt == null ? '' : left === L('reset', 'сброс') ? left : `${L('resets in', 'сброс через')} ${left}`;
  top.append(num, el('span', 'mlabel', label), el('span', 'mreset', reset));
  const bar = el('div', 'bar');
  const fill = el('div', 'fill');
  fill.style.width = `${Math.min(100, p ?? 0)}%`;
  fill.style.background = level(p, tint);
  bar.append(fill);
  if (f && f.elapsed > 0) {
    const pace = el('div', 'pace');
    pace.style.left = `${f.elapsed * 100}%`;
    pace.title = L(`${Math.round(f.elapsed * 100)}% of the window has passed`, `Прошло ${Math.round(f.elapsed * 100)}% времени окна`);
    bar.append(pace);
  }
  box.append(top, bar, el('div', 'hint' + (f && f.bad ? ' bad' : ''), f ? f.text : ''));
  return box;
}

// The 5-hour limit over the last 24 hours.
function chart(points, tint, now) {
  const box = el('div', 'chart');
  const W = 600, H = 64, from = now - 86400;
  if (points.length < 2) {
    box.append(el('div', 'chart-empty', L('The usage chart appears once samples build up (one every 5 minutes)', 'График расхода появится, когда накопятся замеры (раз в 5 минут)')));
    return box;
  }
  const xy = points.map(([t, p]) => [((t - from) / 86400) * W, H - 2 - (Math.min(100, p) / 100) * (H - 6)]);
  const line = xy.map(([x, y], i) => `${i ? 'L' : 'M'}${x.toFixed(1)} ${y.toFixed(1)}`).join(' ');
  const area = `${line} L${xy[xy.length - 1][0].toFixed(1)} ${H} L${xy[0][0].toFixed(1)} ${H} Z`;
  const id = `g${Math.random().toString(36).slice(2, 8)}`;
  box.innerHTML =
    `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">` +
    `<defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1">` +
    `<stop offset="0" stop-color="${tint}" stop-opacity="0.45"/>` +
    `<stop offset="1" stop-color="${tint}" stop-opacity="0"/></linearGradient></defs>` +
    `<line class="grid" x1="0" x2="${W}" y1="${H / 2}" y2="${H / 2}"/>` +
    `<path d="${area}" fill="url(#${id})"/><path class="line" d="${line}" stroke="${tint}"/></svg>`;
  const axis = el('div', 'axis');
  const peak = Math.round(Math.max(...points.map((p) => p[1])));
  axis.append(el('span', '', L('24 hours ago', '24 часа назад')),
    el('span', '', L(`5-hour limit · peak ${peak}%`, `5-часовой лимит · пик ${peak}%`)),
    el('span', '', L('now', 'сейчас')));
  box.append(axis);
  return box;
}

// The small widget's rows: "5h · 2h 14m ........ 34%" over a thin bar.
function barRow(limit, short, tint, now) {
  const p = effective(limit, now);
  const row = el('div', 'brow');
  const top = el('div', 'btop');
  top.append(el('span', '', `${short} · ${remaining(limit.resetsAt, now)}`), el('span', 'bp', p == null ? '—' : `${Math.round(p)}%`));
  const bar = el('div', 'bar thin');
  const fill = el('div', 'fill');
  fill.style.width = `${Math.min(100, p ?? 0)}%`;
  fill.style.background = level(p, tint);
  bar.append(fill);
  row.append(top, bar);
  return row;
}

function renderAgent(id, usage, now, view) {
  const tint = css(`--${id}`);
  const box = $(id);
  box.className = `agent v-${view}`;
  box.replaceChildren();
  const head = el('div', 'head');
  const dot = el('span', 'dot');
  dot.style.background = tint;
  head.append(dot, el('span', 'name', AGENTS[id]));
  if (usage.staleReason && usage.fetchedAt) {
    const at = new Date(usage.fetchedAt * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    const note = el('span', 'note', `${L('as of', 'на')} ${at}`);
    note.title = usage.staleReason;
    head.append(note);
  }
  box.append(head);
  if (usage.error) {
    box.append(el('div', 'error', usage.error));
    return;
  }
  if (view === 'bars') {
    box.append(barRow(usage.fiveHour, L('5h', '5ч'), tint, now), barRow(usage.week, L('week', 'неделя'), tint, now));
    return;
  }
  if (view === 'big') {
    box.append(
      meter(usage.fiveHour, 'fiveHour', L('5 hours', '5 часов'), tint, now),
      meter(usage.week, 'week', L('week', 'неделя'), tint, now),
      chart((state.history && state.history[id]) || [], tint, now)
    );
    return;
  }
  const rings = el('div', 'rings');
  rings.append(ring(usage.fiveHour, L('5 hours', '5 часов'), tint, now), ring(usage.week, L('week', 'неделя'), tint, now));
  box.append(rings);
}

function renderWaiting(now) {
  const box = $('waiting');
  box.replaceChildren();
  const list = state.waiting;
  const head = el('div', 'whead');
  head.append(el('span', 't', list.length ? L('Waiting on you', 'Ждут вас') : L('No sessions need your attention', 'Нет сессий, требующих внимания')));
  if (list.length) {
    head.append(el('span', 'count' + (list.some((s) => s.blocking) ? ' blocked' : ''), String(list.length)));
  }
  box.append(head);

  const view = ['compact', 'cards'].includes(state.settings.view) ? state.settings.view : 'list';
  const max = state.settings.maxRows || Infinity;
  const holder = el('div', view === 'cards' ? 'cards' : 'rows');
  for (const s of list.slice(0, max)) {
    const row = el('div', (view === 'cards' ? 'row card' : 'row') + (s.blocking ? ' blocking' : ''));
    row.title = [s.cwd, s.detail].filter(Boolean).join(' · ');
    const dot = el('span', 'dot');
    dot.style.background = css(`--${s.agent}`);
    const where = s.title === s.project ? ago(s.since, now) : `${s.project} · ${ago(s.since, now)}`;
    if (view === 'cards') {
      // A card per session: where it is, what it is, what it wants.
      const top = el('div', 'top');
      top.append(dot, el('span', 'meta', where));
      row.append(top, el('div', 'title', s.title), el('div', 'state', LABELS[s.state]));
      if (s.detail) row.append(el('div', 'detail', s.detail));
    } else if (view === 'compact') {
      row.append(dot, el('span', 'title', s.title), el('span', 'meta state', `${LABELS[s.state]} · ${ago(s.since, now)}`));
    } else {
      const sub = LABELS[s.state] + (s.detail ? ` · ${s.detail}` : '');
      row.append(dot, el('span', 'title', s.title), el('span', 'meta', where), el('span', 'sub', sub));
    }
    row.addEventListener('click', () => invoke('open_session', { id: s.id }));
    holder.append(row);
  }
  if (list.length) box.append(holder);
  if (list.length > max) box.append(el('div', 'more', `+${list.length - max} ${L('more', 'ещё')}`));

  const foot = el('div', 'foot');
  const action = (text, fn) => {
    const b = el('button', 'link', text);
    b.addEventListener('click', fn);
    foot.append(b);
  };
  if (list.some((s) => s.state === 'yourTurn')) action(L('Clear Finished', 'Убрать завершённые'), () => invoke('clear_finished'));
  for (const [agent, name] of [['claude', 'Claude Code'], ['codex', 'Codex']]) {
    if (shows(agent) && !state.hooks[agent]) action(`${L('Connect', 'Подключить')} ${name}`, () => connect(agent, name));
  }
  if (foot.children.length) box.append(foot);
}

async function connect(agent, name) {
  const file = agent === 'codex' ? '~/.codex/hooks.json' : '~/.claude/settings.json';
  const ok = confirm(L(
    `Connect ${name}?\n\nAgent Monitor will add hooks to ${file} so it can tell when the agent is waiting for you. ` +
      'Existing hooks are kept, and a backup is saved next to the file (.agentmonitor-backup).',
    `Подключить ${name}?\n\nAgent Monitor добавит свои хуки в ${file}, чтобы видеть, когда агент ждёт вас. ` +
      'Ваши хуки останутся, рядом сохранится бэкап (.agentmonitor-backup).'
  ));
  if (!ok) return;
  try {
    await invoke('toggle_hooks', { agent });
  } catch (e) {
    showNotice(String(e));
  }
}

function renderFresh(now) {
  const u = state.snapshot.updated;
  const s = u == null ? null : Math.floor(now - u);
  $('fresh-text').textContent = s == null ? '' : s < 60 ? L('now', 'сейчас') : L(`${ago(u, now)} ago`, `${ago(u, now)} назад`);
  $('fresh').title = L('Refresh', 'Обновить');
  $('fresh').classList.toggle('spin', state.loading || pendingRefresh);
}

const shows = (agent) => !['claude', 'codex'].includes(state.settings.agents) || state.settings.agents === agent;

function render() {
  if (!state) return;
  const now = Date.now() / 1000;
  const st = state.settings;
  const both = shows('claude') && shows('codex');
  // Limits: rings by default, the big view when a single agent has the whole width.
  const view = ['rings', 'bars', 'big'].includes(st.limitsView) ? st.limitsView : both ? 'rings' : 'big';
  if (st.theme === 'dark' || st.theme === 'light') document.documentElement.dataset.theme = st.theme;
  else delete document.documentElement.dataset.theme;
  $('card').classList.toggle('wide', st.size === 'wide');
  $('card').classList.toggle('narrow', st.size === 'narrow');
  $('usage').hidden = $('fresh').hidden = !st.showLimits;
  // Two agents sit side by side unless there is no room for that.
  $('usage').classList.toggle('stack', both && (view === 'big' || st.size === 'narrow'));
  $('claude').hidden = !shows('claude');
  $('codex').hidden = !shows('codex');
  document.querySelector('.divider').hidden = !both;
  $('waiting').hidden = !st.showTasks;
  $('waiting').classList.toggle('alone', !st.showLimits);
  renderAgent('claude', state.snapshot.claude, now, view);
  renderAgent('codex', state.snapshot.codex, now, view);
  renderWaiting(now);
  renderFresh(now);
  fit();
}

let lastSize = '';
function fit() {
  const h = Math.ceil($('card').getBoundingClientRect().height);
  const size = `${h}:${state && state.settings.size}`;
  if (size !== lastSize) {
    lastSize = size;
    invoke('set_height', { height: h });
  }
}

let noticeTimer;
function showNotice(text) {
  const n = $('notice');
  n.textContent = text;
  n.hidden = false;
  fit();
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { n.hidden = true; fit(); }, 9000);
}

$('fresh').addEventListener('click', () => {
  pendingRefresh = true;
  render();
  invoke('refresh');
  setTimeout(() => { pendingRefresh = false; render(); }, 1500);
});

// Drag the widget by any empty spot.
document.addEventListener('mousedown', (e) => {
  if (e.button === 0 && !e.target.closest('button, .row')) invoke('start_drag');
});
document.addEventListener('contextmenu', (e) => e.preventDefault());

listen('state', (e) => { state = e.payload; render(); });
listen('notice', (e) => showNotice(e.payload));
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', render);
setInterval(render, 20000); // countdowns and ages tick
invoke('get_state').then((s) => { state = s; render(); });
