//! The runtime's ManualSpawner drives reactive-core async flights.

use reactive_core::{create_async_memo, loading_boundary, Runtime};
use runtime::ManualSpawner;

#[test]
fn manual_spawner_drives_an_async_boundary() {
    let runtime = Runtime::with_spawner(ManualSpawner::new());
    let cx = runtime.cx();
    let data = create_async_memo(&cx, |_cx, _| Box::pin(async { Ok("ready".to_string()) }));
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "ready");
}
