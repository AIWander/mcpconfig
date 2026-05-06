use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Append-only JSONL event writer. Flushes after every line.
pub struct EventWriter {
    file: File,
    pub run_dir: PathBuf,
}

/// A single event in the run log.
#[derive(Debug, Serialize)]
pub struct Event {
    pub ts: String,
    pub kind: String,
    #[serde(flatten)]
    pub data: Value,
}

impl EventWriter {
    /// Create a new run directory and event writer.
    pub fn new(runs_dir: &Path, task_name: &str) -> Result<Self> {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S");
        let dir_name = format!("{}_{}", timestamp, task_name);
        let run_dir = runs_dir.join(dir_name);
        fs::create_dir_all(&run_dir)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(run_dir.join("run.jsonl"))?;

        Ok(EventWriter { file, run_dir })
    }

    /// Write one event, flush immediately.
    pub fn log(&mut self, kind: &str, data: Value) -> Result<()> {
        let event = Event {
            ts: Utc::now().to_rfc3339(),
            kind: kind.to_string(),
            data,
        };
        let line = serde_json::to_string(&event)?;
        writeln!(self.file, "{}", line)?;
        self.file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::BufRead;

    #[test]
    fn test_jsonl_serialization() {
        let dir = std::env::temp_dir().join("bakeoff_test_events");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut writer = EventWriter {
            file: OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("test.jsonl"))
                .unwrap(),
            run_dir: dir.clone(),
        };

        writer
            .log("run_start", json!({"task": "smoke", "model": "test-model"}))
            .unwrap();
        writer
            .log(
                "tools_registered",
                json!({"count": 3, "names": ["a", "b", "c"]}),
            )
            .unwrap();
        writer
            .log("final_answer", json!({"iteration": 1, "content": "done"}))
            .unwrap();

        // Read back and verify
        let file = File::open(dir.join("test.jsonl")).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map(|l| l.unwrap())
            .collect();

        assert_eq!(lines.len(), 3);

        // Each line must be valid JSON with ts and kind
        for line in &lines {
            let parsed: Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("ts").is_some());
            assert!(parsed.get("kind").is_some());
        }

        // Verify specific fields
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(first["kind"], "run_start");
        assert_eq!(first["task"], "smoke");

        let second: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(second["kind"], "tools_registered");
        assert_eq!(second["count"], 3);

        // Cleanup
        let _ = fs::remove_dir_all(&dir);
    }
}
