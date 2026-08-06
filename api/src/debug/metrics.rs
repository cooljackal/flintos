// SPDX-License-Identifier: Apache-2.0

//! Named metrics: counters and gauges.
//!
//! `Counter` is a monotonically increasing named value
//! (e.g. "packets_rx", "spi_errors").
//!
//! `Gauge` is a named value that can go up and down
//! (e.g. "heap_free", "queue_depth").
//!
//! # Example
//!
//! ```ignore
//! use api::debug::metrics::Counter;
//!
//! static ERRORS: Counter = Counter::new("spi_timeouts");
//! ERRORS.increment();
//! assert!(ERRORS.read() >= 1);
//! ```

use core::sync::atomic::{AtomicU32, Ordering};

/// A named counter metric.
pub struct Counter {
    name: &'static str,
    value: AtomicU32,
}

impl Counter {
    /// Define a named counter.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            value: AtomicU32::new(0),
        }
    }

    /// Increment by 1.
    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by `n`.
    pub fn increment_by(&self, n: u32) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Read the current value.
    pub fn read(&self) -> u32 {
        self.value.load(Ordering::Relaxed)
    }

    /// Return the name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// A named gauge metric.
pub struct Gauge {
    name: &'static str,
    value: AtomicU32,
}

impl Gauge {
    /// Define a named gauge.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            value: AtomicU32::new(0),
        }
    }

    /// Set the gauge value.
    pub fn set(&self, val: u32) {
        self.value.store(val, Ordering::Relaxed);
    }

    /// Read the current value.
    pub fn read(&self) -> u32 {
        self.value.load(Ordering::Relaxed)
    }

    /// Return the name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_increment() {
        let c = Counter::new("test");
        assert_eq!(c.read(), 0);
        c.increment();
        assert_eq!(c.read(), 1);
        c.increment_by(5);
        assert_eq!(c.read(), 6);
    }

    #[test]
    fn counter_name() {
        let c = Counter::new("errors");
        assert_eq!(c.name(), "errors");
    }

    #[test]
    fn gauge_set_read() {
        let g = Gauge::new("temp");
        assert_eq!(g.read(), 0);
        g.set(85);
        assert_eq!(g.read(), 85);
    }

    #[test]
    fn gauge_name() {
        let g = Gauge::new("voltage");
        assert_eq!(g.name(), "voltage");
    }
}
