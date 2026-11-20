//! Churn checks: a rapid deterministic fuzz run for agents, and a long
//! wall-clock monitor that a human can leave running in the background.
//!
//! Neither test is part of CI.

use std::process::Command;
use std::time::{Duration, Instant};

use reactive_core::{create_memo, create_signal, render_effect, Cx, Runtime};

fn churn_owner(cx: &Cx<'_>, random: u64) {
    let owner = cx.create_owner();
    cx.with_owner(owner, |cx| match random % 4 {
        0 => {
            let (value, set_value) = create_signal(cx, 0_u32);
            let _ = value;
            set_value.set(cx, 1);
        }
        1 => {
            let (value, set_value) = create_signal(cx, 0_u32);
            let doubled = create_memo(cx, move |cx, _| value.get(cx) * 2);
            let _ = render_effect(cx, move |cx| doubled.get(cx), |_cx, _value| {});
            set_value.set(cx, 1);
        }
        2 => {
            let child = cx.create_owner();
            cx.with_owner(child, |cx| {
                let (value, set_value) = create_signal(cx, 0_u32);
                let _ = create_memo(cx, move |cx, _| value.get(cx) + 1);
                set_value.set(cx, 2);
            });
        }
        _ => {
            for index in 0..4_u32 {
                let (value, set_value) = create_signal(cx, index);
                let _ = value;
                set_value.set(cx, index + 1);
            }
        }
    });
    owner.dispose(cx);
}

fn rss_kb() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Rapid deterministic churn. This is the agent-runnable long-term simulation.
#[test]
#[ignore = "rapid fuzz; run scripts/soak-fuzz.sh"]
fn fuzz_churn_is_bounded() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let nodes = cx.live_nodes();
    let owners = cx.live_owners();
    let rounds = std::env::var("SOAK_FUZZ_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(100_000);
    let mut random = 0x9e37_79b9_7f4a_7c15_u64;
    let start = Instant::now();
    for round in 0..rounds {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        churn_owner(&cx, random ^ round);
        cx.flush().unwrap();
        if round % 10_000 == 0 {
            assert_eq!(cx.live_nodes(), nodes, "nodes grew at round {round}");
            assert_eq!(cx.live_owners(), owners, "owners grew at round {round}");
        }
    }
    cx.flush().unwrap();
    assert_eq!(cx.live_nodes(), nodes);
    assert_eq!(cx.live_owners(), owners);
    eprintln!(
        "fuzz churn: {rounds} rounds in {:.3}s ({:.0} rounds/s), nodes={nodes}, owners={owners}, rss={:?} KB",
        start.elapsed().as_secs_f64(),
        rounds as f64 / start.elapsed().as_secs_f64(),
        rss_kb()
    );
}

/// Long wall-clock monitor for a human to leave running.
///
/// ```bash
/// SOAK_SECONDS=86400 SOAK_INTERVAL_SECS=3600 \
///   cargo test -p reactive-core --test soak soak_monitor -- --ignored --nocapture
/// ```
#[test]
#[ignore = "manual long-running soak; use scripts/soak-monitor.sh"]
fn soak_monitor() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();

    for _ in 0..100 {
        churn_owner(&cx, 1);
        cx.flush().unwrap();
    }
    let nodes = cx.live_nodes();
    let owners = cx.live_owners();
    let seconds = std::env::var("SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(86_400);
    let interval = std::env::var("SOAK_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3_600)
        .max(1);
    eprintln!(
        "soak monitor start: duration={seconds}s interval={interval}s nodes={nodes} owners={owners} rss={:?} KB",
        rss_kb()
    );

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut rounds = 0_u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        churn_owner(&cx, rounds);
        cx.flush().unwrap();
        rounds += 1;
        if last_report.elapsed() >= Duration::from_secs(interval) {
            assert_eq!(cx.live_nodes(), nodes, "nodes grew after {rounds} rounds");
            assert_eq!(
                cx.live_owners(),
                owners,
                "owners grew after {rounds} rounds"
            );
            eprintln!(
                "soak monitor: {}s elapsed, {} rounds ({:.0}/s), nodes={}, owners={}, rss={:?} KB",
                start.elapsed().as_secs(),
                rounds,
                rounds as f64 / start.elapsed().as_secs_f64(),
                cx.live_nodes(),
                cx.live_owners(),
                rss_kb()
            );
            last_report = Instant::now();
        }
    }
    assert_eq!(cx.live_nodes(), nodes);
    assert_eq!(cx.live_owners(), owners);
    eprintln!(
        "soak monitor done: {}s, {} rounds, nodes={}, owners={}, rss={:?} KB",
        start.elapsed().as_secs(),
        rounds,
        cx.live_nodes(),
        cx.live_owners(),
        rss_kb()
    );
}
