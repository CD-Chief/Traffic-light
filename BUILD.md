# Build cards

## Rules for the builder

- Build only the card you were given. If the card looks wrong, make the smallest sensible choice and report it under "Questions/deviations". Don't redesign.
- Tests never touch the network. State-machine tests drive `Store::apply` directly; server tests use the in-process router (see `crates/core/src/server.rs` tests).
- Run checks with real exit codes: `cmd; echo EXIT=$?`. Never pipe a check through `tail`/`grep` in a way that hides failure.
- Git: small logical commits, short imperative subjects, no em dashes. No attribution of any kind (no Co-Authored-By, no "Generated with", no mention of Claude, Anthropic or AI). Don't change git config. Don't push.

## Conventions

- Rust 2021, `cargo fmt` clean, no new dependencies unless the card says so.
- Privacy rule: `HookEvent` deserializes only the fields the state machine needs. Every other part of a hook body is dropped by serde and never stored or logged. Any new field must be an opaque ID or a name, never user content, and the README "Privacy and security" list must name it.
- `Session` is serialized to the UI. Internal bookkeeping fields on it are `#[serde(skip)]`.
- Comments explain *why* (hook ordering quirks etc.), not what.

---

## Stage 1: Amber while Claude waits on you mid-turn

Today a session goes amber on `PermissionRequest`, and goes green again on the next `PreToolUse`/`PostToolUse` of **any** tool. Two bugs follow:

1. `AskUserQuestion` (Claude asks a multiple-choice question) and `ExitPlanMode` (Claude asks you to approve a plan) only go amber if a `PermissionRequest` fires for them. That depends on permission mode and allow rules, so sometimes it doesn't.
2. While one prompt is open, any other tool event in the same session (a background subagent, a parallel tool call, a second parallel permission prompt being approved) flips the light green although Claude is still waiting on the user.

Observed real event order for a question (trace taken 2026-11-17, Claude Code on Windows, same session_id throughout):

```
PreToolUse        tool=AskUserQuestion   (has tool_use_id)
PermissionRequest tool=AskUserQuestion   (NO tool_use_id, per hooks docs)
Notification      notification_type=permission_prompt   (~6 s later, only if unanswered)
PostToolUse       tool=AskUserQuestion   (has tool_use_id, fires when the user answers)
```

Per the hooks reference: `PreToolUse`, `PostToolUse`, `PostToolUseFailure` and `PermissionDenied` carry `tool_use_id`. `PermissionRequest` does **not**. `PostToolUseFailure` also fires for interrupts (`is_interrupt`).

### Card 1: Track open prompts per tool call

**What to build** (all in `crates/core/src/state.rs`, plus README):

1. `HookEvent` gains `pub tool_use_id: Option<String>`.
2. A constant `pub const WAITS_ON_USER: &[&str] = &["AskUserQuestion", "ExitPlanMode"];` These tools block until the user responds.
3. Per-session bookkeeping on `Session`, both `#[serde(skip)]`:
   - `in_flight: Vec<(String, String)>`: `(tool_use_id, tool_name)` for tools whose `PreToolUse` was seen but no result yet. Oldest first. Capped at 64 entries: drop the oldest when full, so a lost `PostToolUse` can't grow it forever.
   - `waiting: Vec<String>`: the `tool_use_id`s that are blocked on the user.
4. New rules in `Store::apply`. Only these events change; everything else behaves as today.
   - **`PreToolUse` with a `tool_use_id`**: push to `in_flight`. If `tool_name` is in `WAITS_ON_USER`, also push the id to `waiting`.
   - **`PermissionRequest`**: find the **most recent** `in_flight` entry with the same `tool_name` that isn't already in `waiting`, and add its id to `waiting`. If none matches (no ID, or the widget started mid-turn), the session still goes NeedsYou exactly as today, but nothing is added to `waiting`.
   - **`PostToolUse` / `PostToolUseFailure` / `PermissionDenied` with a `tool_use_id`**: remove that id from both `in_flight` and `waiting`.
   - **`UserPromptSubmit`, `Stop`, `StopFailure`**: clear both lists. These are turn boundaries. After Escape, no result may ever arrive for the open prompt, and the user can't type a new prompt while a dialog is open, so clearing is safe.
   - **Resulting state** for `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied` and `ElicitationResult`: `NeedsYou` if `waiting` is non-empty after the update, else `Working`. For `PermissionRequest`/`Elicitation`: `NeedsYou` as today. `Notification` rules: unchanged, but an ignored notification type must not clear amber while `waiting` is non-empty (it already keeps `from`, so this holds; add a test).
   - Events **without** `tool_use_id` (older Claude Code) fall back to today's behaviour exactly.
