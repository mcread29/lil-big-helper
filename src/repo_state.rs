use std::{
    fs,
    path::{Path, PathBuf},
};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::Result;

const STATE_DIR_NAME: &str = "lbh";
const STATE_FILE_NAME: &str = "state.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoState {
    #[serde(default)]
    pub branch_origins: FxHashMap<String, String>,
    #[serde(default)]
    pub branch_prefix: Option<String>,
}

impl RepoState {
    pub fn get_branch_origin(&self, branch: &str) -> Option<&str> {
        self.branch_origins.get(branch).map(String::as_str)
    }

    pub fn set_branch_origin(&mut self, branch: &str, base: &str) {
        self.branch_origins.insert(branch.into(), base.into());
    }

    pub fn get_branch_prefix(&self) -> Option<&str> {
        self.branch_prefix.as_deref()
    }

    pub fn set_branch_prefix(&mut self, branch_prefix: Option<&str>) {
        self.branch_prefix = branch_prefix.map(str::to_owned);
    }
}

pub fn load_repo_state(git_dir: &Path) -> Result<RepoState> {
    let path = state_file_path(git_dir);
    if !path.exists() {
        return Ok(RepoState::default());
    }

    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

pub fn save_repo_state(git_dir: &Path, state: &RepoState) -> Result<()> {
    let path = state_file_path(git_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn state_file_path(git_dir: &Path) -> PathBuf {
    git_dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn repo_state_roundtrip() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path();

        let mut state = RepoState::default();
        state.set_branch_origin("feature/test", "dev");
        state.set_branch_prefix(Some("feature/"));
        save_repo_state(git_dir, &state).unwrap();

        let loaded = load_repo_state(git_dir).unwrap();
        assert_eq!(loaded.get_branch_origin("feature/test"), Some("dev"));
        assert_eq!(loaded.get_branch_prefix(), Some("feature/"));
    }

    #[test]
    fn repo_state_defaults_branch_prefix_when_missing() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path();
        let path = state_file_path(git_dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "{\n  \"branch_origins\": {\n    \"feature/test\": \"dev\"\n  }\n}",
        )
        .unwrap();

        let loaded = load_repo_state(git_dir).unwrap();
        assert_eq!(loaded.get_branch_origin("feature/test"), Some("dev"));
        assert_eq!(loaded.get_branch_prefix(), None);
    }
}
