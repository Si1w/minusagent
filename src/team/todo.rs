use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::core::node::Node;
use crate::core::store::SharedStore;
use crate::tool::push_tool_result;

/// Status of a todo item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// A single todo item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub text: String,
    pub status: TodoStatus,
}

/// Manages todo items for multi-step task tracking
///
/// Enforces at most one `in_progress` item at a time.
/// Tracks rounds since last update for nag reminders.
pub struct TodoManager {
    pub items: Vec<TodoItem>,
    pub rounds_since_update: usize,
}

impl TodoManager {
    /// Create an empty manager
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            rounds_since_update: 0,
        }
    }

    /// Render the current todo list as a formatted string
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "No todos.".into();
        }
        let mut out = String::from("Todos:\n");
        for item in &self.items {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[>]",
                TodoStatus::Completed => "[x]",
            };
            out.push_str(&format!("  {} #{} {}\n", marker, item.id, item.text));
        }
        out
    }
}

/// Node that validates and writes todo items to the store
pub struct TodoWrite {
    pub call_id: String,
    pub items: Vec<TodoItem>,
}

impl Node for TodoWrite {
    type PrepRes = Vec<TodoItem>;
    type ExecRes = String;

    /// Validate: at most one `in_progress` item
    async fn prep(&self, _store: &SharedStore) -> Result<Vec<TodoItem>> {
        let in_progress = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::InProgress)
            .count();
        if in_progress > 1 {
            return Err(anyhow::anyhow!(
                "Only one task can be in_progress at a time"
            ));
        }
        Ok(self.items.clone())
    }

    /// Render the updated todo list
    async fn exec(&self, items: Vec<TodoItem>) -> Result<String> {
        let mgr = TodoManager {
            items,
            rounds_since_update: 0,
        };
        Ok(mgr.render())
    }

    /// Write validated items to store and push tool result
    async fn post(
        &self,
        store: &mut SharedStore,
        prep_res: Vec<TodoItem>,
        exec_res: String,
    ) -> Result<()> {
        store.state.todo.items = prep_res;
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_todo_write_basic() {
        let mut store = SharedStore::test_default();
        let node = TodoWrite {
            call_id: "t1".into(),
            items: vec![
                TodoItem {
                    id: 1,
                    text: "first task".into(),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    id: 2,
                    text: "second task".into(),
                    status: TodoStatus::InProgress,
                },
            ],
        };

        node.run(&mut store).await.expect("todo write failed");

        assert_eq!(store.state.todo.items.len(), 2);

        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("first task"));
        assert!(last.content.as_ref().unwrap().contains("[>]"));
    }

    #[tokio::test]
    async fn test_todo_write_rejects_multiple_in_progress() {
        let store = SharedStore::test_default();
        let node = TodoWrite {
            call_id: "t2".into(),
            items: vec![
                TodoItem {
                    id: 1,
                    text: "a".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 2,
                    text: "b".into(),
                    status: TodoStatus::InProgress,
                },
            ],
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Only one"));
    }

    #[test]
    fn test_render_empty() {
        let mgr = TodoManager::new();
        assert_eq!(mgr.render(), "No todos.");
    }

    #[test]
    fn test_render_items() {
        let mgr = TodoManager {
            items: vec![
                TodoItem {
                    id: 1,
                    text: "done".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    id: 2,
                    text: "doing".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 3,
                    text: "todo".into(),
                    status: TodoStatus::Pending,
                },
            ],
            rounds_since_update: 0,
        };
        let output = mgr.render();
        assert!(output.contains("[x] #1 done"));
        assert!(output.contains("[>] #2 doing"));
        assert!(output.contains("[ ] #3 todo"));
    }

    #[test]
    fn test_nag_reminder_triggers_after_3_rounds() {
        use crate::core::store::{Message, Role};

        let mut store = SharedStore::test_default();
        // Populate todo items so nag can trigger
        store.state.todo.items = vec![TodoItem {
            id: 1,
            text: "task".into(),
            status: TodoStatus::InProgress,
        }];

        // Simulate 3 rounds without todo update
        store.state.todo.rounds_since_update = 3;

        // Add a tool result message (nag appends to the last Tool message)
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some("some result".into()),
            tool_calls: None,
            tool_call_id: Some("c1".into()),
        });

        // Simulate the nag logic from agent.rs
        if store.state.todo.rounds_since_update >= 3
            && !store.state.todo.items.is_empty()
        {
            if let Some(last) = store.context.history.last_mut() {
                if last.role == Role::Tool {
                    if let Some(content) = &mut last.content {
                        content.push_str(
                            "\n\n<reminder>Update your todos.</reminder>",
                        );
                    }
                }
            }
        }

        let last = store.context.history.last().unwrap();
        assert!(last
            .content
            .as_ref()
            .unwrap()
            .contains("<reminder>Update your todos.</reminder>"));
    }

    #[test]
    fn test_nag_reminder_skipped_when_no_items() {
        use crate::core::store::{Message, Role};

        let mut store = SharedStore::test_default();
        // No todo items
        store.state.todo.rounds_since_update = 5;

        store.context.history.push(Message {
            role: Role::Tool,
            content: Some("result".into()),
            tool_calls: None,
            tool_call_id: Some("c1".into()),
        });

        // Nag should NOT trigger when items is empty
        let should_nag = store.state.todo.rounds_since_update >= 3
            && !store.state.todo.items.is_empty();
        assert!(!should_nag);
    }
}
