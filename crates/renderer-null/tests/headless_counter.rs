//! The counter reference app on the null renderer.
//!
//! This is the Phase 3 milestone: the reactive half and a renderer backend meet
//! with a deterministic operation log, without a native window or layout.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{
    create_memo, create_signal, render_effect, OwnerHandle, ReadSignal, Runtime, RuntimeHandle,
    WriteSignal,
};
use renderer_api::{ElementTag, EventKind, NodeId, Renderer};
use renderer_null::{NullRenderer, Op};

struct CounterNodes {
    owner: OwnerHandle,
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
    let root_cx = handle.cx();
    let owner = root_cx.create_owner();
    let nodes = root_cx.with_owner(owner, |cx| {
        let (local, set_local) = create_signal(cx, 0_u32);
        let local_double = create_memo(cx, move |cx, _| local.get(cx) * 2);

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

        for (node, label, read) in [
            (local_text, "local", local),
            (shared_text, "shared", shared),
        ] {
            let renderer = Rc::clone(renderer);
            render_effect(
                cx,
                move |cx| read.get(cx),
                move |_cx, value| {
                    renderer
                        .borrow_mut()
                        .set_text(node, &format!("{label} {value}"))
                        .unwrap();
                },
            );
        }
        {
            let renderer = Rc::clone(renderer);
            render_effect(
                cx,
                move |cx| local_double.get(cx),
                move |_cx, value| {
                    renderer
                        .borrow_mut()
                        .set_text(double_text, &format!("local double {value}"))
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

        (
            local_text,
            shared_text,
            double_text,
            local_button,
            shared_button,
        )
    });
    let (local_text, shared_text, double_text, local_button, shared_button) = nodes;
    CounterNodes {
        owner,
        local_text,
        shared_text,
        double_text,
        local_button,
        shared_button,
    }
}

fn setup() -> (
    RuntimeHandle,
    Rc<RefCell<NullRenderer>>,
    NodeId,
    ReadSignal<u32>,
    WriteSignal<u32>,
) {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let root = renderer
        .borrow_mut()
        .create_element(ElementTag::new("Column"))
        .unwrap();
    let (shared, bump_shared) = create_signal(&cx, 0_u32);
    (runtime, renderer, root, shared, bump_shared)
}

fn text_of(renderer: &Rc<RefCell<NullRenderer>>, id: NodeId) -> Option<String> {
    renderer.borrow().text_of(id).map(str::to_string)
}

fn assert_only_text_updates(renderer: &Rc<RefCell<NullRenderer>>, expected: &[NodeId]) {
    let mut changed = Vec::new();
    for op in renderer.borrow().ops() {
        match op {
            Op::SetText { id, .. } => changed.push(*id),
            other => panic!("unexpected structural operation after mount: {other:?}"),
        }
    }
    changed.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(changed, expected);
}

#[test]
fn counter_local_isolation() {
    let (runtime, renderer, root, shared, bump_shared) = setup();
    let cx = runtime.cx();
    let first = mount_counter(&runtime, &renderer, root, shared, bump_shared);
    let second = mount_counter(&runtime, &renderer, root, shared, bump_shared);

    assert_eq!(
        text_of(&renderer, first.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, first.double_text).as_deref(),
        Some("local double 0")
    );
    assert_eq!(
        text_of(&renderer, second.local_text).as_deref(),
        Some("local 0")
    );

    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().click(first.local_button);
    cx.flush().unwrap();

    assert_eq!(
        text_of(&renderer, first.local_text).as_deref(),
        Some("local 1")
    );
    assert_eq!(
        text_of(&renderer, first.double_text).as_deref(),
        Some("local double 2")
    );
    assert_eq!(
        text_of(&renderer, second.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, second.double_text).as_deref(),
        Some("local double 0")
    );
    assert_eq!(
        text_of(&renderer, first.shared_text).as_deref(),
        Some("shared 0")
    );
    assert_eq!(
        text_of(&renderer, second.shared_text).as_deref(),
        Some("shared 0")
    );
    assert_only_text_updates(&renderer, &[first.local_text, first.double_text]);

    runtime.root_owner().dispose(&cx);
    drop(renderer);
}

#[test]
fn counter_shared_propagation() {
    let (runtime, renderer, root, shared, bump_shared) = setup();
    let cx = runtime.cx();
    let first = mount_counter(&runtime, &renderer, root, shared, bump_shared);
    let second = mount_counter(&runtime, &renderer, root, shared, bump_shared);

    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().click(first.shared_button);
    cx.flush().unwrap();

    assert_eq!(
        text_of(&renderer, first.shared_text).as_deref(),
        Some("shared 1")
    );
    assert_eq!(
        text_of(&renderer, second.shared_text).as_deref(),
        Some("shared 1")
    );
    assert_eq!(
        text_of(&renderer, first.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, second.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, first.double_text).as_deref(),
        Some("local double 0")
    );
    assert_eq!(
        text_of(&renderer, second.double_text).as_deref(),
        Some("local double 0")
    );
    assert_only_text_updates(&renderer, &[first.shared_text, second.shared_text]);

    runtime.root_owner().dispose(&cx);
    drop(renderer);
}

#[test]
fn counter_no_rebuild_after_mount() {
    let (runtime, renderer, root, shared, bump_shared) = setup();
    let cx = runtime.cx();
    let first = mount_counter(&runtime, &renderer, root, shared, bump_shared);
    let second = mount_counter(&runtime, &renderer, root, shared, bump_shared);

    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().click(first.local_button);
    cx.flush().unwrap();
    renderer.borrow_mut().click(second.shared_button);
    cx.flush().unwrap();

    assert_only_text_updates(
        &renderer,
        &[
            first.local_text,
            first.double_text,
            first.shared_text,
            second.shared_text,
        ],
    );

    runtime.root_owner().dispose(&cx);
    drop(renderer);
}

#[test]
fn counter_two_mounts_survive_one_disposal() {
    let (runtime, renderer, root, shared, bump_shared) = setup();
    let cx = runtime.cx();
    let first = mount_counter(&runtime, &renderer, root, shared, bump_shared);
    let second = mount_counter(&runtime, &renderer, root, shared, bump_shared);

    first.owner.dispose(&cx);
    cx.flush().unwrap();

    assert_eq!(
        text_of(&renderer, second.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, second.shared_text).as_deref(),
        Some("shared 0")
    );

    renderer.borrow_mut().click(second.local_button);
    cx.flush().unwrap();
    renderer.borrow_mut().click(second.shared_button);
    cx.flush().unwrap();

    assert_eq!(
        text_of(&renderer, second.local_text).as_deref(),
        Some("local 1")
    );
    assert_eq!(
        text_of(&renderer, second.double_text).as_deref(),
        Some("local double 2")
    );
    assert_eq!(
        text_of(&renderer, second.shared_text).as_deref(),
        Some("shared 1")
    );
    // The disposed instance no longer receives updates.
    assert_eq!(
        text_of(&renderer, first.local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text_of(&renderer, first.double_text).as_deref(),
        Some("local double 0")
    );
    assert_eq!(
        text_of(&renderer, first.shared_text).as_deref(),
        Some("shared 0")
    );

    runtime.root_owner().dispose(&cx);
    drop(renderer);
}
