use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// No event for this long dims the light. Safety net for crashed sessions
/// only; timeouts never drive normal state.
pub const STALE_AFTER_MS: u64 = 30 * 60 * 1000;
/// No event for this long removes the session.
pub const REMOVE_AFTER_MS: u64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Working,
    NeedsYou,
    Done,
    Error,
}

impl State {
    /// Higher is more urgent: Needs you > Error > Working > Done.
    pub fn urgency(self) -> u8 {
        match self {
            State::NeedsYou => 3,
            State::Error => 2,
            State::Working => 1,
            State::Done => 0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::NeedsYou => "needs you",
            State::Done => "done",
            State::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub session_id: String,
    /// Last folder of `cwd`, shown instead of the UUID.
    pub label: String,
    pub cwd: String,
    pub state: State,
    /// Milliseconds since the Unix epoch.
    pub last_event_at: u64,
    pub last_tool: Option<String>,
    /// Dimmed: nothing heard for 30 minutes.
    pub stale: bool,
    /// Insertion order, so rows keep their place in the widget.
    #[serde(skip)]
    seq: u64,
}

/// The only fields ever read from a hook payload. Everything else in the body
/// (prompts, commands, file contents) is dropped by the deserializer and never
/// stored or logged.
#[derive(Debug, Default, Deserialize)]
pub struct HookEvent {
    pub session_id: Option<String>,
    pub hook_event_name: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub notification_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub label: String,
    pub event: String,
    pub from: Option<State>,
    /// `None` means the session was removed.
    pub to: Option<State>,
}

#[derive(Debug, Default)]
pub struct Store {
    sessions: HashMap<String, Session>,
    next_seq: u64,
}

enum Action {
    /// SessionStart: add as Done, but leave an existing session's state alone
    /// (it also fires mid-turn after compaction).
    Start,
    Set(State),
    Notification,
    Remove,
}

fn action_for(event: &str) -> Option<Action> {
    Some(match event {
        "SessionStart" => Action::Start,
        "UserPromptSubmit" => Action::Set(State::Working),
        // PreToolUse fires *before* a permission prompt, so the Post* events
        // are what clear "Needs you" once an approved tool has run.
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied"
        | "ElicitationResult" => Action::Set(State::Working),
        // Fires the moment a prompt appears; the permission_prompt
        // Notification only follows after ~6 s without typing.
        "PermissionRequest" | "Elicitation" => Action::Set(State::NeedsYou),
        "Notification" => Action::Notification,
        "Stop" => Action::Set(State::Done),
        "StopFailure" => Action::Set(State::Error),
        "SessionEnd" => Action::Remove,
        _ => return None,
    })
}

fn label_for(cwd: &str, session_id: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| session_id.chars().take(8).collect())
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one hook event. Returns a transition when anything visible
    /// changed (state, membership, label, tool or staleness).
    pub fn apply(&mut self, ev: &HookEvent, now: u64) -> Option<Transition> {
        let id = ev.session_id.as_deref().filter(|s| !s.is_empty())?;
        let event = ev.hook_event_name.as_deref()?;
        let action = action_for(event)?;

        if let Action::Remove = action {
            let old = self.sessions.remove(id)?;
            return Some(Transition {
                label: old.label,
                event: event.to_owned(),
                from: Some(old.state),
                to: None,
            });
        }

        let existing = self.sessions.get(id);
        let from = existing.map(|s| s.state);
        let target = match action {
            Action::Start => from.unwrap_or(State::Done),
            Action::Set(s) => s,
            Action::Notification => match ev.notification_type.as_deref() {
                Some(
                    "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog"
                    | "agent_needs_input",
                ) => State::NeedsYou,
                // idle reminders, auth results, quota notices: not a call for attention.
                Some(_) => from.unwrap_or(State::Done),
                // Older Claude Code without notification_type: the idle
                // reminder arrives after a finished turn, so Done stays Done.
                None => match from {
                    Some(State::Done) | None => State::Done,
                    Some(_) => State::NeedsYou,
                },
            },
            Action::Remove => unreachable!(),
        };

        let before = existing.cloned();
        let seq = self.next_seq;
        let s = self.sessions.entry(id.to_owned()).or_insert_with(|| Session {
            session_id: id.to_owned(),
            label: label_for("", id),
            cwd: String::new(),
            state: State::Done,
            last_event_at: now,
            last_tool: None,
            stale: false,
            seq,
        });
        if before.is_none() {
            self.next_seq += 1;
        }

        // The project folder is fixed at first sight (and on SessionStart), so
        // the label doesn't jump around when Claude cd's into a subfolder.
        if let Some(cwd) = ev.cwd.as_deref().filter(|c| !c.is_empty()) {
            if s.cwd.is_empty() || matches!(action, Action::Start) {
                s.cwd = cwd.to_owned();
                s.label = label_for(cwd, id);
            }
        }
        match event {
            "PreToolUse" | "PermissionRequest" => {
                if let Some(tool) = ev.tool_name.as_deref().filter(|t| !t.is_empty()) {
                    s.last_tool = Some(tool.to_owned());
                }
            }
            "UserPromptSubmit" | "Stop" | "StopFailure" => s.last_tool = None,
            _ => {}
        }
        s.state = target;
        s.stale = false;
        s.last_event_at = now;

        let changed = match &before {
            None => true,
            Some(b) => {
                b.state != s.state || b.stale || b.label != s.label || b.last_tool != s.last_tool
            }
        };
        changed.then(|| Transition {
            label: s.label.clone(),
            event: event.to_owned(),
            from,
            to: Some(target),
        })
    }

    /// Dim sessions that have gone quiet and drop long-dead ones. Returns
    /// true when anything changed.
    pub fn sweep(&mut self, now: u64) -> bool {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, s| now.saturating_sub(s.last_event_at) < REMOVE_AFTER_MS);
        let mut changed = self.sessions.len() != before;
        for s in self.sessions.values_mut() {
            if !s.stale && now.saturating_sub(s.last_event_at) >= STALE_AFTER_MS {
                s.stale = true;
                changed = true;
            }
        }
        changed
    }

