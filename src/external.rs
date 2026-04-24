use std::{
    cell::RefCell,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use arboard::Clipboard;

use crate::config::ClipboardConfig;

const USER_COMMAND_MARKER_PREFIX: &str = "{{";
const USER_COMMAND_TARGET_HASH_MARKER: &str = "{{target_hash}}";
const USER_COMMAND_FIRST_PARENT_HASH_MARKER: &str = "{{first_parent_hash}}";
const USER_COMMAND_PARENT_HASHES_MARKER: &str = "{{parent_hashes}}";
const USER_COMMAND_REFS_MARKER: &str = "{{refs}}";
const USER_COMMAND_BRANCHES_MARKER: &str = "{{branches}}";
const USER_COMMAND_REMOTE_BRANCHES_MARKER: &str = "{{remote_branches}}";
const USER_COMMAND_TAGS_MARKER: &str = "{{tags}}";
const USER_COMMAND_AREA_WIDTH_MARKER: &str = "{{area_width}}";
const USER_COMMAND_AREA_HEIGHT_MARKER: &str = "{{area_height}}";

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

pub fn copy_to_clipboard(value: String, config: &ClipboardConfig) -> Result<(), String> {
    match config {
        ClipboardConfig::Auto => copy_to_clipboard_auto(value),
        ClipboardConfig::Custom { commands } => copy_to_clipboard_custom(value, commands),
    }
}

fn copy_to_clipboard_custom(value: String, commands: &[String]) -> Result<(), String> {
    if commands.is_empty() {
        return Err("No clipboard command specified".to_string());
    }

    let mut child = Command::new(&commands[0])
        .args(&commands[1..])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {e}", commands[0]))?;

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(value.as_bytes())
        .map_err(|e| format!("Failed to write to {}: {e}", commands[0]))?;

    child
        .wait()
        .map_err(|e| format!("{} failed: {e}", commands[0]))?;

    Ok(())
}

fn copy_to_clipboard_auto(value: String) -> Result<(), String> {
    CLIPBOARD.with_borrow_mut(|clipboard| {
        if clipboard.is_none() {
            *clipboard = Clipboard::new()
                .map(Some)
                .map_err(|e| format!("Failed to create clipboard: {e:?}"))?;
        }

        clipboard
            .as_mut()
            .expect("The clipboard should have been initialized above")
            .set_text(value)
            .map_err(|e| format!("Failed to copy to clipboard: {e:?}"))
    })
}

pub struct ExternalCommandParameters<'a> {
    pub command: &'a [String],
    pub target_hash: &'a str,
    pub parent_hashes: Vec<&'a str>,
    pub all_refs: Vec<&'a str>,
    pub branches: Vec<&'a str>,
    pub remote_branches: Vec<&'a str>,
    pub tags: Vec<&'a str>,
    pub area_width: u16,
    pub area_height: u16,
}

