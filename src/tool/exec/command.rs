use std::path::PathBuf;

use anyhow::Result;
use tokio::process::Command;
use tokio::time::Duration;

use crate::config::tuning;
use crate::engine::node::Node;
use crate::engine::store::SharedStore;
use crate::tool::push_tool_result;

/// Check if a command contains dangerous patterns
pub fn is_dangerous_command(command: &str) -> bool {
    const DANGEROUS: &[&str] = &["rm -rf /", "sudo", "shutdown", "reboot", "> /dev/"];
    DANGEROUS.iter().any(|pattern| command.contains(pattern))
}

/// macOS sandbox-exec policy: allow most operations, deny writes to system paths
const SANDBOX_POLICY: &str = r#"(version 1)
(allow default)
(deny file-write*
    (subpath "/System")
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/Library"))
(deny process-exec
    (subpath "/System")
    (subpath "/usr/sbin"))
"#;

#[derive(Clone)]
pub struct CommandSpec {
    command: String,
    sandbox: bool,
    timeout_secs: u64,
    current_dir: Option<PathBuf>,
}

/// Execute a shell command and capture output
pub struct BashExec {
    pub call_id: String,
    pub command: String,
    /// Run inside macOS sandbox-exec (default: true from config)
    pub sandbox: bool,
    /// Override timeout in seconds (None = use config default)
    pub timeout_secs: Option<u64>,
    /// Working directory override (None = inherit current dir)
    pub current_dir: Option<PathBuf>,
}

impl Node for BashExec {
    type PrepRes = CommandSpec;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<CommandSpec> {
        if is_dangerous_command(&self.command) {
            return Err(anyhow::anyhow!(
                "command blocked: contains dangerous pattern"
            ));
        }

        Ok(CommandSpec {
            command: self.command.clone(),
            sandbox: self.sandbox,
            timeout_secs: self
                .timeout_secs
                .unwrap_or(tuning().timeouts.bash_timeout_secs),
            current_dir: self.current_dir.clone(),
        })
    }

    async fn exec(&self, spec: CommandSpec) -> Result<String> {
        let output = tokio::time::timeout(
            Duration::from_secs(spec.timeout_secs),
            build_command(&spec).output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("command timed out after {}s", spec.timeout_secs))??;

        Ok(format_command_output(&output))
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: CommandSpec,
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

fn build_command(spec: &CommandSpec) -> Command {
    let mut command = if spec.sandbox && cfg!(target_os = "macos") {
        let mut command = Command::new("sandbox-exec");
        command
            .arg("-p")
            .arg(SANDBOX_POLICY)
            .arg("sh")
            .arg("-c")
            .arg(&spec.command);
        command
    } else {
        let mut command = Command::new("sh");
        command.arg("-c").arg(&spec.command);
        command
    };

    if let Some(dir) = &spec.current_dir {
        command.current_dir(dir);
    }

    command
}

fn format_command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        stdout.to_string()
    } else {
        format!("stdout:\n{stdout}\nstderr:\n{stderr}")
    }
}
