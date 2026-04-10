mod command;
mod file;

pub use command::{BashExec, is_dangerous_command};
pub use file::{EditFile, ReadFile, WriteFile};

#[cfg(test)]
pub fn safe_path(raw: &str) -> anyhow::Result<String> {
    file::safe_path(raw)
}
