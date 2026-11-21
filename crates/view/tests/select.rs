use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime};
use renderer_api::{Length, ResolvedStyle, Size, TextStyle};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{select, ViewTree};

#[test]
fn select_marks_and_updates_the_selected_row() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (items, _set_items) =
        create_signal(&cx, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    let (selected, set_selected) = create_signal(&cx, "a".to_string());
    let mut row = ResolvedStyle {
        size: Size {
            width: Length::Px(120.0),
            height: Length::Px(24.0),
        },
        text: TextStyle::default(),
        ..ResolvedStyle::default()
    };
    row.background = Some(renderer_api::Color::new(0.2, 0.2, 0.2, 1.0));

    let root = tree.mount(
        &cx,
        select(
            move |cx| items.get(cx),
            |item: &String| item.clone(),
            |item: &String| item.clone(),
            move |cx| selected.get(cx),
            move |cx, item: String| set_selected.set(cx, item),
            row,
            |_cx| false,
        ),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let list = tree.children(root)[0];
    let rows = tree.children(list);
    assert_eq!(rows.len(), 3);

    tree.click_node(rows[1]);
    cx.flush().unwrap();
    tree.frame(&cx, 1);
    assert_eq!(selected.get(&cx), "b");

    tree.unmount(&cx, root);
}
