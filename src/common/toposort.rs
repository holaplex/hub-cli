use std::{
    iter,
    sync::{
        atomic::{self, AtomicUsize},
        Arc,
    }, num::NonZeroUsize,
};

use dispose::{Disposable, Dispose};
use log::error;
use parking_lot::Mutex;

pub trait PendingFail {
    fn failed(self);
}

#[derive(Debug)]
struct Pending<T: PendingFail> {
    unmet_deps: AtomicUsize,
    failed_deps: AtomicUsize,
    node: Mutex<Option<T>>,
}

impl<T: PendingFail> Drop for Pending<T> {
    fn drop(&mut self) {
        let Self {
            unmet_deps,
            failed_deps,
            node,
        } = self;
        let unmet_deps = unmet_deps.get_mut();
        let failed_deps = failed_deps.get_mut();
        let node = node.get_mut();

        assert!(
            unmet_deps <= failed_deps,
            "Pending node dropped with unmet dependencies!",
        );

        if *failed_deps == 0 {
            assert!(
                node.is_none(),
                "Pending node with no failed dependencies did not yield!",
            );
        } else {
            node.take()
                .unwrap_or_else(|| unreachable!("Pending node with failed dependencies yielded!"))
                .failed();
        }
    }
}

#[derive(Debug)]
#[repr(transparent)]
#[must_use = "Dependency iterator must be exhausted"]
pub struct Dependencies<T: PendingFail>(iter::Take<iter::Repeat<Arc<Pending<T>>>>);

impl<T: PendingFail> Dependencies<T> {
    pub fn new(deps: NonZeroUsize, node: T) -> Self {
        let deps = deps.get();
        let this = Pending {
            unmet_deps: deps.into(),
            failed_deps: 0.into(),
            node: Some(node).into(),
        };

        Self(std::iter::repeat(Arc::new(this)).take(deps))
    }
}

impl<T: PendingFail> Iterator for Dependencies<T> {
    type Item = Dependency<T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|a| Dependency(Dependent(a).into()))
    }
}

impl<T: PendingFail> ExactSizeIterator for Dependencies<T> {
    fn len(&self) -> usize {
        // TODO: TrustedLen pls
        self.0.size_hint().0
    }
}

impl<T: PendingFail> Drop for Dependencies<T> {
    fn drop(&mut self) {
        assert!(
            self.0.next().is_none(),
            "Dependency iterator was not exhausted!",
        );
    }
}

#[derive(Debug)]
#[repr(transparent)]
struct Dependent<T: PendingFail>(Arc<Pending<T>>);

impl<T: PendingFail> Dispose for Dependent<T> {
    fn dispose(self) { self.0.failed_deps.fetch_add(1, atomic::Ordering::SeqCst); }
}

#[derive(Debug)]
#[repr(transparent)]
pub struct Dependency<T: PendingFail>(Disposable<Dependent<T>>);

impl<T: PendingFail> Dependency<T> {
    #[must_use]
    pub fn ok<U>(self, submit: impl FnOnce(T) -> U) -> Option<U> {
        let inner = unsafe { Disposable::leak(self.0) };

        match inner.0.unmet_deps.fetch_sub(1, atomic::Ordering::SeqCst) {
            0 => panic!("Too many calls to next_dep!"),
            1 => Some(submit(
                inner
                    .0
                    .node
                    .try_lock()
                    .and_then(|mut j| j.take())
                    .unwrap_or_else(|| {
                        unreachable!("Contended pending node - this should not happen!")
                    }),
            )),
            _ => None,
        }
    }
}
