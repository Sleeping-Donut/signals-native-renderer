use reactive_core::{create_signal, Runtime};
use view::Prop;

#[test]
fn props_are_static_or_reactive() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 1_u32);
    let fixed = Prop::static_value(10_u32);
    let live = Prop::dynamic(move |cx| count.get(cx) + 1);

    assert!(!fixed.is_reactive());
    assert!(live.is_reactive());
    assert_eq!(fixed.get(&cx), 10);
    assert_eq!(live.get(&cx), 2);

    set_count.set(&cx, 4);
    cx.flush().unwrap();
    assert_eq!(live.get(&cx), 5);
    assert_eq!(fixed.get(&cx), 10);
}