5. `last_tool` behaviour unchanged.
6. The "changed" detection in `apply` must stay correct. Bookkeeping-only changes (`in_flight` growing) are not visible changes and must not by themselves produce a `Transition`.
7. README:
   - Events table: `PreToolUse` row says AskUserQuestion / ExitPlanMode → Needs you. Add a line explaining that amber holds until the waiting tool's own result arrives, even when other tools run meanwhile.
   - "Where this differs" section: add a bullet on `PermissionRequest` having no `tool_use_id` and being matched to the latest open `PreToolUse` of the same tool.
   - Privacy list: add `tool_use_id`.

**Decisions already made**

- Amber for both `AskUserQuestion` and `ExitPlanMode` (owner chose this).
- Matching is by `tool_use_id`, never by timing. No timers.
- No hooks config change: `PreToolUse`/`PostToolUse` already have no matcher, so they fire for every tool.
- No UI change. The UI already renders `needs_you`.

**Done when** (new tests in the `state.rs` test module, all via `Store::apply`):

- `question_goes_amber_without_permission_request`: Pre(AskUserQuestion, id q1) → NeedsYou. Post(q1) → Working.
- `plan_approval_goes_amber`: same with ExitPlanMode.
- `other_tools_dont_clear_open_question`: Pre(AskUserQuestion q1) → Pre(Bash b1) → Post(b1) → still NeedsYou. Post(q1) → Working.
- `parallel_permission_prompts`: Pre(Bash b1), Pre(Bash b2), PermissionRequest(Bash) [matches b2], PermissionRequest(Bash) [matches b1] → NeedsYou. Post(b1) → still NeedsYou. Post(b2) → Working.
- `permission_denied_and_failure_clear_waiting`: one test each.
- `interrupt_then_new_prompt_clears`: Pre(AskUserQuestion q1), then UserPromptSubmit → Working. Also Stop → Done.
- `idle_notification_doesnt_clear_waiting`: Pre(AskUserQuestion q1), Notification(idle_prompt) → still NeedsYou.
- `in_flight_is_capped`: 100 Pres with no Posts leaves `in_flight.len() == 64` (test it from inside the module).
- `bookkeeping_alone_is_not_a_transition`: Pre(Bash b1) then Pre(Bash b2) in Working, same tool → second returns `None`.
- Existing tests pass unchanged, except `full_turn_lifecycle`, which may need ids added if it now relies on them. Prefer leaving it id-less, since that exercises the fallback.
- A server test in `server.rs` posts a real-shaped AskUserQuestion `PreToolUse` JSON body with `tool_use_id` and extra unknown fields (`tool_input` with a question text) and asserts NeedsYou. `tool_input` must not appear anywhere in `Session`'s serialized JSON.
- `cargo test -p traffic-light-core; echo EXIT=$?` prints EXIT=0
- `cargo build -p traffic-light; echo EXIT=$?` prints EXIT=0. This checks the Tauri app still compiles; check `src-tauri/Cargo.toml` for the exact package name.
- `cargo fmt --all -- --check; echo EXIT=$?` prints EXIT=0

**[owner] steps**: none. The reviewer rebuilds and reinstalls the widget after review.

---

## Stage 2: Waiting timer, plan limits, click to jump

Owner picked these on 2026-11-20. Three cards. Cards 2 and 3 share the UI files and are built by one builder. Card 4 is built afterwards.

### Card 2: How long a session has been waiting

**What to build**

1. `Session` gains `pub state_since: u64` (ms since epoch, **serialized**). It is set when the session is created and whenever `state` actually changes value, and left alone otherwise. `stale` changes don't touch it. `last_event_at` keeps its current meaning.
2. UI (`ui/main.js`, `ui/style.css`):
   - Rows in `needs_you` show a small elapsed time after the label, in a new `<span class="since">`: nothing under 1 min, then `1m`, `12m`, `1h 5m`.
   - Rows in other states show nothing there.
   - It updates every 15 s; the existing 30 s timer refreshes tooltips, so fold both into one 15 s timer.
   - Tooltip gains "Waiting for 3 min" for `needs_you` and "Working for 3 min" for `working` (from `state_since`).
3. The span must look right in all 6 themes: same font as the label, dimmed, never wrapping. The label keeps ellipsizing first.