pub fn exec_user_command(params: ExternalCommandParameters) -> Result<String, String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .output()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.status.success() {
        let msg = format!(
            "Command exited with non-zero status: {}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(msg);
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

pub fn exec_user_command_suspend(params: ExternalCommandParameters) -> Result<(), String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.success() {
        let msg = format!("Command exited with non-zero status: {output}");
        return Err(msg);
    }

    Ok(())
}

pub fn generate_commit_message_with_codex(
    repo_path: &Path,
    staged_diff: &str,
) -> Result<String, String> {
    ensure_codex_authenticated()?;

    if staged_diff.trim().is_empty() {
        return Err("No staged diff available to generate a commit message".to_string());
    }

    let output_path = std::env::temp_dir().join(format!(
        "lil-big-helper-codex-commit-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    ));

    let prompt = concat!(
        "Generate a git commit message for the staged diff provided on stdin.\n",
        "Return only the commit message text.\n",
        "Use a short imperative subject line.\n",
        "Include a blank line and a body only if the change benefits from extra context.\n",
        "Do not use code fences, bullets, labels, or commentary."
    );

    let mut child = Command::new("codex")
        .arg("exec")
        .arg("--color")
        .arg("never")
        .arg("-C")
        .arg(repo_path)
        .arg("-o")
        .arg(&output_path)
        .arg(prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run codex: {e}"))?;

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(staged_diff.as_bytes())
        .map_err(|e| format!("Failed to write staged diff to codex stdin: {e}"))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for codex: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = fs::remove_file(&output_path);
        if is_codex_auth_error(&stderr) {
            return Err("Codex CLI is not authenticated. Run `codex login` and try again.".into());
        }
        return Err(if stderr.is_empty() {
            format!("codex exited with non-zero status: {}", output.status)
        } else {
            format!("codex failed: {stderr}")
        });
    }

    let message = fs::read_to_string(&output_path)
        .map_err(|e| format!("Failed to read codex commit message output: {e}"))?;
    let _ = fs::remove_file(&output_path);

    let message = message.trim().to_string();
    if message.is_empty() {
        return Err("Codex returned an empty commit message".into());
    }

    Ok(message)
}

fn build_user_command(params: &ExternalCommandParameters) -> Vec<String> {
    fn to_vec(ss: &[&str]) -> Vec<String> {
        ss.iter().map(|s| s.to_string()).collect()
    }
    let mut command = Vec::new();
    for arg in params.command {
        if !arg.contains(USER_COMMAND_MARKER_PREFIX) {
            command.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            // If the marker is used as a standalone argument, expand it into multiple arguments.
            // This allows the command to receive each item as a separate argument and correctly handle items that contain spaces.
            USER_COMMAND_BRANCHES_MARKER => command.extend(to_vec(&params.branches)),
            USER_COMMAND_REMOTE_BRANCHES_MARKER => command.extend(to_vec(&params.remote_branches)),
            USER_COMMAND_TAGS_MARKER => command.extend(to_vec(&params.tags)),
            USER_COMMAND_REFS_MARKER => command.extend(to_vec(&params.all_refs)),
            USER_COMMAND_PARENT_HASHES_MARKER => command.extend(to_vec(&params.parent_hashes)),
            // Otherwise, replace the marker within the single argument string.
            _ => command.push(replace_command_arg(arg, params)),
        }
    }
    command
}

fn ensure_codex_authenticated() -> Result<(), String> {
    let output = Command::new("codex")
        .arg("login")
        .arg("status")
        .output()
        .map_err(|e| format!("Failed to run codex login status: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if is_codex_auth_error(&stderr) || stderr.is_empty() {
            return Err("Codex CLI is not authenticated. Run `codex login` and try again.".into());
        }
        return Err(format!("Failed to check Codex login status: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout.contains("Logged in") || stderr.contains("Logged in") {
        Ok(())
    } else {
        Err("Codex CLI is not authenticated. Run `codex login` and try again.".into())
    }
}

fn is_codex_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not logged in")
        || lower.contains("not authenticated")
        || lower.contains("login required")
        || lower.contains("unauthorized")
}

fn replace_command_arg(s: &str, params: &ExternalCommandParameters) -> String {
    let sep = " ";
    let target_hash = params.target_hash;
    let first_parent_hash = &params.parent_hashes.first().cloned().unwrap_or_default();
    let parent_hashes = &params.parent_hashes.join(sep);
    let all_refs = &params.all_refs.join(sep);
    let branches = &params.branches.join(sep);
    let remote_branches = &params.remote_branches.join(sep);
    let tags = &params.tags.join(sep);
    let area_width = &params.area_width.to_string();
    let area_height = &params.area_height.to_string();

    s.replace(USER_COMMAND_TARGET_HASH_MARKER, target_hash)
        .replace(USER_COMMAND_FIRST_PARENT_HASH_MARKER, first_parent_hash)
        .replace(USER_COMMAND_PARENT_HASHES_MARKER, parent_hashes)
        .replace(USER_COMMAND_REFS_MARKER, all_refs)
        .replace(USER_COMMAND_BRANCHES_MARKER, branches)
        .replace(USER_COMMAND_REMOTE_BRANCHES_MARKER, remote_branches)
        .replace(USER_COMMAND_TAGS_MARKER, tags)
        .replace(USER_COMMAND_AREA_WIDTH_MARKER, area_width)
        .replace(USER_COMMAND_AREA_HEIGHT_MARKER, area_height)
}
