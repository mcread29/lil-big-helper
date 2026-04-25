use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    config::GitHelperConfig,
    external::{run_codex_exec_capture, run_codex_exec_status},
    git,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowAction {
    StatusCommit {
        paths: Vec<String>,
    },
    StatusDiscard {
        paths: Vec<String>,
    },
    GraphPushCurrent,
    GraphPullCurrent,
    GraphPullOtherRemote {
        remote: String,
        branch: String,
    },
    GraphCreateBranch {
        base_branch: String,
        branch_name: String,
    },
    GraphCreatePullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowExecutionMode {
    Silent,
    Suspend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPromptContext {
    pub repo_path: PathBuf,
    pub current_branch: Option<String>,
    pub upstream_branch: Option<String>,
    pub base_branch: Option<String>,
    pub branch_prefix: String,
    pub protected_branches: Vec<String>,
    pub selected_paths: Vec<String>,
    pub selected_commit_hash: Option<String>,
    pub git_dummy_index_path: PathBuf,
    pub git_dummy_index_exists: bool,
    pub git_dummy_index_contents: Option<String>,
    pub require_git_dummy_index: bool,
    pub auto_set_upstream_on_first_push: bool,
    pub pr_create_default: String,
}

impl WorkflowPromptContext {
    pub fn gather(
        repo_path: &Path,
        git_helper: &GitHelperConfig,
        selected_paths: Vec<String>,
        selected_commit_hash: Option<String>,
        base_branch: Option<String>,
    ) -> Result<Self, String> {
        let repo_path = repo_path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve repository path: {e}"))?;
        let git_dummy_index_path = repo_path.join(&git_helper.workflow.git_dummy_index_path);
        let git_dummy_index_contents = fs::read_to_string(&git_dummy_index_path).ok();
        Ok(Self {
            current_branch: git::get_current_branch(&repo_path),
            upstream_branch: git::get_upstream_branch(&repo_path),
            repo_path,
            base_branch,
            branch_prefix: git_helper.branch_prefix.clone(),
            protected_branches: git_helper.protected_base_branches.clone(),
            selected_paths,
            selected_commit_hash,
            git_dummy_index_exists: git_dummy_index_path.exists(),
            git_dummy_index_path,
            git_dummy_index_contents,
            require_git_dummy_index: git_helper.workflow.require_git_dummy_index,
            auto_set_upstream_on_first_push: git_helper.auto_set_upstream_on_first_push,
            pr_create_default: git_helper.workflow.pr_create_default.clone(),
        })
    }
}

pub fn execute_workflow_action(
    git_helper: &GitHelperConfig,
    action: &WorkflowAction,
    context: &WorkflowPromptContext,
    mode: WorkflowExecutionMode,
) -> Result<Option<String>, String> {
    if !git_helper.workflow.enabled {
        return Err("Git workflow integration is disabled in config".into());
    }

    let prompt = build_workflow_prompt(action, context);
    match mode {
        WorkflowExecutionMode::Silent => run_codex_exec_capture(
            &git_helper.workflow.codex_command,
            &context.repo_path,
            &prompt,
        )
        .map(Some),
        WorkflowExecutionMode::Suspend => run_codex_exec_status(
            &git_helper.workflow.codex_command,
            &context.repo_path,
            &prompt,
        )
        .map(|_| None),
    }
}

pub fn build_workflow_prompt(action: &WorkflowAction, context: &WorkflowPromptContext) -> String {
    let current_branch = context.current_branch.as_deref().unwrap_or("detached");
    let upstream_branch = context.upstream_branch.as_deref().unwrap_or("none");
    let base_branch = context.base_branch.as_deref().unwrap_or("unknown");
    let selected_commit_hash = context.selected_commit_hash.as_deref().unwrap_or("none");
    let selected_paths = if context.selected_paths.is_empty() {
        "none".to_string()
    } else {
        context
            .selected_paths
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let git_dummy_status = if context.git_dummy_index_exists {
        format!(
            "Present at {}.\nContents:\n{}",
            context.git_dummy_index_path.display(),
            context
                .git_dummy_index_contents
                .as_deref()
                .unwrap_or("(empty)")
        )
    } else {
        format!("Missing at {}.", context.git_dummy_index_path.display())
    };

    let action_text = match action {
        WorkflowAction::StatusCommit { .. } => format!(
            "Requested action: commit only the selected paths.\n\
             Requirements:\n\
             - Stage only the selected paths.\n\
             - Write an informative git commit message.\n\
             - Refuse if the current branch is protected.\n\
             - If the git-dummy index is missing and required, create or update it before finishing.\n\
             - Append a commit log entry to the git-dummy index after a successful commit."
        ),
        WorkflowAction::StatusDiscard { .. } => format!(
            "Requested action: discard only the selected paths.\n\
             Requirements:\n\
             - Restore tracked files only within the selected paths.\n\
             - Remove untracked files only within the selected paths.\n\
             - Refuse if the operation would affect paths outside the selection."
        ),
        WorkflowAction::GraphPushCurrent => format!(
            "Requested action: push the current branch to its remote.\n\
             Requirements:\n\
             - Push the current branch only.\n\
             - If there is no upstream and auto-set-upstream is enabled, set it during push."
        ),
        WorkflowAction::GraphPullCurrent => format!(
            "Requested action: pull the current branch from its tracked upstream.\n\
             Requirements:\n\
             - Pull only from the tracked upstream.\n\
             - Refuse if there is no current branch or no upstream."
        ),
        WorkflowAction::GraphPullOtherRemote { remote, branch } => format!(
            "Requested action: pull from a different remote into the current branch.\n\
             Requirements:\n\
             - Pull from remote `{remote}` branch `{branch}` into the current branch.\n\
             - Refuse if the current branch is unknown."
        ),
        WorkflowAction::GraphCreateBranch {
            base_branch,
            branch_name,
        } => format!(
            "Requested action: create and switch to a new branch.\n\
             Requirements:\n\
             - Use base branch `{base_branch}`.\n\
             - Create branch `{branch_name}`.\n\
             - Honor branch-prefix and base-branch rules from the git-dummy index when present.\n\
             - If the git-dummy index is missing and required, create it before branching."
        ),
        WorkflowAction::GraphCreatePullRequest => format!(
            "Requested action: create a pull request with GitHub CLI.\n\
             Requirements:\n\
             - Use `gh pr create`.\n\
             - Prefer `{}`.\n\
             - Target the recorded base branch when known, otherwise use the configured base/protected branch.",
            context.pr_create_default
        ),
    };

    format!(
        "You are performing a git workflow action for lil-big-helper.\n\
         Execute the requested action directly in the repository. Do not describe what you would do.\n\
         Return concise terminal-safe output only.\n\
         Refuse with a clear error if the requested action is unsafe.\n\
         \n\
         Repository: {}\n\
         Current branch: {current_branch}\n\
         Upstream branch: {upstream_branch}\n\
         Base branch: {base_branch}\n\
         Branch prefix: {}\n\
         Protected branches: {}\n\
         Auto set upstream on first push: {}\n\
         Selected commit hash: {selected_commit_hash}\n\
         Selected paths:\n\
         {selected_paths}\n\
         \n\
         Git-dummy index:\n\
         {git_dummy_status}\n\
         Required if missing: {}\n\
         \n\
         {action_text}\n\
         \n\
         Use git commands for git operations and `gh` for pull request creation when needed.",
        context.repo_path.display(),
        context.branch_prefix,
        context.protected_branches.join(", "),
        if context.auto_set_upstream_on_first_push {
            "true"
        } else {
            "false"
        },
        if context.require_git_dummy_index {
            "true"
        } else {
            "false"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{build_workflow_prompt, WorkflowAction, WorkflowPromptContext};
    use std::path::PathBuf;

    fn sample_context() -> WorkflowPromptContext {
        WorkflowPromptContext {
            repo_path: PathBuf::from("/repo"),
            current_branch: Some("feature/test".into()),
            upstream_branch: Some("origin/feature/test".into()),
            base_branch: Some("main".into()),
            branch_prefix: "feature".into(),
            protected_branches: vec!["main".into()],
            selected_paths: vec!["src/app.rs".into(), "src/view/status.rs".into()],
            selected_commit_hash: Some("abc1234".into()),
            git_dummy_index_path: PathBuf::from("/repo/.git-dummy/index.mdc"),
            git_dummy_index_exists: false,
            git_dummy_index_contents: None,
            require_git_dummy_index: true,
            auto_set_upstream_on_first_push: true,
            pr_create_default: "gh pr create --fill".into(),
        }
    }

    #[test]
    fn commit_prompt_includes_selected_paths_and_git_dummy_requirement() {
        let prompt = build_workflow_prompt(
            &WorkflowAction::StatusCommit {
                paths: vec!["src/app.rs".into()],
            },
            &sample_context(),
        );
        assert!(prompt.contains("commit only the selected paths"));
        assert!(prompt.contains("- src/app.rs"));
        assert!(prompt.contains("Missing at /repo/.git-dummy/index.mdc."));
    }

    #[test]
    fn pull_other_remote_prompt_includes_remote_and_branch() {
        let prompt = build_workflow_prompt(
            &WorkflowAction::GraphPullOtherRemote {
                remote: "upstream".into(),
                branch: "main".into(),
            },
            &sample_context(),
        );
        assert!(prompt.contains("remote `upstream` branch `main`"));
    }
}
