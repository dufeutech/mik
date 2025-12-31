//! Load balancing selection algorithms.
//!
//! Implements various algorithms for selecting which backend should handle
//! an incoming request. Only healthy backends are considered for selection.
//!
//! # Key Types
//!
//! - [`Selection`] - Trait defining the selection algorithm interface
//! - [`RoundRobin`] - Distributes requests evenly in circular fashion
//! - [`WeightedRoundRobin`] - Distributes requests proportionally to backend weights

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

/// Weighted round-robin load balancing algorithm.
///
/// Distributes requests across backends proportionally to their weights.
/// A backend with weight 2 will receive twice as many requests as a backend
/// with weight 1.
///
/// # Algorithm
///
/// Uses an expanded index approach where each backend appears in the rotation
/// a number of times equal to its weight. For example, with backends:
/// - Backend 0: weight 2
/// - Backend 1: weight 1
/// - Backend 2: weight 3
///
/// The virtual rotation is: [0, 0, 1, 2, 2, 2] and we cycle through it.
/// This ensures smooth distribution without bursts.
///
/// # Example
///
/// ```ignore
/// use mik::runtime::lb::WeightedRoundRobin;
///
/// // Backend 0 gets 2x traffic, backend 1 gets 1x traffic
/// let wrr = WeightedRoundRobin::new(vec![2, 1]);
/// ```
#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct WeightedRoundRobin {
    /// Weights for each backend (index matches backend index).
    weights: Vec<u32>,
    /// Current position in the weighted rotation.
    current: AtomicUsize,
    /// Total weight sum (for wrapping).
    total_weight: usize,
}

impl WeightedRoundRobin {
    /// Create a new weighted round-robin selector with the given weights.
    ///
    /// Each weight corresponds to a backend at the same index.
    /// Weights of 0 are treated as 1.
    #[allow(dead_code)]
    pub(super) fn new(weights: Vec<u32>) -> Self {
        let weights: Vec<u32> = weights.into_iter().map(|w| w.max(1)).collect();
        let total_weight: usize = weights.iter().map(|&w| w as usize).sum();

        Self {
            weights,
            current: AtomicUsize::new(0),
            total_weight,
        }
    }

    /// Compute expanded indices for the given weights and backend indices.
    ///
    /// Creates a virtual index list where each backend appears proportionally
    /// to its weight.
    fn compute_expanded_indices(weights: &[u32], indices: &[usize]) -> Vec<usize> {
        let mut expanded = Vec::new();
        for (i, &idx) in indices.iter().enumerate() {
            let weight = weights.get(idx).copied().unwrap_or(1) as usize;
            for _ in 0..weight {
                expanded.push(i); // Push the position in the healthy_indices array
            }
        }
        expanded
    }

    /// Get the next weighted index.
    fn next_weighted_index(&self, healthy_expanded: &[usize]) -> usize {
        if healthy_expanded.is_empty() {
            return 0;
        }
        let pos = self.current.fetch_add(1, Ordering::Relaxed);
        pos % healthy_expanded.len()
    }

    /// Get the weight for a specific backend index.
    #[allow(dead_code)]
    pub(super) fn weight(&self, index: usize) -> u32 {
        self.weights.get(index).copied().unwrap_or(1)
    }

    /// Get the total weight of all backends.
    #[allow(dead_code)]
    pub(super) const fn total_weight(&self) -> usize {
        self.total_weight
    }
}

