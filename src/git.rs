use std::{
    hash::Hash,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::FxHashMap;

use crate::Result;

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitHash(String);

impl CommitHash {
    pub fn as_short_hash(&self) -> &str {
        &self.0[0..7]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CommitHash {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Default, Clone)]
pub enum CommitType {
    #[default]
    Commit,
    Stash,
}

#[derive(Debug, Default, Clone)]
pub struct Commit {
    pub commit_hash: CommitHash,
    pub author_name: String,
    pub author_email: String,
    pub author_date: DateTime<FixedOffset>,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<FixedOffset>,
    pub subject: String,
    pub body: String,
    pub parent_commit_hashes: Vec<CommitHash>,
    pub commit_type: CommitType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ref {
    Tag {
        name: String,
        target: CommitHash,
    },
    Branch {
        name: String,
        target: CommitHash,
    },
    RemoteBranch {
        name: String,
        target: CommitHash,
    },
    Stash {
        name: String,
        message: String,
        target: CommitHash,
    },
}

impl Ref {
    pub fn name(&self) -> &str {
        match self {
            Ref::Tag { name, .. } => name,
            Ref::Branch { name, .. } => name,
            Ref::RemoteBranch { name, .. } => name,
            Ref::Stash { name, .. } => name,
        }
    }

    pub fn target(&self) -> &CommitHash {
        match self {
            Ref::Tag { target, .. } => target,
            Ref::Branch { target, .. } => target,
            Ref::RemoteBranch { target, .. } => target,
            Ref::Stash { target, .. } => target,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Head {
    Branch { name: String },
    Detached { target: CommitHash },
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SortCommit {
    Chronological,
    Topological,
}

type CommitMap = FxHashMap<CommitHash, Commit>;
type CommitsMap = FxHashMap<CommitHash, Vec<CommitHash>>;

type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    path: PathBuf,
    commit_map: CommitMap,

    parents_map: CommitsMap,
    children_map: CommitsMap,

    ref_map: RefMap,
    head: Head,
    // to preserve order of the original commits from `git log`, we store the commit hashes
    commit_hashes: Vec<CommitHash>,
}

impl Repository {
    pub fn load(path: &Path, sort: SortCommit, max_count: Option<usize>) -> Result<Self> {
        check_git_repository(path)?;

        let (mut ref_map, head) = load_refs(path);

        let stashes = load_all_stashes(path);
        let commits = load_all_commits(path, sort, &head, &stashes, max_count);
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);

        let stash_ref_map = load_stashes_as_refs(path);
        merge_ref_maps(&mut ref_map, stash_ref_map);

        Ok(Self::new(
            path.to_path_buf(),
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
        ))
    }

    pub fn new(
        path: PathBuf,
        commit_map: CommitMap,
        parents_map: CommitsMap,
        children_map: CommitsMap,
        ref_map: RefMap,
        head: Head,
        commit_hashes: Vec<CommitHash>,
    ) -> Self {
        Self {
            path,
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
        }
    }

    pub fn commit(&self, commit_hash: &CommitHash) -> Option<&Commit> {
        self.commit_map.get(commit_hash)
    }

    pub fn all_commits(&self) -> Vec<&Commit> {
        self.commit_hashes
            .iter()
            .filter_map(|hash| self.commit(hash))
            .collect()
    }

    pub fn parents_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.parents_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn children_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn refs(&self, commit_hash: &CommitHash) -> Vec<&Ref> {
        self.ref_map
            .get(commit_hash)
            .map(|refs| refs.iter().collect::<Vec<&Ref>>())
            .unwrap_or_default()
    }

    pub fn all_refs(&self) -> Vec<&Ref> {
        self.ref_map.values().flatten().collect()
    }

    pub fn head(&self) -> &Head {
        &self.head
    }

    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap().clone();
        let changes = if commit.parent_commit_hashes.is_empty() {
            get_initial_commit_additions(&self.path, commit_hash)
        } else {
            get_diff_summary(&self.path, commit_hash)
        };
        (commit, changes)
    }
}

fn check_git_repository(path: &Path) -> Result<()> {
    if !is_inside_work_tree(path) && !is_bare_repository(path) {
        let msg = "not a git repository (or any of the parent directories)";
        return Err(msg.into());
    }
    Ok(())
}

fn is_inside_work_tree(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn is_bare_repository(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Vec<Commit> {
    let mut cmd = Command::new("git");
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format()))
    .arg("--date=iso-strict")
    .arg("-z"); // use NUL as a delimiter

    // exclude stashes and other refs
    cmd.arg("--branches").arg("--remotes").arg("--tags");

    // commits that are reachable from the stashes
    stashes.iter().for_each(|stash| {
        cmd.arg(stash.parent_commit_hashes[0].as_str());
    });

    if !matches!(head, Head::None) {
        cmd.arg("HEAD");
    }

    if let Some(n) = max_count {
        cmd.arg("--max-count").arg(n.to_string());
    }

    cmd.current_dir(path).stdout(Stdio::piped());

    let mut process = cmd.spawn().unwrap();

    let stdout = process.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
            commit_type: CommitType::Commit,
        };

        commits.push(commit);
    }

    process.wait().unwrap();

    commits
}

fn load_all_stashes(path: &Path) -> Vec<Commit> {
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--pretty={}", load_commits_format()))
        .arg("--date=iso-strict")
        .arg("-z") // use NUL as a delimiter
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
            commit_type: CommitType::Stash,
        };

        commits.push(commit);
    }

    cmd.wait().unwrap();

    commits
}

