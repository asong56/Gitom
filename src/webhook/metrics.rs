use std::sync::atomic::AtomicU64;

#[derive(Default)]
pub struct Metrics {
    pub enqueued:  AtomicU64,
    pub delivered: AtomicU64,
    pub failed:    AtomicU64,
    pub in_flight: AtomicU64,
}
