//! Core of the Claude Code traffic light: the per-session state machine and
//! the localhost HTTP server that Claude Code hooks post to. No UI in here, so
//! it builds and tests anywhere.

pub mod server;
pub mod state;

pub use server::{router, serve, Shared, DEFAULT_PORT, TOKEN_HEADER};
pub use state::{most_urgent, HookEvent, Session, State, Store, Transition};