fn load_commits_format() -> String {
    [
        "%H", "%an", "%ae", "%ad", "%cn", "%ce", "%cd", "%s", "%b", "%P",
    ]
    .join("%x1f") // use Unit Separator as a delimiter
}

fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn parse_parent_commit_hashes(s: &str) -> Vec<CommitHash> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(' ').map(|s| s.into()).collect()
}

fn build_commits_maps(commits: &Vec<Commit>) -> (CommitsMap, CommitsMap) {
    let mut parents_map: CommitsMap = FxHashMap::default();
    let mut children_map: CommitsMap = FxHashMap::default();
    for commit in commits {
        let hash = &commit.commit_hash;
        for parent_hash in &commit.parent_commit_hashes {
            parents_map
                .entry(hash.clone())
                .or_default()
                .push(parent_hash.clone());
            children_map
                .entry(parent_hash.clone())
                .or_default()
                .push(hash.clone());
        }
    }

    (parents_map, children_map)
}

fn to_commit_map(commits: Vec<Commit>) -> CommitMap {
    commits
        .into_iter()
        .map(|commit| (commit.commit_hash.clone(), commit))
        .collect()
}

fn merge_stashes_to_commits(commits: Vec<Commit>, stashes: Vec<Commit>) -> Vec<Commit> {
    // Stash commit has multiple parent commits, but the first parent commit is the commit that the stash was created from.
    // If the first parent commit is not found, the stash commit is ignored.
    let mut ret = Vec::new();
    let mut statsh_map: FxHashMap<CommitHash, Vec<Commit>> =
        stashes
            .into_iter()
            .fold(FxHashMap::default(), |mut acc, commit| {
                let parent = commit.parent_commit_hashes[0].clone();
                acc.entry(parent).or_default().push(commit);
                acc
            });
    for commit in commits {
        if let Some(stashes) = statsh_map.remove(&commit.commit_hash) {
            for stash in stashes {
                ret.push(stash);
            }
        }
        ret.push(commit);
    }
    ret
}

fn load_refs(path: &Path) -> (RefMap, Head) {
    let mut cmd = Command::new("git")
        .arg("show-ref")
        .arg("--head")
        .arg("--dereference")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();
    let mut tag_map: FxHashMap<String, Ref> = FxHashMap::default();
    let mut head: Head = Head::None;

    for line in reader.lines() {
        let line = line.unwrap();

        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let hash = parts[0];
        let refs = parts[1];

        if refs == "HEAD" {
            head = if let Some(branch) = get_current_branch(path) {
                Head::Branch { name: branch }
            } else {
                Head::Detached {
                    target: hash.into(),
                }
            };
        } else if let Some(r) = parse_branch_refs(hash, refs) {
            ref_map.entry(hash.into()).or_default().push(r);
        } else if let Some(r) = parse_tag_refs(hash, refs) {
            // if annotated tag exists, it will be overwritten by the following line of the same tag
            // this will make the tag point to the commit that the annotated tag points to
            tag_map.insert(r.name().into(), r);
        }
    }

    for tag in tag_map.into_values() {
        ref_map.entry(tag.target().clone()).or_default().push(tag);
    }

    ref_map.values_mut().for_each(|refs| refs.sort());

    cmd.wait().unwrap();

    (ref_map, head)
}

fn load_stashes_as_refs(path: &Path) -> RefMap {
    let format = ["%gd", "%H", "%s"].join("%x1f"); // use Unit Separator as a delimiter
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--format={format}"))
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();

    for line in reader.lines() {
        let line = line.unwrap();

        let parts: Vec<&str> = line.split('\x1f').collect();
        if parts.len() != 3 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let name = parts[0];
        let hash = parts[1];
        let subject = parts[2];

        let r = Ref::Stash {
            name: name.into(),
            message: subject.into(),
            target: hash.into(),
        };

        ref_map.entry(hash.into()).or_default().push(r);
    }

    cmd.wait().unwrap();

    ref_map
}

