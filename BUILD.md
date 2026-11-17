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
