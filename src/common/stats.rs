use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn increment(&self) { self.0.fetch_add(1, Ordering::AcqRel); }

    pub fn load(&self) -> u64 { self.0.load(Ordering::Acquire) }
}
