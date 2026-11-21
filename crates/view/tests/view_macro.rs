use std::cell::RefCell;
use std::rc::Rc;

use macros::{component, view, view_builder};
use std::future::poll_fn;
use std::task::Poll;

use reactive_core::{create_async_memo, memo, signal, Runtime};
use renderer_api::ResolvedStyle;
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{Prop, View, ViewTree};

#[test]
fn view_macro_builds_a_counter() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (count, set_count) = signal!(&cx, 0_u32);
    let double = memo!(&cx, move |cx, _| count.get(cx) * 2);

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
    assert_eq!(double.get(&cx), 0);

    tree.click_node(children[1]);
    tree.frame(&cx, 1);
    assert_eq!(renderer.borrow().text_of(text_node), Some("count 1"));
    assert_eq!(double.get(&cx), 2);

    tree.unmount(&cx, root);
    drop(tree);
    drop(renderer);
}

#[test]
fn view_macro_attributes_and_for() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (items, _set_items) = signal!(&cx, vec!["one".to_string(), "two".to_string()]);

    let root = tree.mount(
        &cx,
        view! {
            <Column style={ResolvedStyle::default()}>
                <Text style={ResolvedStyle::default()}>{format!("n={}", items.get(cx).len())}</Text>
                <For
                    each={move |cx| items.get(cx)}
                    key={|item: &String| item.clone()}
                    children={move |_cx, item| {
                        let item = item.clone();
                        view! { <Text style={ResolvedStyle::default()}>{item.clone()}</Text> }
                    }}
                />
            </Column>
        },
    );
    tree.frame(&cx, 0);

    let children = tree.children(root);
    assert_eq!(children.len(), 2);
    let text_node = tree.renderer_id(children[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(text_node), Some("n=2"));
    assert_eq!(tree.children(children[1]).len(), 2);

    tree.unmount(&cx, root);
}

#[test]
fn view_macro_loading_switches_from_fallback_to_content() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (ready, set_ready) = signal!(&cx, false);

    let root = tree.mount(
        &cx,
        view! {
            <Loading
                ready={move |cx| ready.get(cx)}
                fallback={|_cx| view! { <Text>"loading"</Text> }}
            >
                <Text>"content"</Text>
            </Loading>
        },
    );
    tree.frame(&cx, 0);
    let fallback = tree.renderer_id(tree.children(root)[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(fallback), Some("loading"));

    set_ready.set(&cx, true);
    cx.flush().unwrap();
    tree.frame(&cx, 1);
    let content = tree.renderer_id(tree.children(root)[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(content), Some("content"));

    tree.unmount(&cx, root);
}

#[test]
fn view_macro_show_switches_branches() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (flag, set_flag) = signal!(&cx, false);
    let root = tree.mount(
        &cx,
        view! {
            <Show when={move |cx| flag.get(cx)} fallback={|_cx| view! { <Text>"no"</Text> }}>
                <Text>"yes"</Text>
            </Show>
        },
    );
    tree.frame(&cx, 0);
    let node = tree.renderer_id(tree.children(root)[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(node), Some("no"));
    set_flag.set(&cx, true);
    cx.flush().unwrap();
    tree.frame(&cx, 1);
    let node = tree.renderer_id(tree.children(root)[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(node), Some("yes"));
    tree.unmount(&cx, root);
}

#[test]
fn view_macro_loading_detects_pending_content_without_ready() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let data = create_async_memo(&cx, |_cx, _: Option<&String>| {
        Box::pin(poll_fn(|_| Poll::Ready(Ok("loaded".to_string() as String))))
    });

    let root = tree.mount(
        &cx,
        view! {
            <Loading fallback={|_cx| view! { <Text>"loading"</Text> }}>
                <Text>{data.get(cx)}</Text>
            </Loading>
        },
    );
    tree.frame(&cx, 0);
    let fallback = tree.children(root)[1];
    let node = tree.renderer_id(fallback).unwrap();
    assert_eq!(renderer.borrow().text_of(node), Some("loading"));

    for frame in 1..8 {
        runtime.pump();
        cx.flush().unwrap();
        tree.frame(&cx, frame);
    }
    let content = tree.children(root)[0];
    let node = tree.renderer_id(content).unwrap();
    assert_eq!(renderer.borrow().text_of(node), Some("loaded"));
    tree.unmount(&cx, root);
}

#[derive(macros::Props, Default)]
struct BadgeProps {
    #[prop(into)]
    label: Prop<String>,
}

#[component]
fn Badge(cx: &reactive_core::Cx<'_>, props: BadgeProps) -> View {
    let _ = cx;
    let label = props.label;
    view::text_fn(move |cx| label.get(cx))
}

#[test]
fn view_macro_component_and_props() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let root = tree.mount(
        &cx,
        view! {
            <Column>
                <Badge label={String::from("badge")} />
            </Column>
        },
    );
    tree.frame(&cx, 0);
    let badge = tree.children(root)[0];
    let text = tree.renderer_id(badge).unwrap();
    assert_eq!(renderer.borrow().text_of(text), Some("badge"));
    tree.unmount(&cx, root);
}

#[derive(macros::Props)]
struct PanelProps {
    children: View,
}

#[component]
fn Panel(_cx: &reactive_core::Cx<'_>, props: PanelProps) -> View {
    props.children
}

#[test]
fn view_macro_component_children() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let root = tree.mount(
        &cx,
        view! {
            <Panel>
                <Text>"inside"</Text>
            </Panel>
        },
    );
    tree.frame(&cx, 0);
    let child = tree.children(root)[0];
    let text = tree.renderer_id(child).unwrap();
    assert_eq!(renderer.borrow().text_of(text), Some("inside"));
    tree.unmount(&cx, root);
}

#[test]
fn view_builder_builds_a_counter() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let (count, set_count) = signal!(&cx, 0_u32);
    let root = tree.mount(
        &cx,
        view_builder! {
            Column
                .child(Text.value(format!("count {}", count.get(cx))))
                .child(
                    Button
                        .on_click(move |cx| set_count.update_with(cx, |value| value + 1))
                        .child(Text.value("+"))
                )
        },
    );
    tree.frame(&cx, 0);
    let children = tree.children(root);
    let text = tree.renderer_id(children[0]).unwrap();
    assert_eq!(renderer.borrow().text_of(text), Some("count 0"));
    tree.click_node(children[1]);
    tree.frame(&cx, 1);
    assert_eq!(renderer.borrow().text_of(text), Some("count 1"));
    tree.unmount(&cx, root);
}
