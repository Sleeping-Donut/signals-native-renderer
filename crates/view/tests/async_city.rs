//! Async states-and-cities wiring: first load, pending refetch, derived reset.

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::rc::Rc;
use std::task::Poll;

use reactive_core::{
    create_async_memo, create_derived_signal, create_memo, is_pending, loading_boundary,
    AsyncError, Runtime, RuntimeHandle,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{column, text, text_fn, view_slot, ViewTree};

type Fut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, AsyncError>>>>;

fn delayed<T: Clone + 'static>(value: T, ready_at: usize) -> Fut<T> {
    let polls = Rc::new(Cell::new(0));
    Box::pin(poll_fn(move |_| {
        let count = polls.get();
        polls.set(count + 1);
        if count < ready_at {
            Poll::Pending
        } else {
            Poll::Ready(Ok(value.clone()))
        }
    }))
}

struct Harness {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    set_state: reactive_core::WriteSignal<String>,
    set_city: reactive_core::WriteSignal<String>,
    cities: reactive_core::Memo<Vec<String>>,
    frames: u64,
}

fn cities_for(state: &str) -> Vec<String> {
    match state {
        "California" => vec!["Los Angeles", "San Francisco", "San Diego"],
        "Florida" => vec!["Miami", "Orlando", "Tampa"],
        "Texas" => vec!["Houston", "Dallas", "Austin"],
        _ => Vec::new(),
    }
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn setup() -> Harness {
    let runtime = Runtime::new().into_handle();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let cx = runtime.cx();
    let states = create_async_memo(&cx, |_cx, _: Option<&Vec<String>>| {
        delayed(
            vec![
                "California".to_string(),
                "Florida".to_string(),
                "Texas".to_string(),
            ],
            2,
        )
    });
    let (state, set_state) = create_derived_signal(&cx, move |cx, _| {
        states.get(cx).first().cloned().unwrap_or_default()
    });
    let cities = create_async_memo(&cx, move |cx, _: Option<&Vec<String>>| {
        let name = state.get(cx);
        delayed(cities_for(&name), 2)
    });
    let (city, set_city) = create_derived_signal(&cx, move |cx, _| {
        cities.get(cx).first().cloned().unwrap_or_default()
    });
    let selection = create_memo(&cx, move |cx, _previous: Option<&String>| {
        format!(
            "{}|{}|{}|{}",
            states.get(cx).len(),
            state.get(cx),
            cities.get(cx).len(),
            city.get(cx)
        )
    });
    let ready = loading_boundary(
        &cx,
        None,
        move |cx| {
            let _ = states.get(cx);
            let _ = cities.get(cx);
            true
        },
        |_| false,
    );
    let app = view_slot(
        move |cx| ready.get(cx),
        move |cx| {
            if ready.get(cx) {
                let cities = cities;
                column(vec![text_fn(move |cx| selection.get(cx))])
                    .disabled_when(move |cx| is_pending(cx, move |cx| cities.get(cx)))
            } else {
                text("Loading")
            }
        },
    );
    let root = tree.mount(&cx, app);
    Harness {
        runtime,
        tree,
        root,
        set_state,
        set_city,
        cities,
        frames: 0,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

fn scene_text(tree: &ViewTree) -> String {
    tree.build_scene()
        .ops()
        .iter()
        .filter_map(|primitive| match primitive {
            scene::Primitive::Text { layout, .. } => Some(
                layout
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn settle(harness: &mut Harness, mut done: impl FnMut(&ViewTree) -> bool) {
    let cx = harness.runtime.cx();
    for _ in 0..40 {
        harness.runtime.pump();
        cx.flush().unwrap();
        harness.frames += 1;
        harness.tree.frame(&cx, harness.frames);
        if done(&harness.tree) {
            return;
        }
    }
    panic!("async state did not settle: {}", scene_text(&harness.tree));
}

#[test]
fn async_city_selection_waits_pends_and_resets() {
    let mut harness = setup();
    harness.tree.set_viewport(400.0, 200.0);
    {
        let cx = harness.runtime.cx();
        harness.tree.frame(&cx, 0);
    }
    assert!(scene_text(&harness.tree).contains("Loading"));

    settle(&mut harness, |tree| {
        scene_text(tree).contains("3|California|3|Los Angeles")
    });

    {
        let cx = harness.runtime.cx();
        harness.set_state.set(&cx, "Florida".to_string());
        cx.flush().unwrap();
        harness.runtime.pump();
        cx.flush().unwrap();
        let cities = harness.cities;
        assert!(is_pending(&cx, move |cx| cities.get(cx)));
        harness.frames += 1;
        harness.tree.frame(&cx, harness.frames);
        let content = harness.tree.children(harness.root)[0];
        assert!(harness.tree.is_disabled(content));
    }
    let pending = scene_text(&harness.tree);
    assert!(
        pending.contains("3|California|3|Los Angeles"),
        "selection holds the committed state and city while Florida loads: {pending}"
    );

    settle(&mut harness, |tree| {
        scene_text(tree).contains("3|Florida|3|Miami")
    });
    {
        let content = harness.tree.children(harness.root)[0];
        assert!(!harness.tree.is_disabled(content));
    }

    {
        let cx = harness.runtime.cx();
        harness.set_city.set(&cx, "Orlando".to_string());
        cx.flush().unwrap();
        harness.frames += 1;
        harness.tree.frame(&cx, harness.frames);
    }
    assert!(scene_text(&harness.tree).contains("Florida|3|Orlando"));

    {
        let cx = harness.runtime.cx();
        harness.set_state.set(&cx, "Texas".to_string());
        cx.flush().unwrap();
    }
    settle(&mut harness, |tree| {
        scene_text(tree).contains("3|Texas|3|Houston")
    });
}

#[test]
fn async_city_supersession_drops_the_old_flight() {
    let mut harness = setup();
    harness.tree.set_viewport(400.0, 200.0);
    settle(&mut harness, |tree| scene_text(tree).contains("California"));

    {
        let cx = harness.runtime.cx();
        harness.set_state.set(&cx, "Florida".to_string());
        cx.flush().unwrap();
        harness.runtime.pump();
        harness.set_state.set(&cx, "Texas".to_string());
        cx.flush().unwrap();
    }
    settle(&mut harness, |tree| {
        scene_text(tree).contains("3|Texas|3|Houston")
    });
    assert!(!scene_text(&harness.tree).contains("Miami"));
}
