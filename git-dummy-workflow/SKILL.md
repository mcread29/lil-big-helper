---
name: git-dummy-workflow
description: Manage git-related work in a local repository using repo-local rules and logging stored in `.git-dummy/index.mdc`. Use when Codex needs to do git work such as checking status, creating or switching branches, committing, pulling, pushing, stashing, discarding changes, or keeping branch hygiene. Trigger for user requests about git, branches, commits, pushes, pulls, rebases, stashes, or switching branches in the current repo.
---

# Git Dummy Workflow

## Overview

Use this skill to enforce a consistent git workflow across repositories by keeping repo-local rules in `.git-dummy/index.mdc`, requiring a branch prefix and base branch, writing informative commits, and logging each commit with timestamp and hash.

Treat `.git-dummy/index.mdc` as the local source of truth for workflow rules and commit history within the repository. Keep it current whenever git state changes materially.

## Workflow

Follow this workflow in order for any git-related task.

1. Confirm the current directory is in a git repository.
2. Ensure `.git-dummy/index.mdc` exists.
3. Read `.git-dummy/index.mdc` before taking git actions if it already exists.
4. Ensure the repo rules include both `branch prefix` and `base branch`.
5. Apply the branch, commit, pull, push, and switching rules below.
6. Update `.git-dummy/index.mdc` after commits or repo-rule changes.

## Repo Index File

The repo-local workflow file must live at `.git-dummy/index.mdc`.

If the file does not exist:

- create the `.git-dummy/` directory if needed
- create `.git-dummy/index.mdc`
- initialize it with a repo rules section and a commit log section

If the file does exist:

- read it first
- treat existing repo rules as authoritative unless the user explicitly changes them
- append to the commit log rather than replacing prior entries

Prefer this structure:

```md
# Git Dummy Index

## Repo Rules

- branch prefix: <required>
- base branch: <required>

## Notes

- Add repo-specific workflow notes here.

## Commit Log

- 2026-04-24T13:45:00-07:00 | abc1234 | feat: example title
  - Added example detail line.
```

Keep the file concise and readable. Preserve user-written notes unless they conflict with explicit user instructions.

## Required Repo Rules

The repo rules must include both of these entries:

- `branch prefix`
- `base branch`

If either is missing, ask the user before continuing with branch creation or branch-switching work that depends on it.

When asking for `branch prefix`, give suggestions such as:

- username, for example `mason`
- initials, for example `ml`
- team alias, for example `platform`
- ticket namespace if the repo already uses one, for example `mason` with branch names like `mason/fix-ci`

Explain the rule clearly:

- every working branch in the repo must use the configured prefix
- branch names must be of the form `<branch-prefix>/<branch-name>`
- working branches must be pushed to a remote

When asking for `base branch`, give suggestions such as:

- `main`
- `master`
- `develop`
- an agreed integration branch already used by the repo

Explain the rule clearly:

- all new working branches must be created from the configured base branch
- keep the local base branch refreshed before branching from it

If the repo already signals a conventional base branch through remotes or local branches, mention that in the suggestion.

## Branch Rules

Apply these rules to all branch work:

- create new branches from the configured base branch
- name all working branches as `<branch-prefix>/<descriptive-name>`
- push working branches to the remote after branch creation unless the user explicitly wants a local-only branch
- do not create unprefixed working branches
- do not branch from another working branch unless the user explicitly requests stacked work

When a requested branch name does not follow the configured prefix, correct it before creating or pushing the branch.

## Pull and Sync Rules

Keep branches current relative to the base branch.

- refresh the local base branch regularly
- pull the base branch into the working branch regularly
- do this especially before large changes, before opening or updating a PR, and when the branch has drifted

Prefer the repository's normal integration style. If there is no explicit repo preference:

- prefer a regular pull or merge from the base branch into the working branch
- avoid history-rewriting operations unless the user explicitly asks for them

If there are local conflicts or risky changes, surface that clearly before proceeding.

## Commit Rules

When committing:

- prefer committing local work over stashing or discarding it
- write an informative title
- write an informative description/body when the change is non-trivial
- make the title specific to the actual change, not a generic placeholder

Good commit titles:

- `fix: preserve repo rules when updating git-dummy index`
- `feat: add branch prefix enforcement for repo workflow`

Avoid titles like:

- `update`
- `fix stuff`
- `changes`

When the work spans multiple concerns, prefer multiple coherent commits if that can be done safely.

## Commit Log Rules

Log every commit in `.git-dummy/index.mdc`.

Each log entry must include:

- timestamp in ISO-8601 format with timezone when available
- commit hash
- commit title

When a commit has a meaningful body, add one short indented line summarizing the purpose.

Append new entries to the `## Commit Log` section. Do not delete older entries unless the user explicitly asks to rewrite the log.

## Branch Switching Safety

Before switching branches, ensure the working tree is safe.

All changes must be one of:

- committed
- stashed
- discarded

Preference order:

1. commit without push
2. stash
3. discard

Do not switch branches with loose changes unless the user explicitly asks for that risk and the command is safe for the specific files involved.

If there are uncommitted changes, state the options clearly and prefer a local commit.

## Output Expectations

For git tasks, report the important workflow state briefly:

- current branch
- configured branch prefix
- configured base branch
- whether `.git-dummy/index.mdc` was created or updated
- whether changes were committed, pushed, stashed, or discarded
- any follow-up needed, such as pushing a branch or syncing from base

When rules are missing, ask only for the missing values and give concrete suggestions.

## Example Requests

- "Create a new git branch for this work."
- "Commit my changes."
- "Push this branch."
- "Switch me back to the base branch."
- "Pull the latest changes from main."
- "Help me clean up my git status."
- "Do the git steps for this repo."
