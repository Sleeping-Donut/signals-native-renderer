//! Allocation-profile evidence for the "no per-frame tree rebuild" exit.
//!
//! This is one test in its own binary, so the global allocation counter is not
//! disturbed by other tests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use reactive_core::{create_signal, Runtime};
use renderer_null::{NullRenderer, Op, SharedNullRenderer};
use view::{column, keyed_list, text, text_fn, ViewTree};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::SeqCst);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn reset_allocs() {
    ALLOCS.store(0, Ordering::SeqCst);
}

fn allocs() -> usize {
    ALLOCS.load(Ordering::SeqCst)
}

#[test]
#[cfg_attr(
    miri,
    ignore = "allocation instrumentation is not meaningful under Miri"
)]
fn unchanged_frames_and_large_reorders_do_not_rebuild_the_tree() {
    const N: usize = 10_000;

    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (order, set_order) = create_signal(&cx, (0..N).collect::<Vec<usize>>());
    let list = tree.mount(
        &cx,
        keyed_list(
            move |cx| order.get(cx),
            |index| *index,
            |_cx, _index| text("row"),
        ),
    );

    // Warm the renderer's operation vector so frame hooks do not allocate.
    for frame in 0..4 {
        tree.frame(&cx, frame);
    }
    renderer.borrow_mut().clear_ops();
    renderer.borrow_mut().clear_frames();

    reset_allocs();
    tree.frame(&cx, 4);
    let no_op_allocs = allocs();
    assert_eq!(tree.last_frame_dirty(), 0);
    assert!(
        renderer
            .borrow()
            .ops()
            .iter()
            .all(|op| matches!(op, Op::BeginFrame(_) | Op::EndFrame)),
        "an unchanged frame must not touch the tree"
    );
    assert_eq!(no_op_allocs, 0, "an unchanged frame must not allocate");

    // Reorder 10k existing rows. Rows are moved, never re-created.
    renderer.borrow_mut().clear_ops();
    reset_allocs();
    set_order.set(&cx, (0..N).rev().collect());
    tree.frame(&cx, 5);
    let reorder_allocs = allocs();
    let creates = renderer
        .borrow()
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::CreateElement { .. } | Op::CreateText { .. }))
        .count();
    let text_sets = renderer
        .borrow()
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::SetText { .. }))
        .count();
    assert_eq!(creates, 0, "reorder must reuse every row");
    assert_eq!(text_sets, 0, "reorder must not rebind row text");
    assert!(
        reorder_allocs < 200,
        "10k reorder allocated {reorder_allocs} times; that suggests rebuilding"
    );
    assert!(tree.last_frame_dirty() > 0, "reorder invalidates structure");

    // A property binding update allocates only the new text.
    let (count, set_count) = create_signal(&cx, 0_u32);
    let text_root = tree.mount(
        &cx,
        column(vec![text_fn(move |cx| count.get(cx).to_string())]),
    );
    tree.frame(&cx, 6);
    renderer.borrow_mut().clear_ops();
    reset_allocs();
    set_count.set(&cx, 1_u32);
    tree.frame(&cx, 7);
    let update_allocs = allocs();
    let text_sets = renderer
        .borrow()
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::SetText { .. }))
        .count();
    assert_eq!(text_sets, 1);
    assert_eq!(
        tree.last_frame_dirty(),
        0,
        "a text binding update is not structural"
    );
    assert!(
        update_allocs < 32,
        "one text update allocated {update_allocs} times"
    );

    println!(
        "allocation profile: unchanged={no_op_allocs}, reorder={reorder_allocs}, text-update={update_allocs}"
    );

    tree.unmount(&cx, text_root);
    tree.unmount(&cx, list);
    drop(tree);
    drop(renderer);
}
