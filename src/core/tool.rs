use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct BashExec {
    pub call_id: String,
    pub command: String,
}

impl Node for BashExec {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        let dangerous = ["rm -rf /", "sudo", "shutdown", "reboot", "> /dev/"];
        for pattern in dangerous {
            if self.command.contains(pattern) {
                return Err(anyhow::anyhow!(
                    "command blocked: contains dangerous pattern '{}'",
                    pattern
                ));
            }
        }
        Ok(self.command.clone())
    }

    async fn exec(&self, command: String) -> Result<String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            Ok(format!("stdout:\n{stdout}\nstderr:\n{stderr}"))
        }
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: String,
        exec_res: String,
    ) -> Result<()> {
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(exec_res),
            tool_calls: None,
            tool_call_id: Some(self.call_id.clone()),
        });
        Ok(())
    }
}

pub fn bash_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "bash".into(),
            description: "Run a shell command.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    }
                },
                "required": ["command"]
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::{Config, Context, LlmConfig, SystemState};

    fn empty_store() -> SharedStore {
        SharedStore {
            context: Context {
                system_prompt: String::new(),
                history: Vec::new(),
            },
            state: SystemState {
                config: Config {
                    llm: LlmConfig {
                        model: String::new(),
                        base_url: String::new(),
                        api_key: String::new(),
                    },
                },
            },
        }
    }

    #[tokio::test]
    async fn test_bash_exec() {
        let mut store = empty_store();
        let node = BashExec {
            call_id: "call_123".into(),
            command: "echo hello".into(),
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
        let store = empty_store();
        let node = BashExec {
            call_id: "call_456".into(),
            command: "sudo rm -rf /".into(),
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
    }
}
