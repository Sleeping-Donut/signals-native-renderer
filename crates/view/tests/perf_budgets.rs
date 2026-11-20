//! Release-mode budget measurements. Run with scripts/perf-budgets.sh.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use reactive_core::{create_signal, Runtime};
use renderer_api::{Color, Length, ResolvedStyle, Size, TextStyle};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{keyed_list, text_fn, ViewTree};

fn row_style() -> ResolvedStyle {
    ResolvedStyle {
        size: Size {
            width: Length::Px(300.0),
            height: Length::Px(20.0),
        },
        text: TextStyle {
            color: Color::new(0.9, 0.9, 0.9, 1.0),
            font_size: 14.0,
            line_height: 18.0,
            weight: 400,
        },
        ..ResolvedStyle::default()
    }
}

#[test]
#[ignore = "release budget measurement; run scripts/perf-budgets.sh"]
fn perf_budgets() {
    const N: usize = 10_000;
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (order, set_order) = create_signal(&cx, (0..N).collect::<Vec<usize>>());
    let rows: Rc<RefCell<HashMap<usize, reactive_core::WriteSignal<i32>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let rows_for_list = Rc::clone(&rows);
    let view = keyed_list(
        move |cx| order.get(cx),
        |index| *index,
        move |cx, index| {
            let (read, write) = create_signal(cx, *index as i32);
            rows_for_list.borrow_mut().insert(*index, write);
            text_fn(move |cx| format!("row {}", read.get(cx))).style(row_style())
        },
    );

    tree.set_viewport(400.0, 800.0);
    let start = Instant::now();
    let root = tree.mount(&cx, view);
    tree.frame(&cx, 0);
    let mount_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = Instant::now();
    if let Some(write) = rows.borrow().get(&5_000) {
        write.set(&cx, 99_999);
    }
    tree.frame(&cx, 1);
    let update_ms = start.elapsed().as_secs_f64() * 1000.0;

    let row = tree.children(root)[5_000];
    let mut style = row_style();
    style.text.color = Color::new(1.0, 0.2, 0.2, 1.0);
    tree.set_style(row, style);
    let start = Instant::now();
    tree.frame(&cx, 2);
    let paint_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = Instant::now();
    set_order.set(&cx, (0..N).rev().collect());
    tree.frame(&cx, 3);
    let reorder_ms = start.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "perf budgets: mount+layout {mount_ms:.3} ms, single-row update {update_ms:.3} ms, paint-only {paint_ms:.3} ms, 10k reorder {reorder_ms:.3} ms"
    );

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}
