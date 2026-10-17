use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionRecord {
    pub session_id: String,
    pub token: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FileStore;

impl FileStore {
    fn path() -> Result<PathBuf> {
        let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(".orchestra").join("session.json"))
    }

    pub fn save(&self, rec: &SessionRecord) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = serde_json::to_string_pretty(rec)?;
        fs::write(path, s)?;
        Ok(())
    }

    pub fn load(&self) -> Result<Option<SessionRecord>> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(path)?;
        let rec: SessionRecord = serde_json::from_str(&s)?;
        Ok(Some(rec))
    }

    pub fn clear(&self) -> Result<()> {
        let path = Self::path()?;
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}
