# Claude Code Traffic Light

A small always-on-top Windows widget that shows one light per running Claude
Code session, so you can see at a glance which sessions are working, which
need you, and which are done.

| Light | Meaning |
| --- | --- |
| Green | Claude is thinking or running tools |
| Amber (pulsing) | Waiting on a permission prompt or your input |
| Gray | Turn finished, your move |
| Red | The turn failed (API error, etc.) |

Everything is local. Claude Code hooks post to a server on `127.0.0.1:7890`
inside the widget; nothing leaves the machine. Only local terminal sessions on
this PC are covered (cloud/remote sessions can't reach your localhost).

## Layout

```
crates/core/   state machine + axum hook server (no UI; `cargo test` runs here)
src-tauri/     the Tauri 2 app: window, tray, autostart, single instance
ui/            the widget: plain HTML/CSS/JS, all designs live in style.css
hooks/         the hook block to merge into %USERPROFILE%\.claude\settings.json
```

## Build and run (Windows)

Needs [Rust](https://rustup.rs) and the WebView2 runtime (preinstalled on Windows 11).

```
cargo install tauri-cli --version "^2"
cargo tauri build
```

The installer lands in `target\release\bundle\nsis\`. For development use
`cargo tauri dev`. On first launch the widget registers itself to start on
login; untick **Start on login** in the tray menu to stop that.

Then merge the `hooks` block from
[`hooks/settings.example.json`](hooks/settings.example.json) into
`%USERPROFILE%\.claude\settings.json` and start a new Claude Code session.

### Without the widget

`cargo run` starts just the hook server and prints every transition to the
console, which is the quickest way to check the hooks are wired up:

```
curl.exe http://127.0.0.1:7890/health
```

Quit the widget first; both want port 7890.

`ui/index.html` also opens in a normal browser with fake sessions cycling
through the states, for previewing and working on designs.

## Events

| Hook event | Light |
| --- | --- |
| `SessionStart` | Add session as Done (state kept if the session is already known) |
| `UserPromptSubmit` | Working |
| `PreToolUse` | Working; Needs you for `AskUserQuestion` / `ExitPlanMode` |
| `PostToolUse`, `PostToolUseFailure`, `PermissionDenied` | Working (unless another prompt is still open) |
| `PermissionRequest` | Needs you |
| `Notification` | Needs you for `permission_prompt` / elicitation / `agent_needs_input`; ignored otherwise (including `idle_prompt`) |
| `Stop` | Done |
| `StopFailure` | Error |
| `SessionEnd` | Remove |

Amber holds until the waiting tool's own result arrives (matched by
`tool_use_id`), even when other tools run in the meantime. A new prompt or the
end of the turn clears it.

Anything else is ignored. A session the widget has never heard of is added on
its first event, so restarting the widget mid-session is fine. Safety net for
crashed sessions only: 30 min of silence dims a light, 6 h removes it.

### Where this differs from the original spec

Checked against the Claude Code hooks reference (Nov 2026):

- **`SessionStart` can't be an HTTP hook** (command and MCP-tool hooks only),
  so that one event posts through `curl.exe` (ships with Windows) as an
  `async` command hook.
- **`PermissionRequest` is hooked for amber.** The `permission_prompt`
  notification only fires after the prompt has sat for about six seconds
  without typing; `PermissionRequest` fires the moment it appears.
- **`PostToolUse` / `PostToolUseFailure` are hooked after all.** `PreToolUse`
  fires *before* the permission prompt, so it can't clear amber after you
  approve. Nothing fires at the moment of approval; the light goes green
  again when the approved tool finishes. A long-running approved command
  therefore stays amber until it completes.
- **`PermissionRequest` has no `tool_use_id`**, so it is matched to the most
  recent open `PreToolUse` of the same tool that isn't already waiting. With
  no match (widget started mid-turn) the light still goes amber, but the
  next tool event of any kind can clear it.
- **Notifications are told apart by `notification_type`**, so the idle
  reminder never turns a finished session amber. The "Done stays Done" rule
  remains as a fallback for payloads without the field.
- **HTTP hooks have no `async` option** (command hooks only). If the ~2 s
  refused-connection delay with the widget closed ever bothers you, copy the
  `SessionStart` entry's `curl.exe` command hook to the other events.

## Waiting timer

A row that needs you shows how long it has been waiting after its name: nothing
under a minute, then `1m`, `12m`, `1h 5m`. Hovering a row also says how long a
waiting or working session has been in that state. It updates every 15 seconds.

## Plan limits

The widget footer shows your 5-hour and weekly plan usage (`5h 23%`, `wk 41%`,
each with a thin bar: amber from 70%, red from 90%), and the tray tooltip adds
a `Limits:` line. A window whose reset time has passed shows `0%`. The footer
is hidden until limits are known, which they never are for API-key users:
Claude Code only reports `rate_limits` to Pro, Max and Ultra subscribers, and
only after a session's first API response.

The numbers come from Claude Code's status line, which passes a JSON object to
your status-line command on stdin. Add the one line from
[`hooks/statusline-forward.sh`](hooks/statusline-forward.sh) to your
status-line script (for example `~/.claude/statusline-command.sh`) right after
it reads its input:

```
input=$(cat)
printf '%s' "$input" | curl -s -m 1 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:7890/status >/dev/null 2>&1 &
```

It runs in the background and never delays the status line. If you set
`TRAFFIC_LIGHT_TOKEN`, add `-H "X-Traffic-Light-Token: $TRAFFIC_LIGHT_TOKEN"`
to that curl command. No polling is involved: the status line already runs on
every assistant message.

## Click to jump

Click a light (a press that moves less than 4 px; anything more drags the
widget) to bring that session's window to the front. It raises the *window*:
for Windows Terminal that is the whole window, not the specific tab, and for
VS Code it is the VS Code window, not the terminal panel. If no window is found
the row shakes. Windows only; elsewhere the click does nothing.

No hook configuration is involved. When an event arrives, the widget looks up
which process owns the client end of that localhost connection, remembers it
and its parent processes, and on click raises the nearest one that owns a
visible window.

## Designs

Tray menu → **Design**. The choice is remembered.

- **Classic**: dark pill, one dot per row that changes colour in place.
- **Liquid glass**: a glass capsule per row with four wells
  (red · amber · green · gray); one droplet slides between them, stretching as
  it moves and changing colour on the way.
- **Signal**: a traffic light on its side, three lenses in a black housing.
  The lit lens switches instantly; Done is all lenses off.
- **Neon**: glowing ring, label lit in the state colour, flickers on change.
- **Pills**: no panel; each session is its own pill washed in its colour.
- **Terminal**: monospace, block cursor, status word (RUN / WAIT / IDLE / ERR).

Every design shares one DOM, and a design is one `html[data-theme="…"]` block
in `ui/style.css`. To add one, write the block and add its name to `THEMES` in
`ui/main.js` and `src-tauri/src/tray.rs`.

The glass is drawn by the widget itself; it doesn't blur the desktop behind
the window, because a transparent WebView2 window can't see what is behind it.
Real blur would mean Windows acrylic (`windowEffects` in `tauri.conf.json`),
which is rectangular and would need the panel reshaped to suit.

## Privacy and security

- Bound to `127.0.0.1` only.
- Hook bodies can contain prompts, paths and commands. Only `session_id`,
  `hook_event_name`, `cwd`, `tool_name`, `tool_use_id` (an opaque ID) and
  `notification_type` are read; the
  rest is discarded during parsing and nothing is logged or written to disk.
- For click to jump, the widget keeps each session's sender process ID and the
  names of its parent processes (for example `claude.exe`, `Code.exe`). They
  are looked up from the local connection table, held in memory only, never
  logged in release builds, and dropped when the session disappears.
- `/event` always answers `200 {}`, even for garbage, so the widget is never
  the reason a hook errors.
- `POST /status` (plan limits) reads only `rate_limits.five_hour` and
  `rate_limits.seven_day` (`used_percentage` and `resets_at`) from the
  status-line JSON. The rest (cwd, transcript path, model, cost) is discarded
  during parsing, and the same `200 {}` and token rules apply as for `/event`.
- Optional hardening: set the `TRAFFIC_LIGHT_TOKEN` environment variable
  before the widget starts and it will ignore events without a matching
  `X-Traffic-Light-Token` header. Add to each HTTP hook:
  `"headers": { "X-Traffic-Light-Token": "$TRAFFIC_LIGHT_TOKEN" }, "allowedEnvVars": ["TRAFFIC_LIGHT_TOKEN"]`
  (and `-H "X-Traffic-Light-Token: <token>"` to the `curl.exe` args).