fn merge_ref_maps(m1: &mut RefMap, m2: RefMap) {
    for (k, v) in m2 {
        m1.entry(k).or_default().extend(v);
    }
}

fn parse_branch_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/heads/") {
        let name = refs.trim_start_matches("refs/heads/");
        Some(Ref::Branch {
            name: name.into(),
            target: hash.into(),
        })
    } else if refs.starts_with("refs/remotes/") {
        let name = refs.trim_start_matches("refs/remotes/");
        Some(Ref::RemoteBranch {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn parse_tag_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/tags/") {
        let name = refs.trim_start_matches("refs/tags/");
        let name = name.trim_end_matches("^{}");
        Some(Ref::Tag {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

pub fn get_current_branch(path: &Path) -> Option<String> {
    git_stdout(
        Command::new("git")
            .arg("branch")
            .arg("--show-current")
            .current_dir(path),
    )
}

pub fn is_dirty(path: &Path) -> bool {
    !git_stdout(
        Command::new("git")
            .arg("status")
            .arg("--short")
            .current_dir(path),
    )
    .unwrap_or_default()
    .is_empty()
}

pub fn has_staged_changes(path: &Path) -> bool {
    let status = Command::new("git")
        .arg("diff")
        .arg("--cached")
        .arg("--quiet")
        .arg("--exit-code")
        .current_dir(path)
        .status()
        .unwrap();
    !status.success()
}

pub fn get_upstream_branch(path: &Path) -> Option<String> {
    git_stdout(
        Command::new("git")
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("--symbolic-full-name")
            .arg("@{upstream}")
            .current_dir(path),
    )
}

pub fn get_repo_root(path: &Path) -> Option<PathBuf> {
    git_stdout(
        Command::new("git")
            .arg("rev-parse")
            .arg("--show-toplevel")
            .current_dir(path),
    )
    .map(PathBuf::from)
}

pub fn get_git_dir(path: &Path) -> Option<PathBuf> {
    git_stdout(
        Command::new("git")
            .arg("rev-parse")
            .arg("--absolute-git-dir")
            .current_dir(path),
    )
    .map(PathBuf::from)
}

pub fn get_local_branches(path: &Path) -> Vec<String> {
    git_lines(
        Command::new("git")
            .arg("for-each-ref")
            .arg("--format=%(refname:short)")
            .arg("refs/heads")
            .current_dir(path),
    )
}

pub fn add_hidden_base_worktree(path: &Path, worktree_path: &Path, base: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("worktree")
            .arg("add")
            .arg("-B")
            .arg(base)
            .arg(worktree_path)
            .arg(base)
            .current_dir(path),
    )
}

pub fn remove_hidden_base_worktree(path: &Path, worktree_path: &Path) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(worktree_path)
            .current_dir(path),
    )
}

pub fn create_branch(path: &Path, branch: &str, start_point: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("branch")
            .arg(branch)
            .arg(start_point)
            .current_dir(path),
    )
}

pub fn switch_branch(path: &Path, branch: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("switch")
            .arg(branch)
            .current_dir(path),
    )
}

pub fn stash_push(path: &Path, message: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("stash")
            .arg("push")
            .arg("-u")
            .arg("-m")
            .arg(message)
            .current_dir(path),
    )
}

pub fn refresh_hidden_base_worktree(worktree_path: &Path, base: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("fetch")
            .arg("origin")
            .current_dir(worktree_path),
    )?;

    run_git(
        Command::new("git")
            .arg("merge")
            .arg("--ff-only")
            .arg(format!("origin/{base}"))
            .current_dir(worktree_path),
    )
}

pub fn commit(path: &Path, message: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg(message)
            .current_dir(path),
    )
}

pub fn commit_staged_changes(path: &Path, message: &str) -> Result<()> {
    commit(path, message)
}

pub fn push(path: &Path, remote: &str, branch: &str, set_upstream: bool) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("push");
    if set_upstream {
        cmd.arg("--set-upstream");
    }
    cmd.arg(remote).arg(branch).current_dir(path);
    run_git(&mut cmd)
}

pub fn merge_base_into_current(path: &Path, base: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("merge")
            .arg("--no-edit")
            .arg(base)
            .current_dir(path),
    )
}

