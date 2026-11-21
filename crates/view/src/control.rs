//! Logical control-flow views.
//!
//! These components group children without creating layout or paint nodes, so
//! placement stays with the surrounding renderer tree. Import the names the
//! `view!` macro emits:
//!
//! ```ignore
//! use view::control::{Loading, Repeat, Show};
//! ```
//!
//! Control flow has no `style` attribute. Wrap a control-flow view in an
//! element when its children need layout:
//!
//! ```ignore
//! <Row style={styles::grid()}>
//!     <Repeat count={10}>
//!         <Counter />
//!     </Repeat>
//! </Row>
//! ```

#![allow(non_snake_case)]

use std::rc::Rc;

use reactive_core::Cx;

use crate::{fragment, keyed_list, loading_auto, loading_when, repeat, show, view_slot, View};

/// Mount `content` when `when` is true, otherwise `fallback`.
pub fn Show<W, F, C>(when: W, fallback: F, content: C) -> View
where
    W: Fn(&Cx<'_>) -> bool + 'static,
    F: Fn(&Cx<'_>) -> View + 'static,
    C: Fn(&Cx<'_>) -> View + 'static,
{
    show(when, content, fallback)
}

/// Keyed list control flow.
pub fn For<T, K, E, G, R>(each: E, key: G, row: R) -> View
where
    T: 'static,
    K: Eq + std::hash::Hash + Clone + 'static,
    E: Fn(&Cx<'_>) -> Vec<T> + 'static,
    G: Fn(&T) -> K + 'static,
    R: for<'a> Fn(&Cx<'a>, &T) -> View + 'static,
{
    keyed_list(each, key, row)
}

/// Show `fallback` until every binding under `content` is ready.
pub fn Loading<F, C>(fallback: F, content: C) -> View
where
    F: Fn(&Cx<'_>) -> View + 'static,
    C: Fn(&Cx<'_>) -> View + 'static,
{
    loading_auto(fallback, content)
}

/// Show `fallback` until `ready` returns true.
pub fn LoadingWhen<R, F, C>(ready: R, fallback: F, content: C) -> View
where
    R: Fn(&Cx<'_>) -> bool + 'static,
    F: Fn(&Cx<'_>) -> View + 'static,
    C: Fn(&Cx<'_>) -> View + 'static,
{
    loading_when(ready, fallback, content)
}

/// Build `content` once per row, `count` rows in total.
pub fn Repeat<C, R>(count: C, content: R) -> View
where
    C: Fn(&Cx<'_>) -> usize + 'static,
    R: Fn(&Cx<'_>, usize) -> View + 'static,
{
    repeat(count, content)
}

type WhenFn = Rc<dyn for<'a, 'b> Fn(&'a Cx<'b>) -> bool>;
type ThenFn = Rc<dyn for<'a, 'b> Fn(&'a Cx<'b>) -> View>;

/// One branch of a [`Switch`].
pub struct Branch {
    when: WhenFn,
    then: ThenFn,
}

impl Branch {
    /// A branch taken when `when` is true.
    #[must_use]
    pub fn new<W, T>(when: W, then: T) -> Self
    where
        W: for<'a, 'b> Fn(&'a Cx<'b>) -> bool + 'static,
        T: for<'a, 'b> Fn(&'a Cx<'b>) -> View + 'static,
    {
        Self {
            when: Rc::new(when),
            then: Rc::new(then),
        }
    }
}

/// A branch of a [`Switch`].
pub fn Match<W, T>(when: W, then: T) -> Branch
where
    W: for<'a, 'b> Fn(&'a Cx<'b>) -> bool + 'static,
    T: for<'a, 'b> Fn(&'a Cx<'b>) -> View + 'static,
{
    Branch::new(when, then)
}

/// Mount the first branch whose `when` returns true.
///
/// The selected index is the slot key: changing which branch matches disposes
/// the previous branch and mounts the next one.
pub fn Switch(branches: Vec<Branch>) -> View {
    let branches = Rc::new(branches);
    let key = {
        let branches = Rc::clone(&branches);
        move |cx: &Cx<'_>| branches.iter().position(|branch| (branch.when)(cx))
    };
    let build = {
        let branches = Rc::clone(&branches);
        move |cx: &Cx<'_>| {
            branches
                .iter()
                .position(|branch| (branch.when)(cx))
                .map_or_else(|| fragment(Vec::new()), |index| (branches[index].then)(cx))
        }
    };
    view_slot(key, build)
}
