use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify, oneshot};

/// Boxed async task
type BoxTask = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;

/// Entry in a lane's FIFO queue
struct LaneEntry {
    task: BoxTask,
    done_tx: oneshot::Sender<anyhow::Result<()>>,
    generation: u64,
}

/// Internal state of a single lane
struct LaneInner {
    name: String,
    max_concurrency: usize,
    deque: VecDeque<LaneEntry>,
    active_count: usize,
    generation: u64,
}

type Lane = Arc<Mutex<LaneInner>>;

/// Lane status snapshot for display
pub struct LaneStats {
    pub name: String,
    pub active: usize,
    pub queued: usize,
    pub max_concurrency: usize,
    pub generation: u64,
}

/// Handle for waiting on an enqueued task's result
pub struct TaskHandle {
    rx: oneshot::Receiver<anyhow::Result<()>>,
}

impl TaskHandle {
    /// Wait for the task to complete
    ///
    /// # Errors
    ///
    /// Returns error if the task was cancelled or the task itself failed.
    pub async fn wait(self) -> anyhow::Result<()> {
        self.rx
            .await
            .map_err(|_| anyhow::anyhow!("task cancelled"))?
    }
}

/// Named-lane task scheduler with per-lane FIFO ordering and concurrency limits
///
/// Each lane has its own queue, concurrency limit, and generation counter.
/// Tasks within a lane execute in FIFO order. Different lanes run independently.
///
/// Lanes are created lazily on first use. The self-pumping design means no
/// external scheduler is needed: each task completion triggers the next dequeue.
///
/// Generation tracking prevents stale tasks (from before a `reset_all`) from
/// pumping the queue after completion.
pub struct CommandQueue {
    lanes: Mutex<HashMap<String, Lane>>,
    idle_notify: Arc<Notify>,
}

impl CommandQueue {
    pub fn new() -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            idle_notify: Arc::new(Notify::new()),
        }
    }

    /// Get or lazily create a lane by name
    async fn get_or_create(&self, name: &str, max_concurrency: usize) -> Lane {
        let mut lanes = self.lanes.lock().await;
        lanes
            .entry(name.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(LaneInner {
                    name: name.to_string(),
                    max_concurrency: max_concurrency.max(1),
                    deque: VecDeque::new(),
                    active_count: 0,
                    generation: 0,
                }))
            })
            .clone()
    }

    /// Enqueue a task to a named lane
    ///
    /// Returns a handle that can be awaited for the task's result.
    /// The lane is created on first use with `max_concurrency=1`.
    pub async fn enqueue<F>(&self, lane_name: &str, task: F) -> TaskHandle
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let (done_tx, done_rx) = oneshot::channel();
        let lane = self.get_or_create(lane_name, 1).await;

        let to_spawn = {
            let mut inner = lane.lock().await;
            let current_gen = inner.generation;
            inner.deque.push_back(LaneEntry {
                task: Box::pin(task),
                done_tx,
                generation: current_gen,
            });
            pump(&mut inner)
        };

        let idle = self.idle_notify.clone();
        for entry in to_spawn {
            spawn_entry(lane.clone(), entry, idle.clone());
        }

        TaskHandle { rx: done_rx }
    }

    /// Mark a lane as having one more active task (for external tracking)
    ///
    /// Use this when the caller manages execution but wants lane stats
    /// to reflect activity. Pair with `mark_done` when work completes.
    pub async fn mark_active(&self, lane_name: &str) {
        let lane = self.get_or_create(lane_name, 1).await;
        let mut inner = lane.lock().await;
        inner.active_count += 1;
    }

    /// Mark a lane as having one fewer active task, pump next if available
    pub async fn mark_done(&self, lane_name: &str) {
        let lane = self.get_or_create(lane_name, 1).await;
        let to_spawn = {
            let mut inner = lane.lock().await;
            inner.active_count = inner.active_count.saturating_sub(1);
            pump(&mut inner)
        };

        let idle = self.idle_notify.clone();
        for entry in to_spawn {
            spawn_entry(lane.clone(), entry, idle.clone());
        }
        self.idle_notify.notify_waiters();
    }

    /// Get stats for a specific lane
    pub async fn lane_stats(&self, name: &str) -> Option<LaneStats> {
        let lanes = self.lanes.lock().await;
        let lane = lanes.get(name)?;
        let inner = lane.lock().await;
        Some(stats_from(&inner))
    }

    /// Get stats for all lanes
    pub async fn all_stats(&self) -> Vec<LaneStats> {
        let lanes = self.lanes.lock().await;
        let mut stats = Vec::new();
        for lane in lanes.values() {
            let inner = lane.lock().await;
            stats.push(stats_from(&inner));
        }
        stats
    }

    /// Increment generation on all lanes
    ///
    /// Stale tasks (whose generation doesn't match the lane's current
    /// generation) will not re-pump the queue when they complete.
    pub async fn reset_all(&self) {
        let lanes = self.lanes.lock().await;
        for lane in lanes.values() {
            let mut inner = lane.lock().await;
            inner.generation += 1;
        }
    }

    /// Set max concurrency for a lane (creates lane if needed)
    pub async fn set_max_concurrency(&self, name: &str, max: usize) {
        let lane = self.get_or_create(name, max).await;
        let to_spawn = {
            let mut inner = lane.lock().await;
            inner.max_concurrency = max.max(1);
            pump(&mut inner)
        };

        let idle = self.idle_notify.clone();
        for entry in to_spawn {
            spawn_entry(lane.clone(), entry, idle.clone());
        }
    }

    /// Wait until all lanes are idle (no active or queued tasks)
    pub async fn wait_for_idle(&self) {
        loop {
            let all_idle = {
                let lanes = self.lanes.lock().await;
                let mut idle = true;
                for lane in lanes.values() {
                    let inner = lane.lock().await;
                    if inner.active_count > 0 || !inner.deque.is_empty() {
                        idle = false;
                        break;
                    }
                }
                idle
            };

            if all_idle {
                return;
            }

            self.idle_notify.notified().await;
        }
    }
}