fn git_stdout(cmd: &mut Command) -> Option<String> {
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

fn git_lines(cmd: &mut Command) -> Vec<String> {
    git_stdout(cmd)
        .map(|stdout| stdout.lines().map(|line| line.to_string()).collect())
        .unwrap_or_default()
}

fn run_git(cmd: &mut Command) -> Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };

    if message.is_empty() {
        Err("git command failed".into())
    } else {
        Err(message.into())
    }
}

#[derive(Debug)]
pub enum FileChange {
    Add { path: String },
    Modify { path: String },
    Delete { path: String },
    Move { from: String, to: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
}

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("diff")
        .arg("--name-status")
        .arg(format!("{}^", commit_hash.0))
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        let parts: Vec<&str> = line.split('\t').collect();

        match &parts[0][0..1] {
            "A" => changes.push(FileChange::Add {
                path: parts[1].into(),
            }),
            "M" => changes.push(FileChange::Modify {
                path: parts[1].into(),
            }),
            "D" => changes.push(FileChange::Delete {
                path: parts[1].into(),
            }),
            "R" => changes.push(FileChange::Move {
                from: parts[1].into(),
                to: parts[2].into(),
            }),
            _ => {}
        }
    }

    cmd.wait().unwrap();

    changes
}

pub fn get_initial_commit_additions(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("ls-tree")
        .arg("--name-status")
        .arg("-r") // the empty tree hash
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        changes.push(FileChange::Add { path: line });
    }

    cmd.wait().unwrap();

    changes
}

pub fn get_status_entries(path: &Path) -> Vec<StatusEntry> {
    git_lines(
        Command::new("git")
            .arg("status")
            .arg("--short")
            .arg("--untracked-files=all")
            .current_dir(path),
    )
    .into_iter()
    .filter_map(|line| parse_status_entry(&line))
    .collect()
}

pub fn get_status_diff(path: &Path, entry: &StatusEntry) -> Result<String> {
    if entry.untracked {
        return git_diff_output(
            Command::new("git")
                .arg("diff")
                .arg("--no-index")
                .arg("--color=never")
                .arg("--")
                .arg("/dev/null")
                .arg(&entry.path)
                .current_dir(path),
            true,
        );
    }

    let staged = if entry.staged {
        Some(git_diff_output(
            Command::new("git")
                .arg("diff")
                .arg("--cached")
                .arg("--color=never")
                .arg("--")
                .arg(&entry.path)
                .current_dir(path),
            false,
        )?)
    } else {
        None
    };

    let unstaged = if entry.unstaged {
        Some(git_diff_output(
            Command::new("git")
                .arg("diff")
                .arg("--color=never")
                .arg("--")
                .arg(&entry.path)
                .current_dir(path),
            false,
        )?)
    } else {
        None
    };

    match (staged, unstaged) {
        (Some(staged), Some(unstaged)) => {
            let mut diff = String::new();
            diff.push_str("--- staged ---\n");
            diff.push_str(&staged);
            if !staged.ends_with('\n') {
                diff.push('\n');
            }
            diff.push_str("--- unstaged ---\n");
            diff.push_str(&unstaged);
            Ok(diff)
        }
        (Some(staged), None) => Ok(staged),
        (None, Some(unstaged)) => Ok(unstaged),
        (None, None) => Ok(String::new()),
    }
}

pub fn stage_path(path: &Path, file_path: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("add")
            .arg("--")
            .arg(file_path)
            .current_dir(path),
    )
}

pub fn unstage_path(path: &Path, file_path: &str) -> Result<()> {
    run_git(
        Command::new("git")
            .arg("restore")
            .arg("--staged")
            .arg("--")
            .arg(file_path)
            .current_dir(path),
    )
}

fn parse_status_entry(line: &str) -> Option<StatusEntry> {
    if line.len() < 4 {
        return None;
    }

    let staged_code = line.chars().next()?;
    let unstaged_code = line.chars().nth(1)?;
    let raw_path = line[3..].trim();
    let path = raw_path
        .rsplit_once(" -> ")
        .map(|(_, to)| to.to_string())
        .unwrap_or_else(|| raw_path.to_string());

    Some(StatusEntry {
        path,
        staged: staged_code != ' ' && staged_code != '?',
        unstaged: unstaged_code != ' ' && unstaged_code != '?',
        untracked: staged_code == '?' && unstaged_code == '?',
    })
}

fn git_diff_output(cmd: &mut Command, allow_exit_code_one: bool) -> Result<String> {
    let output = cmd.output()?;
    if output.status.success() || (allow_exit_code_one && output.status.code() == Some(1)) {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };

    if message.is_empty() {
        Err("git command failed".into())
    } else {
        Err(message.into())
    }
}
