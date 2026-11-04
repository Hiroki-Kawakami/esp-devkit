/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::rc::{Rc, Weak};
use alloc::sync::Arc;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::marker::PhantomData;
use core::ptr;

use devkit::executor::Park;
use idf::executor::{CRITICAL, JoinHandle, LocalExecutor};
use idf::log::error;

use crate::app::{Link, wake};
use crate::component::{Component, Cx};
use crate::tree::Tree;

struct UiPark {
    link: Link,
    clock: Arc<dyn Park>,
}

impl Park for UiPark {
    fn park(&self, _: Option<u32>) {
        unreachable!("the UI task waits on its event queue");
    }

    fn unpark(&self) {
        wake(&self.link);
    }

    fn now(&self) -> u32 {
        self.clock.now()
    }
}

/// The UI task's executor, and the tree its futures reach while it runs them.
pub(crate) struct Local {
    pub(crate) executor: LocalExecutor,
    tree: Cell<*mut Tree>,
}

impl Local {
    pub(crate) fn new(link: Link) -> Rc<Self> {
        let park = Arc::new(UiPark { link, clock: idf::executor::park() });
        Rc::new(Self { executor: LocalExecutor::new(&CRITICAL, park), tree: Cell::new(ptr::null_mut()) })
    }

    /// The ticks to the next timer.
    pub(crate) fn run(&self, tree: *mut Tree) -> Option<u32> {
        self.tree.set(tree);
        let wait = self.executor.run_ready();
        self.tree.set(ptr::null_mut());
        wait
    }
}

/// A component, as its futures reach it.
pub struct This<C> {
    id: u32,
    local: Weak<Local>,
    _component: PhantomData<fn(C)>,
}

impl<C> Clone for This<C> {
    fn clone(&self) -> Self {
        Self { id: self.id, local: self.local.clone(), _component: PhantomData }
    }
}

impl<C: Component> This<C> {
    pub(crate) fn new(id: u32, local: &Rc<Local>) -> Self {
        Self { id, local: Rc::downgrade(local), _component: PhantomData }
    }

    /// Runs `f` on the component as `update` would, shown or covered, and
    /// rebuilds it. `None` once it has gone, and inside another `with`.
    pub fn with<R>(&self, f: impl FnOnce(&mut C, &mut Cx<C>) -> R) -> Option<R> {
        let local = self.local.upgrade()?;
        let tree = local.tree.replace(ptr::null_mut());
        if tree.is_null() {
            error!(c"ui", "with: inside another with");
            return None;
        }
        let mut f = Some(f);
        let mut out = None;
        unsafe { &mut *tree }.with_component::<C>(self.id, &mut |component, cx| {
            if let Some(f) = f.take() {
                out = Some(f(component, cx));
            }
        });
        local.tree.set(tree);
        out
    }
}

/// A future `Cx::spawn` started. It runs as long as its component lives,
/// unless aborted; dropping this leaves it running.
#[derive(Clone)]
pub struct Task(Rc<RefCell<Option<JoinHandle<()>>>>);

impl Task {
    pub(crate) fn new(handle: JoinHandle<()>) -> Self {
        Self(Rc::new(RefCell::new(Some(handle))))
    }

    /// The future is dropped before it runs again.
    pub fn abort(&self) {
        let handle = self.0.borrow_mut().take();
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.0.borrow().as_ref().is_none_or(JoinHandle::is_finished)
    }
}

/// How a node's task makes its future, kept while the node lives.
pub(crate) trait TaskStart {
    fn start(&self, id: u32, local: &Rc<Local>) -> JoinHandle<()>;
}

struct Start<C, F>(F, PhantomData<fn(C)>);

impl<C, F, Fut> TaskStart for Start<C, F>
where
    C: Component,
    F: Fn(This<C>) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    fn start(&self, id: u32, local: &Rc<Local>) -> JoinHandle<()> {
        local.executor.spawn_local((self.0)(This::new(id, local)))
    }
}

/// A node's task as the view gave it; the reconciler takes it out once.
pub(crate) trait TaskSpec {
    fn take(&self) -> Option<Box<dyn TaskStart>>;
}

pub(crate) struct TaskElement<C, F>(Cell<Option<F>>, PhantomData<fn(C)>);

impl<C, F> TaskElement<C, F> {
    pub(crate) fn new(start: F) -> Self {
        Self(Cell::new(Some(start)), PhantomData)
    }
}

impl<C, F, Fut> TaskSpec for TaskElement<C, F>
where
    C: Component,
    F: Fn(This<C>) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
{
    fn take(&self) -> Option<Box<dyn TaskStart>> {
        let start = self.0.take()?;
        Some(Box::new(Start::<C, F>(start, PhantomData)))
    }
}
