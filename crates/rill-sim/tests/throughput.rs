//! Phase 1 check: 500 messages per second flow from simulated adapters through the hub into the core.

use std::time::{Duration, Instant};

use rill_core::{Adapter, Hub, Platform, Session, now_ms};
use rill_sim::{Profile, SimAdapter};

#[test]
fn hub_carries_500_messages_per_second_into_the_session() {
    let adapters: Vec<Box<dyn Adapter>> = Platform::ALL
        .into_iter()
        .map(|p| {
            Box::new(SimAdapter::new(
                p,
                Profile {
                    rate: 500.0 / 3.0,
                    ..Profile::default()
                },
            )) as Box<dyn Adapter>
        })
        .collect();
    let (hub, mut events) = Hub::start(adapters).expect("runtime starts");
    let mut session = Session::new(5_000);
    let start = Instant::now();
    let mut messages = 0usize;
    let mut busy = Duration::ZERO;
    hub.handle().block_on(async {
        while start.elapsed() < Duration::from_secs(3) {
            let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(100), events.recv()).await
            else {
                continue;
            };
            let t = Instant::now();
            if matches!(event, rill_core::Event::Message(_)) {
                messages += 1;
            }
            session.apply(event, now_ms());
            busy += t.elapsed();
        }
    });
    // 300 ms connecting, then ~2.7 s at 500/s.
    assert!(messages >= 1_200, "only {messages} messages in 3 s");
    assert!(session.feed.len() <= 5_000);
    let per_msg = busy / messages as u32;
    assert!(
        per_msg < Duration::from_micros(200),
        "core took {per_msg:?} per message"
    );
    eprintln!("{messages} messages, {per_msg:?} of core time each");
}
