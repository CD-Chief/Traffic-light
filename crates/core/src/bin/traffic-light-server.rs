//! Milestone 1: the hook server on its own, printing transitions to the
//! console. Handy for wiring up hooks and watching real sessions without the
//! widget. Don't run it alongside the widget: both want port 7890.

use traffic_light_core::{serve, Shared, DEFAULT_PORT};

#[tokio::main]
async fn main() {
    let token = std::env::var("TRAFFIC_LIGHT_TOKEN").ok();
    let shared = Shared::new(token, |sessions, transition| {
        let name = |s: Option<traffic_light_core::State>| s.map_or("-", |s| s.name());
        match transition {
            Some(t) => println!(
                "{:<24} {:<18} {} -> {}   ({} session{})",
                t.label,
                t.event,
                name(t.from),
                if t.to.is_some() {
                    name(t.to)
                } else {
                    "removed"
                },
                sessions.len(),
                if sessions.len() == 1 { "" } else { "s" },
            ),
            None => println!("sweep: {} session(s) left", sessions.len()),
        }
    });

    println!("listening on http://127.0.0.1:{DEFAULT_PORT}  (POST /event, GET /health)");
    if let Err(e) = serve(DEFAULT_PORT, shared).await {
        eprintln!("could not serve on 127.0.0.1:{DEFAULT_PORT}: {e}");
        std::process::exit(1);
    }
}
