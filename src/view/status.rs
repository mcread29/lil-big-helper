use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{self, StatusEntry},
    repo_state::load_repo_state,
    view::{ListRefreshViewContext, RefreshViewContext, StatusRefreshViewContext},
    widget::commit_list::{CommitList, CommitListState},
};

#[derive(Debug)]
pub struct StatusView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    entries: Vec<StatusEntry>,
    selected: usize,
    offset: usize,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> StatusView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        entries: Vec<StatusEntry>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        Self {
            commit_list_state: Some(commit_list_state),
            entries,
            selected: 0,
            offset: 0,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => self.tx.send(AppEvent::Quit),
            UserEvent::Cancel | UserEvent::Close => self.tx.send(AppEvent::CloseStatus),
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.select_next();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.select_prev();
                }
            }
            UserEvent::GoToTop => self.select_first(),
            UserEvent::GoToBottom => self.select_last(),
            UserEvent::Confirm | UserEvent::StatusToggle => self.toggle_selected(),
            UserEvent::StatusCommit => self.tx.send(AppEvent::OpenStatusCommitPrompt),
            UserEvent::HelpToggle => self.tx.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.refresh(),
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let status_height = (area.height - 1).min(14);
        let [list_area, status_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(status_height)]).areas(area);

        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let lines = self.render_lines(status_area);
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, status_area);
    }

    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn reset_status_with(&mut self, ctx: StatusRefreshViewContext) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.offset = 0;
        } else {
            self.selected = ctx.selected.min(self.entries.len().saturating_sub(1));
            self.offset = self.selected.saturating_sub(1);
        }
    }

    fn render_lines(&self, area: Rect) -> Vec<Line<'static>> {
        let current_branch =
            git::get_current_branch(std::path::Path::new(".")).unwrap_or_else(|| "detached".into());
        let (base_branch, prefix) = repo_status_metadata(&self.ctx);
        let staged_count = self.entries.iter().filter(|entry| entry.staged).count();
        let unstaged_count = self.entries.iter().filter(|entry| entry.unstaged).count();
        let untracked_count = self.entries.iter().filter(|entry| entry.untracked).count();

        let mut lines = vec![
            Line::raw(format!(
                "Branch: {current_branch}  Base: {base_branch}  Prefix: {prefix}"
            ))
            .fg(self.ctx.color_theme.detail_label_fg),
            Line::raw(format!(
                "Staged: {staged_count}  Unstaged: {unstaged_count}  Untracked: {untracked_count}"
            )),
            Line::raw("Enter/s: toggle stage  c: commit staged  esc/backspace: close")
                .fg(self.ctx.color_theme.status_info_fg),
            Line::raw(""),
        ];

        let max_rows = area.height.saturating_sub(5) as usize;
        if self.entries.is_empty() {
            lines.push(Line::raw("No changes").add_modifier(Modifier::DIM));
            return lines;
        }

        let start = self.offset.min(self.entries.len().saturating_sub(1));
        let end = (start + max_rows.max(1)).min(self.entries.len());
        for (index, entry) in self.entries[start..end].iter().enumerate() {
            let actual = start + index;
            let marker = if actual == self.selected { ">" } else { " " };
            let state = status_flags(entry);
            let mut line = Line::raw(format!("{marker} [{state}] {}", entry.path));
            if actual == self.selected {
                line = line
                    .fg(self.ctx.color_theme.ref_selected_fg)
                    .bg(self.ctx.color_theme.ref_selected_bg);
            } else if entry.untracked {
                line = line.fg(self.ctx.color_theme.status_warn_fg);
            } else if entry.staged {
                line = line.fg(self.ctx.color_theme.status_success_fg);
            }
            lines.push(line);
        }

        lines
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            if self.selected > self.offset + 7 {
                self.offset += 1;
            }
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.offset && self.offset > 0 {
                self.offset -= 1;
            }
        }
    }

    fn select_first(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    fn select_last(&mut self) {
        if !self.entries.is_empty() {
            self.selected = self.entries.len() - 1;
            self.offset = self.selected.saturating_sub(7);
        }
    }

    fn toggle_selected(&self) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        let result = if entry.untracked || entry.unstaged {
            git::stage_path(std::path::Path::new("."), &entry.path)
        } else if entry.staged {
            git::unstage_path(std::path::Path::new("."), &entry.path)
        } else {
            Ok(())
        };

        match result {
            Ok(()) => self.refresh(),
            Err(err) => self.tx.send(AppEvent::NotifyError(err.to_string())),
        }
    }

    pub fn refresh(&self) {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        let status_context = StatusRefreshViewContext {
            selected: self.selected,
        };
        self.tx.send(AppEvent::Refresh(RefreshViewContext::Status {
            list_context,
            status_context,
        }));
    }
}

fn status_flags(entry: &StatusEntry) -> &'static str {
    match (entry.staged, entry.unstaged, entry.untracked) {
        (_, _, true) => "??",
        (true, true, false) => "SM",
        (true, false, false) => "S ",
        (false, true, false) => " M",
        _ => "  ",
    }
}

fn repo_status_metadata(ctx: &AppContext) -> (String, String) {
    let default_prefix = ctx.core_config.git_helper.branch_prefix.clone();
    let Some(git_dir) = git::get_git_dir(std::path::Path::new(".")) else {
        return ("unset".into(), default_prefix);
    };
    let Ok(state) = load_repo_state(&git_dir) else {
        return ("unset".into(), default_prefix);
    };
    let current_branch =
        git::get_current_branch(std::path::Path::new(".")).unwrap_or_else(|| "detached".into());
    let base = state
        .get_branch_origin(&current_branch)
        .unwrap_or("unset")
        .to_string();
    let prefix = state
        .get_branch_prefix()
        .map(str::to_string)
        .unwrap_or(default_prefix);
    (base, prefix)
}
