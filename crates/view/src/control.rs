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

use std::hash::Hash;
use std::marker::PhantomData;
use std::rc::Rc;

use reactive_core::Cx;

use crate::{
    fragment, keyed_list_rows, loading_auto, loading_when, repeat, show, view_slot, ForIndex,
    ForItem, RowFallback, RowKey, View,
};

/// Mount `content` when `when` is true, otherwise `fallback`.
pub fn Show<W, F, C>(when: W, fallback: F, content: C) -> View
where
    W: Fn(&Cx<'_>) -> bool + 'static,
    F: Fn(&Cx<'_>) -> View + 'static,
    C: Fn(&Cx<'_>) -> View + 'static,
{
    show(when, content, fallback)
}

/// List control flow with reactive item and index accessors.
///
/// `For(each)` starts positional; `.keyed(key)` uses stable keys, `.fallback`
/// mounts an empty-state view, and `.render` builds one row:
///
/// ```ignore
/// For(move |cx| todos.get(cx))
///     .keyed(|todo: &Todo| todo.id)
///     .fallback(|cx| view! { <Empty /> })
///     .render(move |cx, todo, index| view! {
///         <TodoRow todo={todo} index={index} />
///     })
/// ```
pub fn For<E, T>(each: E) -> ForBuilder<E, T, usize>
where
    E: Fn(&Cx<'_>) -> Vec<T> + 'static,
    T: 'static,
{
    ForBuilder {
        each,
        key: Box::new(|index, _| index),
        fallback: None,
        _t: PhantomData,
    }
}

/// The builder for a [`For`] list.
pub struct ForBuilder<E, T, K> {
    each: E,
    key: RowKey<T, K>,
    fallback: RowFallback,
    _t: PhantomData<fn() -> T>,
}

impl<E, T, K> ForBuilder<E, T, K> {
    /// Key rows by a stable value. Without this, rows are positional.
    #[must_use]
    pub fn keyed<G, K2>(self, key: G) -> ForBuilder<E, T, K2>
    where
        G: Fn(&T) -> K2 + 'static,
        K2: 'static,
    {
        ForBuilder {
            each: self.each,
            key: Box::new(move |_, item| key(item)),
            fallback: self.fallback,
            _t: PhantomData,
        }
    }

    /// Mount this view when the list is empty.
    #[must_use]
    pub fn fallback<F>(mut self, fallback: F) -> Self
    where
        F: Fn(&Cx<'_>) -> View + 'static,
    {
        self.fallback = Some(Box::new(fallback));
        self
    }

    /// Build one row. `item` and `index` are reactive accessors.
    pub fn render<R>(self, row: R) -> View
    where
        E: Fn(&Cx<'_>) -> Vec<T> + 'static,
        T: Clone + PartialEq + 'static,
        K: Eq + Hash + Clone + 'static,
        R: for<'a> Fn(&Cx<'a>, ForItem<T>, ForIndex) -> View + 'static,
    {
        keyed_list_rows(self.each, self.key, self.fallback, row)
    }
}

/// Show `fallback` until every binding under `content` is ready.
pub fn Loading<F, C>(fallback: F, content: C) -> View
where
    F: FnOnce(&Cx<'_>) -> View + 'static,
    C: FnOnce(&Cx<'_>) -> View + 'static,
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
