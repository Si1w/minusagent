//! Multi-agent team primitives.
//!
//! - [`manager`] — `TeammateManager` owns the team roster, message bus, and
//!   shutdown/plan request protocols.
//! - [`task`] — Task graph (`TaskManager`) and background task pool
//!   (`BackgroundManager`).
//! - [`mod@todo`] — Per-session todo list with `/todo` command + `TodoWrite` tool.
//! - [`worktree`] — Git worktrees that isolate teammate work.

pub mod manager;
pub mod task;
pub mod todo;
pub mod worktree;

pub use manager::{TeammateManager, TeammateStatus};
pub use task::{BackgroundManager, BackgroundStatus, TaskManager, TaskStatus};
pub use todo::{TodoItem, TodoManager, TodoWrite, append_reminder};
pub use worktree::{WorktreeManager, WorktreeStatus};
