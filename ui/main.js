// Renders one row per Claude Code session. All state comes from the Rust side
// as the full list on every change ("sessions-updated"); this file only keeps
// the DOM in step with it. Looks live in style.css.

const THEMES = ["classic", "glass"]; // keep in step with src-tauri/src/tray.rs
const SLOTS = ["error", "needs_you", "working", "done"];
const STATE_TEXT = { working: "Working", needs_you: "Needs you", done: "Done", error: "Error" };

const tauri = window.__TAURI__;
const panel = document.getElementById("panel");
const rows = new Map(); // session_id -> <li>
let sessions = [];

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

function tooltip(s) {
  const lines = [s.cwd || s.session_id];
  if (s.last_tool) lines.push(`Tool: ${s.last_tool}`);
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
  li.append(lamp, label);
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
    li.lastChild.textContent = s.label;
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

// The window is exactly as tall as the list (plus the shadow margin).
function fitWindow() {
  if (!tauri) return;
  const margin = parseFloat(getComputedStyle(panel).marginTop) || 0;
  const { LogicalSize } = tauri.dpi ?? tauri.window;
  tauri.window
    .getCurrentWindow()
    .setSize(new LogicalSize(panel.offsetWidth + margin * 2, Math.max(1, panel.offsetHeight + margin * 2)))
    .catch(() => {});
}

setInterval(() => {
  for (const s of sessions) rows.get(s.session_id).title = tooltip(s);
}, 30000);

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
  tauri.core.invoke("get_sessions").then(render);
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
    last_event_at: now, last_tool: null, stale: false, ...extra,
  });
  let list = [
    mk("a", "chiefosei.dev", "working", { last_tool: "Bash" }),
    mk("b", "traffic-light", "needs_you", { last_tool: "Edit" }),
    mk("c", "api-server", "done"),
    mk("d", "old-experiment", "error", { stale: true, last_event_at: now - 45 * 60000 }),
  ];
  render(list);

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
      ? { ...s, state: cycle[step % cycle.length], last_event_at: Date.now() }
      : s);
    render(list);
    tick++;
  }, 1200);
}
