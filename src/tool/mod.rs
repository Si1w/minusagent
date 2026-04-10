//! Tool registry, dispatch, and handlers.
//!
//! Tools are the primitives the agent invokes via LLM function calling.
//! This module centralises three concerns:
//!
//! - **Schemas** — JSON schemas advertised to the LLM
//!   (see [`all_tools_filtered`]).
//! - **Handlers** — Per-tool implementations dispatched by
//!   [`dispatch_tool`] from the agent loop.
//! - **Adapters** — Filesystem, web, and search building blocks shared
//!   across handlers.

mod exec;
mod handlers;
mod schema;
mod search;
mod web;

use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::engine::store::{LLMConfig, Message, Role, SharedStore};
use crate::frontend::Channel;

pub use schema::{ToolAvailability, ToolCapability, all_tools_filtered};

/// Tool definition for LLM function calling registration
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: ToolFunction,
}

/// Function schema within a tool definition
#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub(super) struct ToolContext<'a> {
    pub name: &'a str,
    pub call_id: &'a str,
    pub args: &'a Value,
    pub store: &'a mut SharedStore,
    pub channel: &'a Arc<dyn Channel>,
}

impl ToolContext<'_> {
    fn push_result(&mut self, content: impl Into<String>) {
        push_tool_result(self.store, self.call_id, content.into());
    }
}

pub(super) fn llm_config_for_agent(store: &SharedStore, agent_id: &str) -> LLMConfig {
    let mut llm_config = store.state.config.llm.clone();
    let model = store.state.agents.effective_model(agent_id);
    if !model.is_empty() {
        llm_config.model = model;
    }
    llm_config
}

/// Dispatch a tool call by name
///
/// # Arguments
///
/// * `name` - Tool name from LLM response
/// * `call_id` - Unique tool call ID
/// * `arguments` - JSON string of tool arguments
/// * `store` - Shared store for reading/writing context
/// * `channel` - Channel for user confirmation (bash only)
///
/// # Returns
///
/// `true` if the tool was found and executed, `false` if unknown.
///
/// # Errors
///
/// Returns error on argument parsing or tool execution failure.
pub async fn dispatch_tool(
    name: &str,
    call_id: String,
    arguments: &str,
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    let args: Value = serde_json::from_str(arguments)?;

    // Enforce tool policy: deny tools that are explicitly blocked
    if store.state.tool_policy.is_denied(name) {
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(format!(
                "Error: tool '{name}' is not allowed for this agent."
            )),
            tool_calls: None,
            tool_call_id: Some(call_id),
        });
        return Ok(true);
    }

    let mut ctx = ToolContext {
        name,
        call_id: &call_id,
        args: &args,
        store,
        channel,
    };
    handlers::handle_tool(&mut ctx).await
}

