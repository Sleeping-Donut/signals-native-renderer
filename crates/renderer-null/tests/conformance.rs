use renderer_api::{
    AttrName, AttrValue, Color, Display, ElementTag, Event, EventKind, FrameInfo, Renderer,
    RendererError, ResolvedStyle,
};
use renderer_null::{NullRenderer, Op};

fn element(renderer: &mut NullRenderer, tag: &str) -> u64 {
    renderer.create_element(ElementTag::new(tag)).unwrap()
}

#[test]
fn structure_insert_remove_and_reinsert() {
    let mut renderer = NullRenderer::new();
    let root = element(&mut renderer, "Column");
    let a = renderer.create_text("a").unwrap();
    let b = renderer.create_text("b").unwrap();
    let c = renderer.create_text("c").unwrap();

    renderer.insert(root, c, None).unwrap();
    renderer.insert(root, a, Some(c)).unwrap();
    renderer.insert(root, b, Some(c)).unwrap();
    assert_eq!(renderer.children_of(root), Some(&[a, b, c][..]));

    renderer.remove(root, b).unwrap();
    assert_eq!(renderer.children_of(root), Some(&[a, c][..]));
    assert_eq!(renderer.children_of(b), Some(&[][..]));

    renderer.insert(root, b, None).unwrap();
    assert_eq!(renderer.children_of(root), Some(&[a, c, b][..]));
    assert_eq!(renderer.tag_of(root), Some("Column"));
}

#[test]
fn text_attributes_and_style_are_recorded() {
    let mut renderer = NullRenderer::new();
    let text = renderer.create_text("hello").unwrap();
    assert_eq!(renderer.text_of(text), Some("hello"));
    renderer.set_text(text, "world").unwrap();
    assert_eq!(renderer.text_of(text), Some("world"));
    renderer.set_text(text, "").unwrap();
    assert_eq!(renderer.text_of(text), Some(""));

    let button = element(&mut renderer, "Button");
    renderer
        .set_attr(button, AttrName::new("role"), AttrValue::new("button"))
        .unwrap();
    assert_eq!(renderer.attr_of(button, "role"), Some("button"));
    renderer
        .set_attr(button, AttrName::new("role"), AttrValue::new(""))
        .unwrap();
    assert_eq!(renderer.attr_of(button, "role"), Some(""));

    let style = ResolvedStyle {
        display: Display::Flex,
        background: Some(Color::new(0.1, 0.2, 0.3, 1.0)),
        ..ResolvedStyle::default()
    };
    renderer.set_style(button, &style).unwrap();
    assert_eq!(renderer.style_of(button), Some(&style));
}

#[test]
fn listeners_dispatch_and_unlisten() {
    let mut renderer = NullRenderer::new();
    let button = element(&mut renderer, "Button");
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let calls_for_listener = std::rc::Rc::clone(&calls);

    let listener = renderer
        .listen(
            button,
            EventKind::Click,
            Box::new(move |_event| calls_for_listener.set(calls_for_listener.get() + 1)),
        )
        .unwrap();
    assert_eq!(
        renderer.dispatch(button, Event::new(EventKind::Click, 1.0, 2.0)),
        1
    );
    assert_eq!(calls.get(), 1);

    renderer.unlisten(button, listener).unwrap();
    assert_eq!(
        renderer.dispatch(button, Event::new(EventKind::Click, 1.0, 2.0)),
        0
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(renderer.listener_count(), 0);
}

#[test]
fn errors_are_values_not_panics() {
    let mut renderer = NullRenderer::new();
    assert_eq!(
        renderer.set_text(999, "nope"),
        Err(RendererError::InvalidNode(999))
    );

    let root = element(&mut renderer, "Column");
    let child = renderer.create_text("x").unwrap();
    assert_eq!(renderer.remove(root, child), Err(RendererError::NotChild));

    let other = element(&mut renderer, "Row");
    assert_eq!(
        renderer.insert(root, child, Some(other)),
        Err(RendererError::NotChild)
    );

    renderer.shutdown();
    assert_eq!(renderer.create_text("after"), Err(RendererError::ShutDown));
}

#[test]
fn frame_hooks_are_recorded_once_per_frame() {
    let mut renderer = NullRenderer::new();
    renderer.begin_frame(&FrameInfo { frame: 7 }).unwrap();
    renderer.end_frame().unwrap();
    assert_eq!(renderer.frames(), &[FrameInfo { frame: 7 }]);
    assert!(matches!(
        renderer.ops(),
        [Op::BeginFrame(FrameInfo { frame: 7 }), Op::EndFrame]
    ));
}

#[test]
fn operation_log_is_deterministic() {
    let mut renderer = NullRenderer::new();
    let root = element(&mut renderer, "Column");
    let text = renderer.create_text("0").unwrap();
    renderer.insert(root, text, None).unwrap();
    renderer.set_text(text, "1").unwrap();

    let ops = renderer.take_ops();
    assert!(matches!(ops[0], Op::CreateElement { id, ref tag } if id == root && tag == "Column"));
    assert!(
        matches!(ops[1], Op::CreateText { id, text: ref content } if id == text && content == "0")
    );
    assert!(
        matches!(ops[2], Op::Insert { parent, child, anchor: None } if parent == root && child == text)
    );
    assert!(
        matches!(ops[3], Op::SetText { id, text: ref content } if id == text && content == "1")
    );
    assert!(renderer.ops().is_empty());
}
