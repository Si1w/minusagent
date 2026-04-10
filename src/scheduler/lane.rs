use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

/// Internal state of a single lane
struct LaneInner {
    name: String,
    active_count: usize,
}

type Lane = Arc<Mutex<LaneInner>>;

/// Lane status snapshot for display
pub struct LaneStats {
    pub name: String,
    pub active: usize,
}

/// Named-lane activity tracker with per-lane counters
///
/// Tracks how many tasks are active in each lane. Lanes are created lazily
/// on first use. Use `mark_active` / `mark_done` to update counters.
pub struct CommandQueue {
    lanes: Mutex<HashMap<String, Lane>>,
}

impl CommandQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
        }
    }

    /// Get or lazily create a lane by name
    async fn get_or_create(&self, name: &str) -> Lane {
        let mut lanes = self.lanes.lock().await;
        lanes
            .entry(name.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(LaneInner {
                    name: name.to_string(),
                    active_count: 0,
                }))
            })
            .clone()
    }

    /// Mark a lane as having one more active task
    ///
    /// Pair with `mark_done` when work completes.
    pub async fn mark_active(&self, lane_name: &str) {
        let lane = self.get_or_create(lane_name).await;
        let mut inner = lane.lock().await;
        inner.active_count += 1;
    }

    /// Mark a lane as having one fewer active task
    pub async fn mark_done(&self, lane_name: &str) {
        let lane = self.get_or_create(lane_name).await;
        let mut inner = lane.lock().await;
        inner.active_count = inner.active_count.saturating_sub(1);
    }

    /// Get stats for a specific lane
    pub async fn lane_stats(&self, name: &str) -> Option<LaneStats> {
        let lanes = self.lanes.lock().await;
        let lane = lanes.get(name)?;
        let inner = lane.lock().await;
        Some(LaneStats {
            name: inner.name.clone(),
            active: inner.active_count,
        })
    }

    /// Get stats for all lanes
    pub async fn all_stats(&self) -> Vec<LaneStats> {
        let lanes = self.lanes.lock().await;
        let mut stats = Vec::new();
        for lane in lanes.values() {
            let inner = lane.lock().await;
            stats.push(LaneStats {
                name: inner.name.clone(),
                active: inner.active_count,
            });
        }
        stats
    }
}

impl Default for CommandQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lane_stats() {
        let queue = CommandQueue::new();
        assert!(queue.lane_stats("missing").await.is_none());

        queue.mark_active("main").await;
        let stats = queue.lane_stats("main").await.unwrap();
        assert_eq!(stats.active, 1);

        queue.mark_done("main").await;
        let stats = queue.lane_stats("main").await.unwrap();
        assert_eq!(stats.active, 0);
    }

    #[tokio::test]
    async fn test_mark_done_saturates_at_zero() {
        let queue = CommandQueue::new();
        queue.mark_done("empty").await;
        let stats = queue.lane_stats("empty").await.unwrap();
        assert_eq!(stats.active, 0);
    }

    #[tokio::test]
    async fn test_all_stats() {
        let queue = CommandQueue::new();
        queue.mark_active("a").await;
        queue.mark_active("b").await;

        let stats = queue.all_stats().await;
        assert_eq!(stats.len(), 2);

        queue.mark_done("a").await;
        queue.mark_done("b").await;
    }
}