**Done when**
- State tests: `state_since` is unchanged by events that keep the state (e.g. two PreToolUse in a row), moves on a real change, and doesn't move when a stale session revives in the same state.
- The demo (`ui/index.html` in a browser, see Card 3 for screenshots) shows a needs_you row with a timer.

### Card 3: 5-hour and weekly plan limits

Claude Code passes a JSON object to the status-line command on stdin. Verified against https://code.claude.com/docs/en/statusline on 2026-11-20, it includes:

```
"rate_limits": {
  "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
  "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 },
  ...
}
```

- `used_percentage` runs from 0 to 100.
- `resets_at` is in Unix epoch **seconds**.
- `rate_limits` is present only for Pro, Max and Ultra subscribers, and only after the session's first API response. Either window may be missing.
- The command runs on new assistant messages, at session start, after `/compact`, on permission-mode change, and when a window's `resets_at` passes.

The owner's status-line script (`~/.claude/statusline-command.sh`, Git Bash) will forward that JSON to the widget with one backgrounded curl line. The reviewer installs that line; your job is the receiving side plus the snippet in the repo.

**What to build**

1. **Core** (`crates/core`): a new route, `POST /status`.
   - It has the same token check and the same "always `200 {}`" rule as `/event`.
   - It deserializes **only** `rate_limits.five_hour` and `rate_limits.seven_day` (`used_percentage: f64`, `resets_at: u64`). Everything else in the body (cwd, transcript_path, model, cost and so on) is dropped by serde, exactly like `HookEvent`.
   - Bad or empty bodies are ignored.
   - Stored as a public `Limits { five_hour: Option<Window>, seven_day: Option<Window> }` with `Window { used_percentage: f64, resets_at: u64 }`, Serialize. The **latest** payload that has `rate_limits` wins, per window: a payload with only `five_hour` keeps the old `seven_day`. Clamp `used_percentage` to 0..=100.
   - The limits are account-wide, not per session.
   - `Shared` exposes `limits() -> Limits`, plus a way to be told when they change. Your choice of API, but keep it small and don't break `Shared::new`'s existing callers or tests (for example an optional second callback set with a builder method before the server starts).
2. **App** (`src-tauri`):
   - A `get_limits` command and a `limits-updated` event.
   - The tray tooltip appends a line like `Limits: 5h 23% · week 41%` when known.
3. **UI**:
   - A slim footer under the session rows (`<div id="limits">`, outside `#panel` or inside it, whichever keeps `fitWindow` correct). It shows the two windows as `5h 23%` and `wk 41%`, each with a thin bar.
   - Colour by level: below 70 neutral, 70 to 89 amber, 90 and over red. Use the existing state colour variables if the themes have them.
   - Tooltip: "5-hour limit: 23% used, resets in 2 h 10 min (14:30)", same for the week.
   - A window whose `resets_at` has passed shows as `0%`, because the window has reset. Re-evaluate on the 15 s timer.
   - Hidden entirely when no limits are known (API-key users).
   - Must look acceptable in all 6 themes, and `fitWindow` must include it.
4. **Repo snippet:**
   - `hooks/statusline-forward.sh`, a one-line comment plus the line the reviewer will paste into the owner's script right after `input=$(cat)`:
     `printf '%s' "$input" | curl -s -m 1 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:7890/status >/dev/null 2>&1 &`
   - README section "Plan limits", explaining how to add it and noting the token header variant.
5. **Demo mode** (browser, no Tauri):
   - Fake limits (5h 23%, wk 78%) and a needs_you row whose `state_since` is 4 minutes ago.
   - `?theme=<name>` picks the theme, so screenshots can be scripted.
6. **README:** privacy list says `/status` reads only the two `rate_limits` windows.

**Decisions already made**
- The footer goes in the widget, and the tray tooltip shows the same numbers.
- No polling, no refreshInterval: the status line already fires on every assistant message.
- Do not read `transcript_path` or anything else from the status JSON.

**Done when** (Cards 2 and 3 together)
- Core tests:
  - `/status` with a realistic full status JSON (including `cwd`, `transcript_path`, `cost`, `model`) stores both windows, and `serde_json::to_string(&limits())` contains none of those other fields.
  - A partial payload keeps the other window.
  - A payload without `rate_limits` leaves the limits unchanged.
  - Garbage gets `200 {}`.
  - A wrong token is ignored.
  - The clamp is applied.
