//! Churn checks for arena reuse and owner disposal.

use reactive_core::{create_memo, create_signal, render_effect, Runtime};

#[test]
fn churn_does_not_grow_graph_arenas() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();

    for _ in 0..2_000 {
        let owner = cx.create_owner();
        cx.with_owner(owner, |cx| {
            let (value, set_value) = create_signal(cx, 0_u32);
            let doubled = create_memo(cx, move |cx, _| value.get(cx) * 2);
            let _ = render_effect(cx, move |cx| doubled.get(cx), |_cx, _value| {});
            set_value.set(cx, 1);
        });
        owner.dispose(&cx);
        cx.flush().unwrap();
    }

    let nodes = cx.live_nodes();
    let owners = cx.live_owners();
    for _ in 0..2_000 {
        let owner = cx.create_owner();
        cx.with_owner(owner, |cx| {
            let (value, set_value) = create_signal(cx, 0_u32);
            let doubled = create_memo(cx, move |cx, _| value.get(cx) * 2);
            let _ = render_effect(cx, move |cx| doubled.get(cx), |_cx, _value| {});
            set_value.set(cx, 1);
        });
        owner.dispose(&cx);
        cx.flush().unwrap();
    }

    assert_eq!(cx.live_nodes(), nodes, "node arena must not grow");
    assert_eq!(cx.live_owners(), owners, "owner arena must not grow");
}

#[test]
#[ignore = "long soak; run with SOAK_SECONDS=86400"]
fn soak_long_churn() {
    use std::time::{Duration, Instant};

    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    for _ in 0..100 {
        let owner = cx.create_owner();
        cx.with_owner(owner, |cx| {
            let (value, set_value) = create_signal(cx, 0_u32);
            let doubled = create_memo(cx, move |cx, _| value.get(cx) * 2);
            let _ = render_effect(cx, move |cx| doubled.get(cx), |_cx, _value| {});
            set_value.set(cx, 1);
        });
        owner.dispose(&cx);
        cx.flush().unwrap();
    }
    let nodes = cx.live_nodes();
    let owners = cx.live_owners();
    let seconds = std::env::var("SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(86_400);
    let start = Instant::now();
    let mut rounds = 0_u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        let owner = cx.create_owner();
        cx.with_owner(owner, |cx| {
            let (value, set_value) = create_signal(cx, 0_u32);
            let doubled = create_memo(cx, move |cx, _| value.get(cx) * 2);
            let _ = render_effect(cx, move |cx| doubled.get(cx), |_cx, _value| {});
            set_value.set(cx, 1);
        });
        owner.dispose(&cx);
        cx.flush().unwrap();
        rounds += 1;
        if rounds % 1_000 == 0 {
            assert_eq!(cx.live_nodes(), nodes, "nodes grew after {rounds} rounds");
            assert_eq!(
                cx.live_owners(),
                owners,
                "owners grew after {rounds} rounds"
            );
        }
    }
    assert_eq!(cx.live_nodes(), nodes);
    assert_eq!(cx.live_owners(), owners);
}