/// Push a tool result message into conversation history
pub fn push_tool_result(store: &mut SharedStore, call_id: &str, content: String) {
    store.context.history.push(Message {
        role: Role::Tool,
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::node::Node;
    use crate::tool::schema::all_tools;

    #[tokio::test]
    async fn test_bash_exec() {
        let mut store = SharedStore::test_default();
        let node = exec::BashExec {
            call_id: "call_123".into(),
            command: "echo hello".into(),
            sandbox: false,
            timeout_secs: None,
            current_dir: None,
        };

        let prep_res = node.prep(&store).await.expect("prep failed");
        let exec_res = node.exec(prep_res.clone()).await.expect("exec failed");
        assert_eq!(exec_res.trim(), "hello");

        node.post(&mut store, prep_res, exec_res)
            .await
            .expect("post failed");

        let last = store.context.history.last().expect("history empty");
        assert!(matches!(last.role, Role::Tool));
        assert_eq!(last.tool_call_id.as_deref(), Some("call_123"));
        assert!(last.content.as_ref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exec_dangerous() {
        let store = SharedStore::test_default();
        let node = exec::BashExec {
            call_id: "call_456".into(),
            command: "sudo rm -rf /".into(),
            sandbox: false,
            timeout_secs: None,
            current_dir: None,
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let mut store = SharedStore::test_default();
        let test_path = "test_rw_output.txt";

        let write_node = exec::WriteFile {
            call_id: "w1".into(),
            path: test_path.into(),
            content: "hello world".into(),
        };
        write_node.run(&mut store).await.expect("write failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("Written"));

        let read_node = exec::ReadFile {
            call_id: "r1".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.expect("read failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("hello world"));

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file() {
        let mut store = SharedStore::test_default();
        let test_path = "test_edit_output.txt";
        std::fs::write(test_path, "foo bar baz").unwrap();

        // Must read before edit
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.expect("read failed");

        let node = exec::EditFile {
            call_id: "e1".into(),
            path: test_path.into(),
            old_string: "bar".into(),
            new_string: "qux".into(),
        };
        node.run(&mut store).await.expect("edit failed");

        let content = std::fs::read_to_string(test_path).unwrap();
        assert_eq!(content, "foo qux baz");

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_string_not_found() {
        let test_path = "test_edit_notfound.txt";
        std::fs::write(test_path, "some content").unwrap();

        let mut store = SharedStore::test_default();
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.unwrap();

        let node = exec::EditFile {
            call_id: "e2".into(),
            path: test_path.into(),
            old_string: "nonexistent".into(),
            new_string: "replacement".into(),
        };
        let result = node.run(&mut store).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_not_unique() {
        let test_path = "test_edit_dup.txt";
        std::fs::write(test_path, "aaa aaa").unwrap();

        let mut store = SharedStore::test_default();
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.unwrap();

        let node = exec::EditFile {
            call_id: "e3".into(),
            path: test_path.into(),
            old_string: "aaa".into(),
            new_string: "bbb".into(),
        };
        let result = node.run(&mut store).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[test]
    fn test_safe_path_blocks_traversal() {
        let result = exec::safe_path("../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_safe_path_allows_relative() {
        std::fs::write("test_safe_path.txt", "").unwrap();
        let result = exec::safe_path("test_safe_path.txt");
        assert!(result.is_ok());
        std::fs::remove_file("test_safe_path.txt").ok();
    }

    #[tokio::test]
    async fn test_task_rejects_unknown_agent() {
        let mut store = SharedStore::test_default();
        let args = r#"{"prompt":"do something","agent":"nonexistent"}"#;
        let result = dispatch_tool("task", "t1".into(), args, &mut store, &silent())
            .await
            .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        let content = last.content.as_ref().unwrap();
        assert!(content.contains("Error: unknown agent"));
        assert!(content.contains("Available:"));
    }

    #[tokio::test]
    async fn test_task_blocked_in_subagent() {
        let mut store = SharedStore::test_default();
        store.state.is_subagent = true;

        let args = r#"{"prompt":"do something","agent":"any"}"#;
        let result = dispatch_tool("task", "t1".into(), args, &mut store, &silent())
            .await
            .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        assert!(
            last.content
                .as_ref()
                .unwrap()
                .contains("not available here")
        );
    }

    #[test]
    fn test_all_tools_excludes_task_for_subagent() {
        let parent_tools = all_tools(ToolAvailability::default());
        let sub_tools = all_tools(ToolAvailability::subagent());

        assert!(parent_tools.iter().any(|t| t.function.name == "task"));
        assert!(!sub_tools.iter().any(|t| t.function.name == "task"));
        assert!(parent_tools.iter().any(|t| t.function.name == "bash"));
        assert!(sub_tools.iter().any(|t| t.function.name == "bash"));
    }

    #[test]
    fn test_all_tools_includes_task_graph_tools() {
        let without = all_tools(ToolAvailability::default());
        let with = all_tools(ToolAvailability::default().with(ToolCapability::Tasks));

        assert!(!without.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_update"));
        assert!(with.iter().any(|t| t.function.name == "task_list"));
        assert!(with.iter().any(|t| t.function.name == "task_get"));
    }

    #[test]
    fn test_all_tools_includes_team_tools() {
        let without = all_tools(ToolAvailability::default());
        let with = all_tools(ToolAvailability::default().with(ToolCapability::Team));
        let sub_with = all_tools(ToolAvailability::subagent().with(ToolCapability::Team));

        assert!(!without.iter().any(|t| t.function.name == "team_spawn"));
        assert!(with.iter().any(|t| t.function.name == "team_spawn"));
        assert!(with.iter().any(|t| t.function.name == "team_send"));
        assert!(with.iter().any(|t| t.function.name == "team_read_inbox"));
        assert!(with.iter().any(|t| t.function.name == "shutdown_request"));
        assert!(with.iter().any(|t| t.function.name == "plan_response"));
        assert!(!sub_with.iter().any(|t| t.function.name == "team_spawn"));
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_request")
        );
        assert!(!sub_with.iter().any(|t| t.function.name == "plan_response"));
        assert!(sub_with.iter().any(|t| t.function.name == "team_send"));
        assert!(
            sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_response")
        );
        assert!(sub_with.iter().any(|t| t.function.name == "plan_submit"));
    }

    fn silent() -> Arc<dyn Channel> {
        Arc::new(crate::frontend::SilentChannel)
    }
}
