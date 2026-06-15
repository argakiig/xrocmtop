//! Fixed-capacity time-series buffers for the history graphs (T6).
//!
//! [`History<T>`] keeps the most recent `capacity` samples in insertion order, dropping the
//! oldest as new ones arrive. The App pushes one sample per metric per tick; the graph widgets
//! read the buffer oldest→newest. A `capacity` of 0 is treated as 1 so a buffer always holds at
//! least the latest value.

use std::collections::VecDeque;

/// A rolling window of the last `capacity` samples, oldest first.
#[derive(Debug, Clone)]
pub struct History<T> {
    buf: VecDeque<T>,
    capacity: usize,
}

// `len`/`is_empty`/`capacity`/`to_vec` round out the buffer's natural interface and are exercised
// by the unit tests below; allow them to exist without a non-test caller.
#[allow(dead_code)]
impl<T: Copy> History<T> {
    /// Create an empty buffer holding up to `capacity` samples (minimum 1).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Append a sample, evicting the oldest if at capacity.
    pub fn push(&mut self, value: T) {
        if self.buf.len() == self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    /// Number of samples currently retained.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Maximum number of samples retained.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The most recent sample, if any.
    pub fn latest(&self) -> Option<T> {
        self.buf.back().copied()
    }

    /// Iterate samples oldest→newest.
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.buf.iter().copied()
    }

    /// Collect samples oldest→newest into a `Vec` (for widgets needing a slice).
    pub fn to_vec(&self) -> Vec<T> {
        self.buf.iter().copied().collect()
    }
}

/// Per-GPU rolling time series for the three graphed metrics (utilization, power, temperature).
///
/// One instance per enumerated GPU lives in the [`App`](crate::app::App); the app pushes one
/// sample per metric on each tick. Each metric keeps its own [`History`] so a missing sample in
/// one series never desynchronizes the others.
#[derive(Debug, Clone)]
pub struct GpuHistory {
    /// Utilization (busy) percentage, 0..=100.
    pub util: History<f64>,
    /// Socket/package power draw in watts.
    pub power: History<f64>,
    /// Edge temperature in degrees Celsius.
    pub temp: History<f64>,
    /// NPU (XDNA/IPU) activity percentage, 0..=100. Stays empty on parts without an NPU, which is
    /// the signal the graphs panel uses to omit the NPU sparkline entirely.
    pub npu_util: History<f64>,
}

impl GpuHistory {
    /// Create an empty history with the given window `capacity` for every metric.
    pub fn new(capacity: usize) -> Self {
        Self {
            util: History::new(capacity),
            power: History::new(capacity),
            temp: History::new(capacity),
            npu_util: History::new(capacity),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        let h: History<f64> = History::new(4);
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert_eq!(h.latest(), None);
        assert_eq!(h.capacity(), 4);
    }

    #[test]
    fn fills_then_evicts_oldest() {
        let mut h = History::new(3);
        h.push(1);
        h.push(2);
        h.push(3);
        assert_eq!(h.to_vec(), vec![1, 2, 3]);
        assert_eq!(h.len(), 3);
        h.push(4); // evicts 1
        assert_eq!(h.to_vec(), vec![2, 3, 4]);
        assert_eq!(h.len(), 3);
        assert_eq!(h.latest(), Some(4));
    }

    #[test]
    fn preserves_order_oldest_first() {
        let mut h = History::new(100);
        for i in 0..5 {
            h.push(i);
        }
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn zero_capacity_clamped_to_one() {
        let mut h = History::new(0);
        assert_eq!(h.capacity(), 1);
        h.push(10);
        h.push(20);
        assert_eq!(h.to_vec(), vec![20]); // only the latest survives
    }
}
