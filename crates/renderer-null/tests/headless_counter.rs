//! The counter reference app on the null renderer.
//!
//! This is the Phase 3 milestone: the reactive half and a renderer backend meet
//! with a deterministic operation log, without a native window or layout.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{
    create_memo, create_signal, render_effect, ReadSignal, Runtime, RuntimeHandle, WriteSignal,
};
use renderer_api::{ElementTag, EventKind, NodeId, Renderer};
use renderer_null::{NullRenderer, Op};

struct CounterNodes {
    local_text: NodeId,
    shared_text: NodeId,
    double_text: NodeId,
    local_button: NodeId,
    shared_button: NodeId,
}

fn mount_counter(
    handle: &RuntimeHandle,
    renderer: &Rc<RefCell<NullRenderer>>,
    parent: NodeId,
    shared: ReadSignal<u32>,
    bump_shared: WriteSignal<u32>,
) -> CounterNodes {
    let cx = handle.cx();
    let (local, set_local) = create_signal(&cx, 0_u32);
    let local_double = create_memo(&cx, move |cx, _| local.get(cx) * 2);

    let column = renderer
        .borrow_mut()
        .create_element(ElementTag::new("Column"))
        .unwrap();
    renderer.borrow_mut().insert(parent, column, None).unwrap();

    let local_button = renderer
        .borrow_mut()
        .create_element(ElementTag::new("Button"))
        .unwrap();
    renderer
        .borrow_mut()
        .insert(column, local_button, None)
        .unwrap();
    let local_text = renderer.borrow_mut().create_text("local 0").unwrap();
    renderer
        .borrow_mut()
        .insert(local_button, local_text, None)
        .unwrap();

    let shared_button = renderer
        .borrow_mut()
        .create_element(ElementTag::new("Button"))
        .unwrap();
    renderer
        .borrow_mut()
        .insert(column, shared_button, None)
        .unwrap();
    let shared_text = renderer.borrow_mut().create_text("shared 0").unwrap();
    renderer
        .borrow_mut()
        .insert(shared_button, shared_text, None)
        .unwrap();

    let double_text = renderer.borrow_mut().create_text("local double 0").unwrap();
    renderer
        .borrow_mut()
        .insert(column, double_text, None)
        .unwrap();

    for (node, read) in [(local_text, local), (shared_text, shared)] {
        let renderer = Rc::clone(renderer);
        render_effect(
            &cx,
            move |cx| read.get(cx),
            move |_cx, value| {
                renderer
                    .borrow_mut()
                    .set_text(node, &format!("count {value}"))
                    .unwrap();
            },
        );
    }
    {
        let renderer = Rc::clone(renderer);
        render_effect(
            &cx,
            move |cx| local_double.get(cx),
            move |_cx, value| {
                renderer
                    .borrow_mut()
                    .set_text(double_text, &format!("double {value}"))
                    .unwrap();
            },
        );
    }

    let local_handle = handle.clone();
    renderer
        .borrow_mut()
        .listen(
            local_button,
            EventKind::Click,
            Box::new(move |_event| {
                let cx = local_handle.cx();
                set_local.update_with(&cx, |value| value + 1);
            }),
        )
        .unwrap();

    let shared_handle = handle.clone();
    renderer
        .borrow_mut()
        .listen(
            shared_button,
            EventKind::Click,
            Box::new(move |_event| {
                let cx = shared_handle.cx();
                bump_shared.update_with(&cx, |value| value + 1);
            }),
        )
        .unwrap();

    CounterNodes {
        local_text,
        shared_text,
        double_text,
        local_button,
        shared_button,
    }
}

#[test]
fn counter_renders_and_updates_only_the_changed_text() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let root = renderer
        .borrow_mut()
        .create_element(ElementTag::new("Column"))
        .unwrap();
    let (shared, bump_shared) = create_signal(&cx, 0_u32);

    let first = mount_counter(&runtime, &renderer, root, shared, bump_shared);
    let second = mount_counter(&runtime, &renderer, root, shared, bump_shared);

    assert_eq!(renderer.borrow().text_of(first.local_text), Some("count 0"));
    assert_eq!(
        renderer.borrow().text_of(first.shared_text),
        Some("count 0")
    );
    assert_eq!(
        renderer.borrow().text_of(first.double_text),
        Some("double 0")
    );
    assert_eq!(
        renderer.borrow().text_of(second.local_text),
        Some("count 0")
    );

    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().click(first.local_button);
    cx.flush().unwrap();

    assert_eq!(renderer.borrow().text_of(first.local_text), Some("count 1"));
    assert_eq!(
        renderer.borrow().text_of(first.double_text),
        Some("double 2")
    );
    assert_eq!(
        renderer.borrow().text_of(second.local_text),
        Some("count 0")
    );

    let changed: Vec<NodeId> = renderer
        .borrow()
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::SetText { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(changed, vec![first.local_text, first.double_text]);

    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().click(first.shared_button);
    cx.flush().unwrap();
    assert_eq!(
        renderer.borrow().text_of(first.shared_text),
        Some("count 1")
    );
    assert_eq!(
        renderer.borrow().text_of(second.shared_text),
        Some("count 1")
    );
    assert_eq!(renderer.borrow().text_of(first.local_text), Some("count 1"));
    assert_eq!(
        renderer.borrow().text_of(second.local_text),
        Some("count 0")
    );

    // Break the runtime -> renderer -> listener -> runtime cycle before drop.
    runtime.root_owner().dispose(&cx);
    drop(renderer);
}
