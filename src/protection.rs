use std::{
    fs,
    path::Path,
};

use crate::{config::GitHelperConfig, Result};

const HOOK_FILE_NAME: &str = "pre-commit";
const MANAGED_START: &str = "# lil-big-helper managed start";
const MANAGED_END: &str = "# lil-big-helper managed end";

pub fn is_protected_branch(branch: &str, config: &GitHelperConfig) -> bool {
    config.protected_base_branches.iter().any(|b| b == branch)
}

pub fn install_or_update_pre_commit_hook(git_dir: &Path, config: &GitHelperConfig) -> Result<()> {
    let hook_path = git_dir.join("hooks").join(HOOK_FILE_NAME);
    let content = managed_hook_content(config);

    match fs::read_to_string(&hook_path) {
        Ok(existing) => {
            if existing.contains(MANAGED_START) && existing.contains(MANAGED_END) {
                let updated = replace_managed_block(&existing, &content)?;
                fs::write(&hook_path, updated)?;
                return Ok(());
            }
            Err("existing pre-commit hook is unmanaged; refusing to overwrite".into())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = hook_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&hook_path, content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&hook_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&hook_path, perms)?;
            }
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

pub fn managed_hook_content(config: &GitHelperConfig) -> String {
    let branches = config
        .protected_base_branches
        .iter()
        .map(|branch| format!("\"{branch}\""))
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "#!/bin/sh
{MANAGED_START}
branch=\"$(git branch --show-current)\"
for protected in {branches}; do
  if [ \"$branch\" = \"$protected\" ]; then
    echo \"lil-big-helper: direct commits to protected branch '$branch' are blocked\"
    exit 1
  fi
done
{MANAGED_END}
"
    )
}

fn replace_managed_block(existing: &str, managed: &str) -> Result<String> {
    let start = existing
        .find(MANAGED_START)
        .ok_or("missing managed hook start marker")?;
    let end = existing
        .find(MANAGED_END)
        .ok_or("missing managed hook end marker")?;
    let end = existing[end..]
        .find('\n')
        .map(|offset| end + offset + 1)
        .unwrap_or(existing.len());

    let mut content = String::new();
    content.push_str(&existing[..start]);
    content.push_str(managed);
    content.push_str(&existing[end..]);
    Ok(content)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn config() -> GitHelperConfig {
        GitHelperConfig {
            protected_base_branches: vec!["main".into(), "dev".into()],
            ..GitHelperConfig::default()
        }
    }

    #[test]
    fn protected_branch_match() {
        assert!(is_protected_branch("main", &config()));
        assert!(!is_protected_branch("feature/test", &config()));
    }

    #[test]
    fn install_hook_refuses_unmanaged_hook() {
        let dir = tempdir().unwrap();
        let hook_path = dir.path().join("hooks").join(HOOK_FILE_NAME);
        fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
        fs::write(&hook_path, "#!/bin/sh\necho custom\n").unwrap();

        let err = install_or_update_pre_commit_hook(dir.path(), &config()).unwrap_err();
        assert!(err.to_string().contains("unmanaged"));
    }

    #[test]
    fn install_hook_creates_managed_file() {
        let dir = tempdir().unwrap();
        install_or_update_pre_commit_hook(dir.path(), &config()).unwrap();

        let hook_path = dir.path().join("hooks").join(HOOK_FILE_NAME);
        let content = fs::read_to_string(hook_path).unwrap();
        assert!(content.contains(MANAGED_START));
    }
}
