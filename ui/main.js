// Renders one row per Claude Code session. All state comes from the Rust side
// as the full list on every change ("sessions-updated"); this file only keeps
// the DOM in step with it. Looks live in style.css.

const THEMES = ["classic", "glass", "signal", "neon", "pills", "terminal"]; // keep in step with src-tauri/src/tray.rs
const SLOTS = ["error", "needs_you", "working", "done"];
const STATE_TEXT = { working: "Working", needs_you: "Needs you", done: "Done", error: "Error" };

const tauri = window.__TAURI__;
const widget = document.getElementById("widget");
const panel = document.getElementById("panel");
const limitsEl = document.getElementById("limits");
const rows = new Map(); // session_id -> <li>
let sessions = [];
let limits = null; // { five_hour, seven_day }, each { used_percentage, resets_at (seconds) } or null

function setTheme(name) {
  if (!THEMES.includes(name)) return;
  document.documentElement.dataset.theme = name;
  try { localStorage.setItem("theme", name); } catch {}
  fitWindow();
}

function ago(ms) {
  const min = Math.floor(Math.max(0, Date.now() - ms) / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  return `${h} h ${min % 60} min ago`;
}

// "3 min", "1 h 5 min", "2 d 3 h".
function span(ms) {
  const min = Math.floor(Math.max(0, ms) / 60000);
  if (min < 60) return `${min} min`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} h ${min % 60} min`;
  return `${Math.floor(h / 24)} d ${h % 24} h`;
}

// Compact elapsed time for the row: nothing under a minute, then 12m, 1h 5m.
function since(ms) {
  const min = Math.floor(Math.max(0, Date.now() - ms) / 60000);
  if (min < 1) return "";
  if (min < 60) return `${min}m`;
  return `${Math.floor(min / 60)}h ${min % 60}m`;
}

function tooltip(s) {
  const lines = [s.cwd || s.session_id];
  if (s.last_tool) lines.push(`Tool: ${s.last_tool}`);
  const verb = { needs_you: "Waiting", working: "Working" }[s.state];
  if (verb) {
    const age = Date.now() - s.state_since;
    lines.push(age < 60000 ? `${verb} for less than a minute` : `${verb} for ${span(age)}`);
  }
  lines.push(`${STATE_TEXT[s.state]}${s.stale ? " (no activity)" : ""} · ${ago(s.last_event_at)}`);
  return lines.join("\n");
}

function createRow() {
  const li = document.createElement("li");
  li.className = "row";
  const lamp = document.createElement("span");
  lamp.className = "lamp";
  for (const slot of SLOTS) {
    const well = document.createElement("i");
    well.className = "well";
    well.dataset.slot = slot;
    lamp.append(well);
  }
  const dot = document.createElement("b");
  dot.className = "dot";
  lamp.append(dot);
  const label = document.createElement("span");
  label.className = "label";
  const timer = document.createElement("span");
  timer.className = "since";
  li.append(lamp, label, timer);
  return li;
}

// Rows are reused, never rebuilt, so the dot can animate from its old
// position and colour to the new one.
function render(list) {
  sessions = list;
  const seen = new Set();
  list.forEach((s, i) => {
    seen.add(s.session_id);
    let li = rows.get(s.session_id);
    if (!li) {
      li = createRow();
      rows.set(s.session_id, li);
    }
    if (li.dataset.state !== s.state) {
      if (li.dataset.state) {
        li.classList.remove("changed");
        void li.offsetWidth; // restart the move animation
        li.classList.add("changed");
      }
      li.dataset.state = s.state;
    }
    li.dataset.stale = String(s.stale);
    li.querySelector(".label").textContent = s.label;
    li.querySelector(".since").textContent = s.state === "needs_you" ? since(s.state_since) : "";
    li.title = tooltip(s);
    if (panel.children[i] !== li) panel.insertBefore(li, panel.children[i] || null);
  });
  for (const [id, li] of rows) {
    if (!seen.has(id)) {
      li.remove();
      rows.delete(id);
    }
  }
  fitWindow();
}

const WINDOW_NAMES = { five_hour: "5-hour limit", seven_day: "Weekly limit" };
const WINDOW_SHORT = { five_hour: "5h", seven_day: "wk" };

function clock(d, withDay) {
  const t = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  return withDay ? `${d.toLocaleDateString([], { weekday: "short" })} ${t}` : t;
}

// resets_at is in seconds, everything else in this file is milliseconds.
function renderLimits() {
  let any = false;
  for (const el of limitsEl.children) {
    const key = el.dataset.win;
    const w = limits && limits[key];
    el.hidden = !w;
    if (!w) continue;
    any = true;
    const resetMs = w.resets_at * 1000;
    const expired = resetMs <= Date.now();
    const pct = expired ? 0 : Math.round(Math.min(100, Math.max(0, w.used_percentage)));
    el.dataset.level = pct >= 90 ? "crit" : pct >= 70 ? "warn" : "ok";
    el.querySelector(".lim-text").textContent = `${WINDOW_SHORT[key]} ${pct}%`;
    el.querySelector(".lim-bar i").style.width = `${pct}%`;
    el.title = expired
      ? `${WINDOW_NAMES[key]}: window has reset`
      : `${WINDOW_NAMES[key]}: ${pct}% used, resets in ${span(resetMs - Date.now())} (${clock(new Date(resetMs), resetMs - Date.now() > 86400000)})`;
  }
  const wasHidden = limitsEl.hidden;
  limitsEl.hidden = !any;
  if (wasHidden !== limitsEl.hidden) fitWindow();
}

function setLimits(l) {
  limits = l;
  renderLimits();
}

// The window is exactly as tall as the widget (plus the shadow margin).
function fitWindow() {
  if (!tauri) return;
  const margin = parseFloat(getComputedStyle(widget).marginTop) || 0;
  const { LogicalSize } = tauri.dpi ?? tauri.window;
  tauri.window
    .getCurrentWindow()
    .setSize(new LogicalSize(widget.offsetWidth + margin * 2, Math.max(1, widget.offsetHeight + margin * 2)))
    .catch(() => {});
}

// One timer for everything that depends on the clock: row timers, tooltips
// and limit windows that have just reset.
setInterval(() => {
  for (const s of sessions) {
    const li = rows.get(s.session_id);
    li.title = tooltip(s);
    li.querySelector(".since").textContent = s.state === "needs_you" ? since(s.state_since) : "";
  }
  renderLimits();
}, 15000);

try { setTheme(localStorage.getItem("theme") || "classic"); } catch {}

if (tauri) {
  // data-tauri-drag-region only fires for clicks on the element itself, not
  // its children, so drag from anywhere on the widget by hand instead.
  document.addEventListener("mousedown", (e) => {
    if (e.button === 0) tauri.window.getCurrentWindow().startDragging();
  });
  document.addEventListener("contextmenu", (e) => e.preventDefault());
  tauri.event.listen("sessions-updated", (e) => render(e.payload));
  tauri.event.listen("theme-changed", (e) => setTheme(e.payload));
  tauri.event.listen("limits-updated", (e) => setLimits(e.payload));
  tauri.core.invoke("get_sessions").then(render);
  tauri.core.invoke("get_limits").then(setLimits);
} else {
  startDemo();
}

// Opened in a plain browser: fake sessions cycle through the states so the
// designs can be previewed and worked on without building the app.
function startDemo() {
  document.documentElement.classList.add("demo");
  const now = Date.now();
  const mk = (id, label, state, extra = {}) => ({
    session_id: id, label, cwd: `C:\\Users\\you\\code\\${label}`, state,
    state_since: now, last_event_at: now, last_tool: null, stale: false, ...extra,
  });
  let list = [
    mk("a", "chiefosei.dev", "working", { last_tool: "Bash" }),
    mk("b", "traffic-light", "needs_you", { last_tool: "Edit", state_since: now - 4 * 60000 }),
    mk("c", "api-server", "done"),
    mk("d", "old-experiment", "error", { stale: true, last_event_at: now - 45 * 60000 }),
  ];
  render(list);
  setLimits({
    five_hour: { used_percentage: 23.4, resets_at: Math.floor(now / 1000) + 130 * 60 },
    seven_day: { used_percentage: 78, resets_at: Math.floor(now / 1000) + 3 * 86400 },
  });
  const wanted = new URLSearchParams(location.search).get("theme");
  if (wanted) setTheme(wanted);

  const bar = document.createElement("div");
  bar.id = "demo-bar";
  for (const t of THEMES) {
    const b = document.createElement("button");
    b.textContent = t;
    b.onclick = () => setTheme(t);
    bar.append(b);
  }
  document.body.append(bar);

  if (new URLSearchParams(location.search).has("static")) return;
  const cycle = ["working", "needs_you", "working", "done", "working", "error"];
  let tick = 0;
  setInterval(() => {
    const i = tick % 3;
    const step = Math.floor(tick / 3) + i * 2;
    list = list.map((s, n) => n === i
      ? { ...s, state: cycle[step % cycle.length], state_since: Date.now(), last_event_at: Date.now() }
      : s);
    render(list);
    tick++;
  }, 1200);
}
