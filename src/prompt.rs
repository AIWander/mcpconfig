use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::{info, warn};

/// Cached file entry: content + last mtime we saw.
struct CacheEntry {
    content: String,
    mtime: SystemTime,
}

/// Reads and caches system prompt context files, re-reading only when mtime changes.
pub struct PromptInjector {
    files: Vec<PathBuf>,
    cache: Mutex<HashMap<PathBuf, CacheEntry>>,
}

fn default_system_prompt_files() -> Vec<String> {
    vec![
        "/opt/cpc/state/ARCHITECTURE.md".to_string(),
        "/opt/cpc/state/STATE.md".to_string(),
    ]
}

impl PromptInjector {
    /// Build from the model config's `system_prompt_files` field.
    /// If the list is empty, uses defaults.
    pub fn new(configured_files: &[String]) -> Self {
        let files = if configured_files.is_empty() {
            default_system_prompt_files()
        } else {
            configured_files.to_vec()
        };
        Self {
            files: files.into_iter().map(PathBuf::from).collect(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Read all configured files (with mtime caching) and prepend to the original system prompt.
    pub fn inject(&self, original_prompt: &str) -> String {
        let sections = self.read_sections();
        if sections.is_empty() {
            return original_prompt.to_string();
        }

        let mut result = String::new();
        for (label, content) in &sections {
            result.push_str(&format!("[{}]\n{}\n\n", label, content));
        }
        result.push_str("[Task System Prompt]\n");
        result.push_str(original_prompt);
        result
    }

    fn read_sections(&self) -> Vec<(String, String)> {
        let mut sections = Vec::new();
        let mut cache = self.cache.lock().unwrap();

        for path in &self.files {
            let label = label_for_path(path);

            // Check if file exists and get mtime
            let metadata = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => {
                    info!(
                        "prompt context file not found, skipping: {}",
                        path.display()
                    );
                    continue;
                }
            };

            let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            // Check cache
            if let Some(entry) = cache.get(path) {
                if entry.mtime == mtime {
                    sections.push((label, entry.content.clone()));
                    continue;
                }
            }

            // Read and cache
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    sections.push((label.clone(), content.clone()));
                    cache.insert(path.clone(), CacheEntry { content, mtime });
                }
                Err(e) => {
                    warn!(
                        "failed to read prompt context file {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }

        sections
    }
}

/// Derive a human-readable label from a file path.
fn label_for_path(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Context");
    match stem {
        "ARCHITECTURE" => "System Context".to_string(),
        "STATE" => "Current State".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_inject_no_files() {
        let injector = PromptInjector::new(&["/nonexistent/foo.md".to_string()]);
        let result = injector.inject("You are helpful.");
        assert_eq!(result, "You are helpful.");
    }

    #[test]
    fn test_inject_with_files() {
        let tmp = std::env::temp_dir().join("mcpconfig_prompt_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let arch_path = tmp.join("ARCHITECTURE.md");
        let state_path = tmp.join("STATE.md");
        fs::write(&arch_path, "# Architecture\nMCP-driven agent").unwrap();
        fs::write(&state_path, "# State\nRunning on droplet").unwrap();

        let injector = PromptInjector::new(&[
            arch_path.to_string_lossy().to_string(),
            state_path.to_string_lossy().to_string(),
        ]);
        let result = injector.inject("You are helpful.");

        assert!(result.contains("[System Context]"));
        assert!(result.contains("MCP-driven agent"));
        assert!(result.contains("[Current State]"));
        assert!(result.contains("Running on droplet"));
        assert!(result.contains("[Task System Prompt]"));
        assert!(result.contains("You are helpful."));

        // Verify order: context files come before original prompt
        let ctx_pos = result.find("[System Context]").unwrap();
        let task_pos = result.find("[Task System Prompt]").unwrap();
        assert!(ctx_pos < task_pos);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_inject_partial_missing() {
        let tmp = std::env::temp_dir().join("mcpconfig_prompt_partial");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let arch_path = tmp.join("ARCHITECTURE.md");
        fs::write(&arch_path, "# Arch\nContent here").unwrap();

        let injector = PromptInjector::new(&[
            arch_path.to_string_lossy().to_string(),
            "/nonexistent/STATE.md".to_string(),
        ]);
        let result = injector.inject("Original prompt");

        // Should have the architecture section but skip the missing state file
        assert!(result.contains("[System Context]"));
        assert!(result.contains("Content here"));
        assert!(result.contains("[Task System Prompt]"));
        assert!(result.contains("Original prompt"));
        assert!(!result.contains("[Current State]"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_mtime_cache() {
        let tmp = std::env::temp_dir().join("mcpconfig_prompt_mtime");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("STATE.md");
        fs::write(&file_path, "version 1").unwrap();

        let injector = PromptInjector::new(&[file_path.to_string_lossy().to_string()]);

        let r1 = injector.inject("prompt");
        assert!(r1.contains("version 1"));

        // Update file content AND mtime
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&file_path, "version 2").unwrap();

        let r2 = injector.inject("prompt");
        assert!(r2.contains("version 2"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_label_for_path() {
        assert_eq!(
            label_for_path(&PathBuf::from("/opt/cpc/state/ARCHITECTURE.md")),
            "System Context"
        );
        assert_eq!(
            label_for_path(&PathBuf::from("/opt/cpc/state/STATE.md")),
            "Current State"
        );
        assert_eq!(
            label_for_path(&PathBuf::from("/opt/cpc/state/CUSTOM.md")),
            "CUSTOM"
        );
    }
}
