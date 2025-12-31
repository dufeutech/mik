//! Load balancing selection algorithm.
//!
//! Implements round-robin selection for distributing requests across
//! healthy backends.
//!
//! # Key Types
//!
//! - [`Selection`] - Trait defining the selection algorithm interface
//! - [`RoundRobin`] - Distributes requests evenly in circular fashion

use std::sync::atomic::{AtomicUsize, Ordering};

/// Trait for load balancing selection algorithms.
pub(super) trait Selection: Send + Sync {
    /// Select the next backend index from the available backends.
    fn select(&self, healthy_indices: &[usize]) -> Option<usize>;
}

/// Round-robin load balancing algorithm.
///
/// Distributes requests evenly across all healthy backends in a circular fashion.
#[derive(Debug)]
pub struct RoundRobin {
    /// Current position in the rotation.
    current: AtomicUsize,
    /// Total number of backends (used for modulo).
    total: usize,
}

impl RoundRobin {
    /// Create a new round-robin selector for the given number of backends.
    pub const fn new(total: usize) -> Self {
        Self {
            current: AtomicUsize::new(0),
            total,
        }
    }

    /// Get the next index, wrapping around.
    fn next_index(&self) -> usize {
        if self.total == 0 {
            return 0;
        }
        self.current.fetch_add(1, Ordering::Relaxed) % self.total
    }
}

impl Selection for RoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }

        // Get the next position and map it to a healthy backend
        let pos = self.next_index();
        let healthy_pos = pos % healthy_indices.len();
        Some(healthy_indices[healthy_pos])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin_basic() {
        let rr = RoundRobin::new(3);
        let healthy = vec![0, 1, 2];

        // Should cycle through all backends
        let mut selections = Vec::new();
        for _ in 0..6 {
            selections.push(rr.select(&healthy).unwrap());
        }

        // Each backend should be selected twice in 6 iterations
        assert_eq!(selections.iter().filter(|&&x| x == 0).count(), 2);
        assert_eq!(selections.iter().filter(|&&x| x == 1).count(), 2);
        assert_eq!(selections.iter().filter(|&&x| x == 2).count(), 2);
    }

    #[test]
    fn test_round_robin_with_unhealthy() {
        let rr = RoundRobin::new(3);
        // Only backends 0 and 2 are healthy
        let healthy = vec![0, 2];

        let mut selections = Vec::new();
        for _ in 0..4 {
            selections.push(rr.select(&healthy).unwrap());
        }

        // Should only select from healthy backends
        for &idx in &selections {
            assert!(idx == 0 || idx == 2);
        }
    }

    #[test]
    fn test_round_robin_empty() {
        let rr = RoundRobin::new(3);
        let healthy: Vec<usize> = vec![];

        assert!(rr.select(&healthy).is_none());
    }

    #[test]
    fn test_round_robin_single() {
        let rr = RoundRobin::new(1);
        let healthy = vec![0];

        // Should always return 0
        for _ in 0..5 {
            assert_eq!(rr.select(&healthy), Some(0));
        }
    }
}