    /// All sessions in the order they were first seen.
    pub fn list(&self) -> Vec<Session> {
        let mut v: Vec<Session> = self.sessions.values().cloned().collect();
        v.sort_by_key(|s| s.seq);
        v
    }
}

/// The state the tray icon should show, ignoring dimmed sessions unless
/// they are all that is left.
pub fn most_urgent(sessions: &[Session]) -> Option<State> {
    let live = sessions.iter().filter(|s| !s.stale).map(|s| s.state);
    live.max_by_key(|s| s.urgency())
        .or_else(|| sessions.iter().map(|s| s.state).max_by_key(|s| s.urgency()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, name: &str) -> HookEvent {
        HookEvent {
            session_id: Some(id.into()),
            hook_event_name: Some(name.into()),
            cwd: Some(r"C:\Users\chief\code\chiefosei.dev".into()),
            ..Default::default()
        }
    }

    fn notif(id: &str, kind: Option<&str>) -> HookEvent {
        HookEvent {
            notification_type: kind.map(Into::into),
            ..ev(id, "Notification")
        }
    }

    fn state(store: &Store, id: &str) -> State {
        store.list().into_iter().find(|s| s.session_id == id).unwrap().state
    }

    #[test]
    fn full_turn_lifecycle() {
        let mut st = Store::new();
        st.apply(&ev("a", "SessionStart"), 0);
        assert_eq!(state(&st, "a"), State::Done);
        assert_eq!(st.list()[0].label, "chiefosei.dev");

        st.apply(&ev("a", "UserPromptSubmit"), 1);
        assert_eq!(state(&st, "a"), State::Working);

        let mut pre = ev("a", "PreToolUse");
        pre.tool_name = Some("Bash".into());
        st.apply(&pre, 2);
        assert_eq!(st.list()[0].last_tool.as_deref(), Some("Bash"));

        st.apply(&ev("a", "PermissionRequest"), 3);
        assert_eq!(state(&st, "a"), State::NeedsYou);
        st.apply(&ev("a", "PostToolUse"), 4);
        assert_eq!(state(&st, "a"), State::Working);

        st.apply(&ev("a", "Stop"), 5);
        assert_eq!(state(&st, "a"), State::Done);
        assert_eq!(st.list()[0].last_tool, None);

        st.apply(&ev("a", "UserPromptSubmit"), 6);
        st.apply(&ev("a", "StopFailure"), 7);
        assert_eq!(state(&st, "a"), State::Error);

        let t = st.apply(&ev("a", "SessionEnd"), 8).unwrap();
        assert_eq!(t.to, None);
        assert!(st.list().is_empty());
    }

    #[test]
    fn notification_types() {
        let mut st = Store::new();
        st.apply(&ev("a", "UserPromptSubmit"), 0);
        st.apply(&notif("a", Some("permission_prompt")), 1);
        assert_eq!(state(&st, "a"), State::NeedsYou);

        st.apply(&ev("a", "Stop"), 2);
        st.apply(&notif("a", Some("idle_prompt")), 3);
        assert_eq!(state(&st, "a"), State::Done);

        // auth_success and friends never change the light
        st.apply(&ev("a", "UserPromptSubmit"), 4);
        st.apply(&notif("a", Some("auth_success")), 5);
        assert_eq!(state(&st, "a"), State::Working);
    }

    #[test]
    fn untyped_notification_falls_back_to_done_rule() {
        let mut st = Store::new();
        st.apply(&ev("a", "SessionStart"), 0);
        st.apply(&notif("a", None), 1);
        assert_eq!(state(&st, "a"), State::Done);

        st.apply(&ev("a", "UserPromptSubmit"), 2);
        st.apply(&notif("a", None), 3);
        assert_eq!(state(&st, "a"), State::NeedsYou);
    }

    #[test]
    fn unknown_events_and_bad_payloads_are_ignored() {
        let mut st = Store::new();
        assert!(st.apply(&ev("a", "SubagentStop"), 0).is_none());
        assert!(st.apply(&HookEvent::default(), 0).is_none());
        assert!(st.apply(&ev("", "Stop"), 0).is_none());
        assert!(st.apply(&ev("a", "SessionEnd"), 0).is_none());
        assert!(st.list().is_empty());
    }

    #[test]
    fn widget_started_mid_session_picks_the_session_up() {
        let mut st = Store::new();
        st.apply(&ev("a", "PreToolUse"), 0);
        assert_eq!(state(&st, "a"), State::Working);
    }

    #[test]
    fn session_start_mid_turn_keeps_state() {
        let mut st = Store::new();
        st.apply(&ev("a", "UserPromptSubmit"), 0);
        st.apply(&ev("a", "SessionStart"), 1); // e.g. after compaction
        assert_eq!(state(&st, "a"), State::Working);
    }

    #[test]
    fn label_is_stable_and_handles_both_separators() {
        assert_eq!(label_for("/home/me/proj/", "x"), "proj");
        assert_eq!(label_for(r"C:\code\thing\", "x"), "thing");
        assert_eq!(label_for("", "0123456789abcdef"), "01234567");

        let mut st = Store::new();
        st.apply(&ev("a", "SessionStart"), 0);
        let mut sub = ev("a", "PreToolUse");
        sub.cwd = Some(r"C:\Users\chief\code\chiefosei.dev\src".into());
        st.apply(&sub, 1);
        assert_eq!(st.list()[0].label, "chiefosei.dev");
    }

    #[test]
    fn stale_then_removed_and_revived_by_events() {
        let mut st = Store::new();
        st.apply(&ev("a", "UserPromptSubmit"), 0);
        assert!(!st.sweep(STALE_AFTER_MS - 1));
        assert!(st.sweep(STALE_AFTER_MS));
        assert!(st.list()[0].stale);
        assert_eq!(state(&st, "a"), State::Working); // state itself untouched

        assert!(st.apply(&ev("a", "PreToolUse"), STALE_AFTER_MS + 1).is_some());
        assert!(!st.list()[0].stale);

        assert!(st.sweep(STALE_AFTER_MS + 1 + REMOVE_AFTER_MS));
        assert!(st.list().is_empty());
    }

    #[test]
    fn ordering_and_urgency() {
        let mut st = Store::new();
        st.apply(&ev("b", "SessionStart"), 0);
        st.apply(&ev("a", "UserPromptSubmit"), 1);
        st.apply(&ev("c", "StopFailure"), 2);
        let ids: Vec<_> = st.list().into_iter().map(|s| s.session_id).collect();
        assert_eq!(ids, ["b", "a", "c"]);
        assert_eq!(most_urgent(&st.list()), Some(State::Error));
        st.apply(&ev("a", "PermissionRequest"), 3);
        assert_eq!(most_urgent(&st.list()), Some(State::NeedsYou));
        assert_eq!(most_urgent(&[]), None);
    }
}
