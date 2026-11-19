use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime};
use renderer_null::{NullRenderer, Op, SharedNullRenderer};
use view::{button_text, column, keyed_list, show, text, text_fn, ViewTree};

type RowSignals = Rc<
    RefCell<
        HashMap<
            usize,
            (
                reactive_core::ReadSignal<i32>,
                reactive_core::WriteSignal<i32>,
            ),
        >,
    >,
>;

#[test]
fn builder_tree_binds_text_and_dispatches_clicks() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (count, set_count) = create_signal(&cx, 0_u32);

    let view = column(vec![
        text_fn(move |cx| format!("count {}", count.get(cx))),
        button_text("+", move |cx| {
            set_count.update_with(cx, |&value| value + 1);
        }),
    ]);

    let root = tree.mount(&cx, view);
    tree.frame(&cx, 0);
    let children = tree.children(root);
    let text_node = tree.renderer_id(children[0]).unwrap();
    let button = tree.renderer_id(children[1]).unwrap();
    assert_eq!(renderer.borrow().text_of(text_node), Some("count 0"));

    renderer.borrow_mut().click(button);
    tree.frame(&cx, 1);
    assert_eq!(renderer.borrow().text_of(text_node), Some("count 1"));

    renderer.borrow_mut().clear_ops();
    tree.frame(&cx, 2);
    assert!(
        renderer
            .borrow()
            .ops()
            .iter()
            .all(|op| matches!(op, Op::BeginFrame(_) | Op::EndFrame)),
        "an unchanged frame performs no structural or binding operations"
    );

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}

#[test]
#[cfg_attr(miri, ignore = "scale test; run natively")]
fn keyed_list_reorders_without_remounting_rows() {
    const N: usize = 10_000;

    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (order, set_order) = create_signal(&cx, (0..N).collect::<Vec<usize>>());
    let row_signals: RowSignals = Rc::new(RefCell::new(HashMap::new()));
    let row_signals_for_rows = Rc::clone(&row_signals);

    let view = keyed_list(
        move |cx| order.get(cx),
        |index| *index,
        move |cx, index| {
            let (read, write) = create_signal(cx, *index as i32);
            row_signals_for_rows
                .borrow_mut()
                .insert(*index, (read, write));
            text_fn(move |cx| format!("row {}", read.get(cx)))
        },
    );

    let root = tree.mount(&cx, view);
    tree.frame(&cx, 0);
    let root_renderer = tree.renderer_id(root).unwrap();
    assert_eq!(
        renderer.borrow().children_of(root_renderer).unwrap().len(),
        N
    );
    let before = renderer
        .borrow()
        .children_of(root_renderer)
        .unwrap()
        .to_vec();
    let focused_row = tree.children(root)[0];
    tree.set_focus(Some(focused_row));

    renderer.borrow_mut().clear_ops();
    set_order.set(&cx, (0..N).rev().collect());
    tree.frame(&cx, 1);

    let after = renderer
        .borrow()
        .children_of(root_renderer)
        .unwrap()
        .to_vec();
    assert_eq!(after, before.iter().rev().copied().collect::<Vec<_>>());
    assert!(
        renderer
            .borrow()
            .ops()
            .iter()
            .all(|op| !matches!(op, Op::SetText { .. })),
        "reorder must not rebind row text"
    );
    assert_eq!(
        tree.focused(),
        Some(focused_row),
        "focus identity survives reorder"
    );

    // Update one row's own signal: only that row's text changes.
    renderer.borrow_mut().clear_ops();
    let write = row_signals.borrow().get(&0).unwrap().1;
    write.set(&cx, 100);
    tree.frame(&cx, 2);
    let row_zero = tree.children(root)[N - 1];
    let row_text = tree.renderer_id(row_zero).unwrap();
    assert_eq!(renderer.borrow().text_of(row_text), Some("row 100"));
    let changed: usize = renderer
        .borrow()
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::SetText { .. }))
        .count();
    assert_eq!(changed, 1, "only the changed row rebinds");

    let row_one = tree.children(root)[N - 2];
    let row_one_text = tree.renderer_id(row_one).unwrap();
    assert_eq!(renderer.borrow().text_of(row_one_text), Some("row 1"));

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}

#[test]
fn show_slot_replaces_the_branch() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (flag, set_flag) = create_signal(&cx, true);

    let view = show(move |cx| flag.get(cx), |_cx| text("yes"), |_cx| text("no"));
    let root = tree.mount(&cx, view);
    tree.frame(&cx, 0);
    let branch = tree.children(root)[0];
    let text_node = tree.renderer_id(branch).unwrap();
    assert_eq!(renderer.borrow().text_of(text_node), Some("yes"));

    set_flag.set(&cx, false);
    tree.frame(&cx, 1);
    let branch = tree.children(root)[0];
    let text_node = tree.renderer_id(branch).unwrap();
    assert_eq!(renderer.borrow().text_of(text_node), Some("no"));

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}
