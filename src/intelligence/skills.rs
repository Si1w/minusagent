use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::intelligence::utils::{extract_body, parse_frontmatter};

const MAX_SKILLS: usize = 150;

/// A discovered skill (frontmatter only, body loaded on activation)
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Path to the SKILL.md file (body read on demand)
    pub path: PathBuf,
}

impl Skill {
    /// Load the full body (after frontmatter) from the SKILL.md file
    pub fn load_body(&self) -> Option<String> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        let body = extract_body(&content);
        if body.is_empty() { None } else { Some(body) }
    }
}

/// Discovers skills from SKILL.md files across workspace directories
///
/// A skill is a directory containing a `SKILL.md` with YAML frontmatter.
/// Scans multiple directories in priority order; later directories override
/// earlier ones with the same skill name.
pub struct SkillsManager {
    workspace_dir: PathBuf,
    pub skills: Vec<Skill>,
}

impl SkillsManager {
    /// Create a new skills manager for the given workspace directory
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
            skills: Vec::new(),
        }
    }

    /// Discover skills by scanning directories in priority order
    ///
    /// Only reads frontmatter (name + description). Body is loaded on demand.
    ///
    /// # Arguments
    ///
    /// * `extra_dirs` - Additional directories to scan (highest priority first)
    pub fn discover(&mut self, extra_dirs: &[PathBuf]) {
        let cwd = std::env::current_dir().unwrap_or_default();

        let mut scan_order: Vec<PathBuf> = extra_dirs.to_vec();
        scan_order.push(self.workspace_dir.join("skills"));
        scan_order.push(self.workspace_dir.join(".skills"));
        scan_order.push(self.workspace_dir.join(".agents").join("skills"));
        scan_order.push(cwd.join(".agents").join("skills"));
        scan_order.push(cwd.join("skills"));

        let mut seen: HashMap<String, Skill> = HashMap::new();
        for dir in &scan_order {
            for skill in Self::scan_dir(dir) {
                seen.insert(skill.name.clone(), skill);
            }
        }

        self.skills = seen.into_values().collect();
        self.skills.truncate(MAX_SKILLS);
    }

    fn scan_dir(base: &Path) -> Vec<Skill> {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        dirs.sort_by_key(|e| e.file_name());

        let mut found = Vec::new();
        for entry in dirs {
            let skill_md = entry.path().join("SKILL.md");
            let content = match std::fs::read_to_string(&skill_md) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let meta = parse_frontmatter(&content);
            let name = match meta.get("name") {
                Some(n) if !n.is_empty() => n.clone(),
                _ => continue,
            };

            found.push(Skill {
                name,
                description: meta
                    .get("description")
                    .cloned()
                    .unwrap_or_default(),
                path: skill_md,
            });
        }

        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_discover_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SkillsManager::new(dir.path());
        mgr.discover(&[]);
        assert!(mgr.skills.is_empty());
    }

    #[test]
    fn test_discover_with_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("greet");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: greet\ndescription: Say hello\n---\nGreets the user.",
        )
        .unwrap();

        let mut mgr = SkillsManager::new(dir.path());
        mgr.discover(&[]);
        assert_eq!(mgr.skills.len(), 1);
        assert_eq!(mgr.skills[0].name, "greet");
        assert_eq!(mgr.skills[0].description, "Say hello");
        // Body not loaded at discovery
        assert_eq!(mgr.skills[0].load_body().unwrap(), "Greets the user.");
    }
}