fn stats_from(inner: &LaneInner) -> LaneStats {
    LaneStats {
        name: inner.name.clone(),
        active: inner.active_count,
        queued: inner.deque.len(),
        max_concurrency: inner.max_concurrency,
        generation: inner.generation,
    }
}

/// Pop tasks from queue while active_count < max_concurrency
fn pump(inner: &mut LaneInner) -> Vec<LaneEntry> {
    let mut to_spawn = Vec::new();
    while inner.active_count < inner.max_concurrency
        && !inner.deque.is_empty()
    {
        let entry = inner.deque.pop_front().unwrap();
        inner.active_count += 1;
        to_spawn.push(entry);
    }
    to_spawn
}

/// Spawn a single task, wiring up task_done → pump for self-pumping
fn spawn_entry(lane: Lane, entry: LaneEntry, idle_notify: Arc<Notify>) {
    tokio::spawn(async move {
        let generation = entry.generation;
        let result = entry.task.await;
        let _ = entry.done_tx.send(result);

        // task_done: decrement active, pump next if same generation
        let to_spawn = {
            let mut inner = lane.lock().await;
            inner.active_count -= 1;
            if generation == inner.generation {
                pump(&mut inner)
            } else {
                Vec::new() // stale task — do not pump
            }
        };

        for next in to_spawn {
            spawn_entry(lane.clone(), next, idle_notify.clone());
        }

        idle_notify.notify_waiters();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enqueue_and_wait() {
        let queue = CommandQueue::new();
        let handle = queue
            .enqueue("test", async { Ok(()) })
            .await;
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn test_enqueue_error_propagates() {
        let queue = CommandQueue::new();
        let handle = queue
            .enqueue("test", async {
                Err(anyhow::anyhow!("boom"))
            })
            .await;
        let result = handle.wait().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("boom"));
    }

    #[tokio::test]
    async fn test_fifo_ordering() {
        let queue = Arc::new(CommandQueue::new());
        let order = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for i in 0..5 {
            let o = order.clone();
            let h = queue
                .enqueue("serial", async move {
                    o.lock().await.push(i);
                    Ok(())
                })
                .await;
            handles.push(h);
        }

        for h in handles {
            h.wait().await.unwrap();
        }

        let result = order.lock().await;
        assert_eq!(*result, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_lane_stats() {
        let queue = CommandQueue::new();
        assert!(queue.lane_stats("missing").await.is_none());

        queue.mark_active("main").await;
        let stats = queue.lane_stats("main").await.unwrap();
        assert_eq!(stats.active, 1);
        assert_eq!(stats.queued, 0);

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

    #[tokio::test]
    async fn test_generation_stale_task_no_pump() {
        let queue = Arc::new(CommandQueue::new());
        let ran = Arc::new(Mutex::new(false));

        // Enqueue a slow task
        let r = ran.clone();
        let h1 = queue
            .enqueue("gen", async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                *r.lock().await = true;
                Ok(())
            })
            .await;

        // Enqueue a second task
        let r2 = ran.clone();
        let _h2 = queue
            .enqueue("gen", async move {
                // This should NOT run after reset
                *r2.lock().await = true;
                Ok(())
            })
            .await;

        // Reset generations before first task completes
        queue.reset_all().await;

        // First task still completes
        h1.wait().await.unwrap();

        // Give time for pump to NOT happen
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The second task should still be queued (not pumped by stale task)
        let stats = queue.lane_stats("gen").await.unwrap();
        assert_eq!(stats.queued, 1);
    }

    #[tokio::test]
    async fn test_set_max_concurrency() {
        let queue = Arc::new(CommandQueue::new());
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        queue.set_max_concurrency("parallel", 3).await;

        let mut handles = Vec::new();
        for _ in 0..6 {
            let a = active.clone();
            let m = max_seen.clone();
            let h = queue
                .enqueue("parallel", async move {
                    let cur = a.fetch_add(
                        1,
                        std::sync::atomic::Ordering::SeqCst,
                    ) + 1;
                    m.fetch_max(cur, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(
                        std::time::Duration::from_millis(20),
                    )
                    .await;
                    a.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .await;
            handles.push(h);
        }

        for h in handles {
            h.wait().await.unwrap();
        }

        let peak = max_seen.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak <= 3, "peak concurrency was {peak}, expected <= 3");
    }

    #[tokio::test]
    async fn test_wait_for_idle() {
        let queue = Arc::new(CommandQueue::new());

        let q = queue.clone();
        let h = queue
            .enqueue("work", async move {
                tokio::time::sleep(std::time::Duration::from_millis(30))
                    .await;
                // Verify stats show active during work
                let stats = q.lane_stats("work").await;
                assert!(stats.is_some());
                Ok(())
            })
            .await;

        // Should block until task completes
        let q2 = queue.clone();
        let idle_task = tokio::spawn(async move {
            q2.wait_for_idle().await;
        });

        h.wait().await.unwrap();
        // idle_task should complete shortly after
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            idle_task,
        )
        .await
        .expect("wait_for_idle should complete")
        .unwrap();
    }

    #[tokio::test]
    async fn test_independent_lanes() {
        let queue = Arc::new(CommandQueue::new());
        let order = Arc::new(Mutex::new(Vec::new()));

        let o1 = order.clone();
        let h1 = queue
            .enqueue("lane-a", async move {
                tokio::time::sleep(std::time::Duration::from_millis(30))
                    .await;
                o1.lock().await.push("a");
                Ok(())
            })
            .await;

        let o2 = order.clone();
        let h2 = queue
            .enqueue("lane-b", async move {
                o2.lock().await.push("b");
                Ok(())
            })
            .await;

        // lane-b should complete before lane-a (no dependency)
        h2.wait().await.unwrap();
        h1.wait().await.unwrap();

        let result = order.lock().await;
        assert_eq!(result[0], "b"); // b finishes first (no sleep)
    }
}
