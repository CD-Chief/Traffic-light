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
- `/event` always answers `200 {}`, even for garbage, so the widget is never
  the reason a hook errors.
- Optional hardening: set the `TRAFFIC_LIGHT_TOKEN` environment variable
  before the widget starts and it will ignore events without a matching
  `X-Traffic-Light-Token` header. Add to each HTTP hook:
  `"headers": { "X-Traffic-Light-Token": "$TRAFFIC_LIGHT_TOKEN" }, "allowedEnvVars": ["TRAFFIC_LIGHT_TOKEN"]`
  (and `-H "X-Traffic-Light-Token: <token>"` to the `curl.exe` args).