impl Selection for WeightedRoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }

        // Build expanded indices for healthy backends only
        let healthy_expanded = Self::compute_expanded_indices(&self.weights, healthy_indices);

        if healthy_expanded.is_empty() {
            return None;
        }

        // Get next position in the weighted rotation
        let pos = self.next_weighted_index(&healthy_expanded);

        // Map back to the original backend index
        let healthy_pos = healthy_expanded[pos];
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

    // ============ WeightedRoundRobin Tests ============

    #[test]
    fn test_weighted_round_robin_basic() {
        // Backend 0: weight 2, Backend 1: weight 1
        // Expected ratio: 2:1 (backend 0 gets 2x traffic)
        let wrr = WeightedRoundRobin::new(vec![2, 1]);
        let healthy = vec![0, 1];

        let mut counts = [0usize; 2];
        // Run enough iterations to see the pattern
        for _ in 0..300 {
            let idx = wrr.select(&healthy).unwrap();
            counts[idx] += 1;
        }

        // Backend 0 should have ~2x the selections of backend 1
        // With weights [2, 1], total weight is 3
        // Backend 0 should get 2/3 of traffic, backend 1 should get 1/3
        // In 300 requests: backend 0 ~200, backend 1 ~100
        assert!(
            counts[0] > counts[1],
            "Backend 0 (weight 2) should have more selections than backend 1 (weight 1): {counts:?}"
        );

        // Allow some variance but the ratio should be roughly 2:1
        let ratio = counts[0] as f64 / counts[1] as f64;
        assert!(
            (1.5..2.5).contains(&ratio),
            "Expected ratio ~2.0, got {ratio}: {counts:?}"
        );
    }

    #[test]
    fn test_weighted_round_robin_equal_weights() {
        // All backends have equal weight - should behave like regular round-robin
        let wrr = WeightedRoundRobin::new(vec![1, 1, 1]);
        let healthy = vec![0, 1, 2];

        let mut counts = [0usize; 3];
        for _ in 0..300 {
            let idx = wrr.select(&healthy).unwrap();
            counts[idx] += 1;
        }

        // Each backend should get roughly equal traffic
        for (i, &count) in counts.iter().enumerate() {
            assert!(
                (80..=120).contains(&count),
                "Backend {i} should have ~100 selections, got {count}"
            );
        }
    }

    #[test]
    fn test_weighted_round_robin_with_unhealthy() {
        // Backend 0: weight 3, Backend 1: weight 2, Backend 2: weight 1
        let wrr = WeightedRoundRobin::new(vec![3, 2, 1]);

        // Only backends 0 and 2 are healthy
        let healthy = vec![0, 2];

        let mut counts = [0usize; 3];
        for _ in 0..400 {
            let idx = wrr.select(&healthy).unwrap();
            counts[idx] += 1;
        }

        // Backend 1 should have 0 selections (unhealthy)
        assert_eq!(counts[1], 0, "Unhealthy backend should not be selected");

        // Backend 0 (weight 3) should have 3x the selections of backend 2 (weight 1)
        let ratio = counts[0] as f64 / counts[2] as f64;
        assert!(
            (2.5..3.5).contains(&ratio),
            "Expected ratio ~3.0, got {ratio}: {counts:?}"
        );
    }

    #[test]
    fn test_weighted_round_robin_empty() {
        let wrr = WeightedRoundRobin::new(vec![1, 2, 3]);
        let healthy: Vec<usize> = vec![];

        assert!(wrr.select(&healthy).is_none());
    }

    #[test]
    fn test_weighted_round_robin_single() {
        let wrr = WeightedRoundRobin::new(vec![5]);
        let healthy = vec![0];

        // Should always return 0
        for _ in 0..10 {
            assert_eq!(wrr.select(&healthy), Some(0));
        }
    }

    #[test]
    fn test_weighted_round_robin_zero_weight_treated_as_one() {
        // Weight of 0 should be treated as 1
        let wrr = WeightedRoundRobin::new(vec![0, 1]);

        assert_eq!(wrr.weight(0), 1); // 0 becomes 1
        assert_eq!(wrr.weight(1), 1);
        assert_eq!(wrr.total_weight(), 2);
    }

    #[test]
    fn test_weighted_round_robin_high_weights() {
        // Test with higher weights
        // Backend 0: weight 10, Backend 1: weight 1
        let wrr = WeightedRoundRobin::new(vec![10, 1]);
        let healthy = vec![0, 1];

        let mut counts = [0usize; 2];
        for _ in 0..1100 {
            let idx = wrr.select(&healthy).unwrap();
            counts[idx] += 1;
        }

        // Backend 0 should get ~10x traffic
        let ratio = counts[0] as f64 / counts[1] as f64;
        assert!(
            (8.0..12.0).contains(&ratio),
            "Expected ratio ~10.0, got {ratio}: {counts:?}"
        );
    }

    #[test]
    fn test_weighted_round_robin_three_backends() {
        // Backend 0: weight 1, Backend 1: weight 2, Backend 2: weight 3
        // Total weight: 6
        // Expected distribution: 1/6, 2/6, 3/6
        let wrr = WeightedRoundRobin::new(vec![1, 2, 3]);
        let healthy = vec![0, 1, 2];

        let mut counts = [0usize; 3];
        for _ in 0..600 {
            let idx = wrr.select(&healthy).unwrap();
            counts[idx] += 1;
        }

        // Backend 0: ~100, Backend 1: ~200, Backend 2: ~300
        assert!(
            counts[0] < counts[1] && counts[1] < counts[2],
            "Counts should increase with weights: {counts:?}"
        );

        // Check approximate ratios
        let ratio_1_0 = counts[1] as f64 / counts[0] as f64;
        let ratio_2_0 = counts[2] as f64 / counts[0] as f64;

        assert!(
            (1.5..2.5).contains(&ratio_1_0),
            "Expected ratio 1:0 ~2.0, got {ratio_1_0}"
        );
        assert!(
            (2.5..3.5).contains(&ratio_2_0),
            "Expected ratio 2:0 ~3.0, got {ratio_2_0}"
        );
    }

    #[test]
    fn test_weighted_round_robin_weight_accessor() {
        let wrr = WeightedRoundRobin::new(vec![5, 3, 7]);

        assert_eq!(wrr.weight(0), 5);
        assert_eq!(wrr.weight(1), 3);
        assert_eq!(wrr.weight(2), 7);
        assert_eq!(wrr.weight(99), 1); // Out of bounds returns default 1
        assert_eq!(wrr.total_weight(), 15);
    }
}