- `cargo test -p traffic-light-core; echo EXIT=$?` gives EXIT=0. `cargo build -p traffic-light; echo EXIT=$?` gives EXIT=0. `cargo fmt --all -- --check; echo EXIT=$?` gives EXIT=0.
- Screenshots of the demo for all 6 themes with `?static&theme=<name>`, using headless Chrome:
  `"/c/Program Files/Google/Chrome/Application/chrome.exe" --headless=new --disable-gpu --force-device-scale-factor=2 --window-size=300,260 --screenshot=<scratch>/<theme>.png "file:///C:/Users/chief/SWProjects/Traffic-light/ui/index.html?static&theme=<theme>"`
  Look at every one yourself. The timer and footer must be legible and not overlap or clip in any theme. Save them outside the repo and list their paths in your report.

**[owner] steps:** none. The reviewer adds the forwarding line to the owner's status-line script (with a backup) and reinstalls the widget.

### Card 4: Click a light to jump to its session's window

Windows only (`#[cfg(windows)]`). On other platforms the click does nothing and everything compiles.

**Approach (decided):** no hook config change. When a hook event arrives, the widget works out which process sent it from the TCP connection itself, and remembers that process and its ancestors. On click it brings the nearest ancestor that owns a visible window to the front.

**What to build**

1. **Core:** the `/event` handler gets the peer `SocketAddr`, via axum `ConnectInfo`, and `serve` switches to `into_make_service_with_connect_info::<SocketAddr>()`.
   - Missing ConnectInfo (the in-process router tests) must work; skip the callback then.
   - `Shared` gets an optional "peer seen" callback `Fn(&str /*session_id*/, SocketAddr)`, called for authorised events that have a session_id. Same API style you used for limits.
   - Core does no Win32 work.
2. **App** (`src-tauri/src/focus.rs`):
   - `remember(session_id, peer)`: if this session has no origin yet, find the PID that owns the client end of the connection. Use `GetExtendedTcpTable` (IPv4, `TCP_TABLE_OWNER_PID_CONNECTIONS`): the row whose **local** addr:port equals the peer addr:port is the client, and its `owningPid` is the sender. Then record the PID chain: that PID and up to 12 ancestors, via a `CreateToolhelp32Snapshot` parent lookup, stopping at PID 0/4 or a cycle. Store `session_id -> Vec<(pid, exe_name)>` in a `Mutex<HashMap>`.
     - Skip the lookup when an origin is already known.
     - Do this off the async server thread (`spawn_blocking`), since it's cheap but blocking.
     - Retry on later events if a lookup failed, up to 5 tries per session.
     - The connection is still open while the handler runs, so the table row exists.
     - Prune entries for sessions that no longer exist whenever the session list is published.
   - `focus(session_id) -> bool` (tauri command `focus_session`):
     - Walk the stored chain from the sender upward. For each pid, find a top-level window that is visible, not owned (`GetWindow(GW_OWNER)` is null), and has a non-empty title, via `EnumWindows` plus `GetWindowThreadProcessId`.
     - Skip `explorer.exe`, and our own process.
     - First hit: if minimised (`IsIconic`), `ShowWindow(SW_RESTORE)`, then bring it to the front.
     - The widget window is `"focus": false`, so Windows may refuse `SetForegroundWindow`. Call the Tauri window's `set_focus()` first, then `SetForegroundWindow`. If it still fails, use the `AttachThreadInput` to the current foreground thread fallback. Report which path worked in your notes.
     - Return true if a window was found.
   - Dependency: the `windows` crate (a current 0.6x version), with only the features needed, under `[target.'cfg(windows)'.dependencies]`.
3. **UI** (`ui/main.js`):
   - Replace "drag on mousedown" with: on mousedown (button 0), remember the point. If the mouse moves more than 4 px with the button held, `startDragging()`. On mouseup without a drag, if the target is inside a `.row`, `invoke("focus_session", { sessionId })`.
   - When it returns false, play a short shake on that row via a CSS class.
   - Rows get `cursor: pointer`.
   - Tooltip gains "Click to open".
4. **README:**
   - A "Click to jump" note: it brings the session's *window* forward. For Windows Terminal that's the whole window, not the specific tab. For VS Code it's the VS Code window, not the terminal panel.
   - Privacy: PIDs and process names are kept in memory only.

**Decisions already made**
- Window-level focus is good enough. Don't try to select a terminal tab or VS Code panel.
- No new hook fields, no `$PPID` tricks.

