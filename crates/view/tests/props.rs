use reactive_core::{create_signal, Runtime};
use view::{CallbackWith, Prop};

#[test]
fn props_are_static_or_reactive() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 1_u32);
    let fixed = Prop::static_value(10_u32);
    let live = Prop::derived(&cx, move |cx| count.get(cx) + 1);
    let from_signal: Prop<u32> = count.into();
    let memo = reactive_core::create_memo(&cx, move |cx, _| count.get(cx) * 2);
    let from_memo: Prop<u32> = memo.into();

    assert!(!fixed.is_reactive());
    assert!(live.is_reactive());
    assert_eq!(fixed.get(&cx), 10);
    assert_eq!(live.get(&cx), 2);
    assert_eq!(from_signal.get(&cx), 1);
    assert_eq!(from_memo.get(&cx), 2);

    set_count.set(&cx, 4);
    cx.flush().unwrap();
    assert_eq!(live.get(&cx), 5);
    assert_eq!(from_signal.get(&cx), 4);
    assert_eq!(from_memo.get(&cx), 8);
    assert_eq!(fixed.get(&cx), 10);

    let (written, set_written) = create_signal(&cx, 0_u32);
    let setter: CallbackWith<u32> = set_written.into();
    setter.call(&cx, 9);
    cx.flush().unwrap();
    assert_eq!(written.get(&cx), 9);

    let named_setter = CallbackWith::set(set_written);
    named_setter.call(&cx, 10);
    cx.flush().unwrap();
    assert_eq!(written.get(&cx), 10);

    // A prop boundary takes `impl Into<CallbackWith<T>>`: both an already-built
    // callback and a write handle satisfy it, so forwarding needs no wrapping.
    fn forward(value: impl Into<CallbackWith<u32>>) -> CallbackWith<u32> {
        value.into()
    }
    let forwarded = forward(named_setter.clone());
    forwarded.call(&cx, 11);
    cx.flush().unwrap();
    assert_eq!(written.get(&cx), 11);
    let forwarded_from_write = forward(set_written);
    forwarded_from_write.call(&cx, 12);
    cx.flush().unwrap();
    assert_eq!(written.get(&cx), 12);

    let closure_setter: CallbackWith<u32> =
        (move |cx: &reactive_core::Cx<'_>, value| set_written.set(cx, value + 1)).into();
    closure_setter.call(&cx, 12);
    cx.flush().unwrap();
    assert_eq!(written.get(&cx), 13);

    let sum = std::rc::Rc::new(std::cell::Cell::new(0.0_f32));
    let sum_in_callback = std::rc::Rc::clone(&sum);
    let drag: CallbackWith<(f32, f32)> =
        (move |_cx: &reactive_core::Cx<'_>, (x, y): (f32, f32)| {
            sum_in_callback.set(x + y);
        })
        .into();
    drag.call(&cx, (1.0, 2.0));
    assert_eq!(sum.get(), 3.0);
}
