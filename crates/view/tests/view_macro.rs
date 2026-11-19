use std::cell::RefCell;
use std::rc::Rc;

use macros::view;
use reactive_core::{create_signal, Runtime};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::ViewTree;

#[test]
fn view_macro_builds_a_counter() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (count, set_count) = create_signal(&cx, 0_u32);

    let root = tree.mount(
        &cx,
        view! {
            <Column>
                <Text>{format!("count {}", count.get(cx))}</Text>
                <Button on_click=move |cx| {
                    set_count.update_with(cx, |&value| value + 1);
                }>"+"</Button>
            </Column>
        },
    );
    tree.frame(&cx, 0);

    let children = tree.children(root);
    let text_node = tree.renderer_id(children[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(text_node), Some("count 0"));

    tree.click_node(children[1]);
    tree.frame(&cx, 1);
    assert_eq!(renderer.borrow().text_of(text_node), Some("count 1"));

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}