**Done when**
- `cargo test -p traffic-light-core; echo EXIT=$?` gives EXIT=0, with a router test proving events still work without ConnectInfo. `cargo build -p traffic-light; echo EXIT=$?` gives EXIT=0. `cargo fmt --all -- --check; echo EXIT=$?` gives EXIT=0.
- **Live check.** This machine has Claude Code sessions posting real hooks to port 7890, including the session you are running in, which is inside VS Code.
  1. Quit the installed widget: `powershell -c "Stop-Process -Name traffic-light"`.
  2. Run `cargo tauri dev` in the background, with a debug-only `eprintln!` that prints the resolved chain (`pid exe` list) and the window title chosen on click.
  3. Trigger an event by running any tool, and confirm the log shows a chain ending in a process that owns the VS Code window.
  4. Call the focus path once without the UI, through a `#[cfg(debug_assertions)]` startup env var `TL_FOCUS_TEST=<session_id>` that logs the chosen title, or a similar debug hook. Remove anything that isn't debug-gated.
  5. Afterwards stop the dev app and restart the installed one: `powershell -c "Start-Process \"$env:LOCALAPPDATA\Traffic Light\traffic-light.exe\""`.
- Report the exact chain you saw.

**[owner] steps:** click a light for a Windows Terminal session and for a VS Code session, and confirm the right window comes forward.

---

## Stage 3: Border glow on state change

### Card 5: A glow sweeps the border when a light changes

Owner request (2026-11-20): when a state changes, a glow briefly travels along the border from one corner to the opposite one.

**What to build** (`ui/style.css`, `ui/main.js` only)

1. When any row's state changes (the place in `render` that already adds `.changed`), trigger a border sweep in the **new state's colour** (`--c` of that row, i.e. `var(--red|--amber|--green|--gray)`).
   - **Panel themes** (classic, glass, signal, neon, terminal): the sweep runs on `#panel`'s border.
   - **pills**: `#panel` has no visible border; the sweep runs on that row's own pill border.
   - The sweep is a ring the same shape as the element (inherits `border-radius`), about 1.5px thick, drawn by a pseudo-element (`::after`) with a border-only mask (`mask: linear-gradient(#000 0 0) content-box, linear-gradient(#000 0 0); mask-composite: exclude;` plus padding equal to the ring width). The ring has no layout impact and `pointer-events: none`.
   - The ring's fill is a diagonal band of light, transparent then colour then transparent, at 135deg, plus a soft outer glow if it can be done cheaply. It animates from the top-left corner to the bottom-right corner in about 900 ms ease-out, then fades out. Nothing is visible when idle.
   - Several changes in a row restart the sweep with the latest colour. Don't queue them.
2. Pass the colour from JS with a CSS custom property on the element that sweeps, e.g. `panel.style.setProperty("--sweep", color)`, and restart the animation by removing a class, forcing reflow (`void el.offsetWidth`), then re-adding it. The existing `.changed` trick in `render` shows the pattern. Read the colour with `getComputedStyle(row).getPropertyValue("--c")`.
3. No sweep on first render, i.e. rows appearing for the first time or the initial `get_sessions`. Only real changes trigger one; the existing `if (li.dataset.state)` guard already marks that.
4. Inside `@media (prefers-reduced-motion: reduce)` the sweep is disabled.
5. Must not break the drag or click handling, `fitWindow`, the limits footer, or the existing per-theme `.changed` animations.
6. Demo: the cycling demo triggers sweeps naturally. Add `?sweep=<state>&at=<0..1>` for screenshots: with `?static`, it shows the sweep frozen at that fraction of its animation (negative `animation-delay` plus `animation-play-state: paused`) in that state's colour.

**Decisions already made**
- CSS only, no canvas, no new files, no Rust changes.
- One sweep element per theme as described. Not per row, except in pills.

**Done when**
- `cargo build -p traffic-light; echo EXIT=$?` gives EXIT=0. The UI is bundled, so this mostly proves nothing else broke.
- Headless-Chrome screenshots, same command as Card 3 plus `--virtual-time-budget=3000`, with `?static&theme=<t>&sweep=needs_you&at=0.35` and `&at=0.7` for classic, glass and pills, and `at=0.5` for the other three. Save them to `C:\Users\chief\AppData\Local\Temp\claude\e7cf42f457511299\c37d3613-5d25-48da-b9b1-2f8348212f47\scratchpad\shots\glow\`. Look at every one: the ring must follow the rounded corners, sit exactly on the border, show the band moving diagonally between the two shots, and leave no artefact outside the widget's 8px margin.
- A screenshot with no `sweep` param shows no ring.

**[owner] steps:** watch it live and say if it's too subtle or too much.
