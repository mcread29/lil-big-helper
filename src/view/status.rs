use std::{cmp::min, collections::BTreeMap, path::Path, rc::Rc};

use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{self, StatusEntry},
    repo_state::load_repo_state,
    view::{ListRefreshViewContext, RefreshViewContext, StatusRefreshViewContext},
    widget::commit_list::CommitListState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    Files,
    Diff,
    Title,
    Description,
    Button,
}

impl Default for FocusArea {
    fn default() -> Self {
        Self::Files
    }
}

#[derive(Debug, Clone)]
struct TreeRow {
    label: String,
    entry_index: Option<usize>,
}

#[derive(Debug, Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    entry_index: Option<usize>,
}

#[derive(Debug, Default)]
struct StatusUiState {
    file_offset: usize,
    diff_offset: usize,
    focus: FocusArea,
    title: Input,
    description: Input,
}

#[derive(Debug)]
pub struct StatusView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    entries: Vec<StatusEntry>,
    tree_rows: Vec<TreeRow>,
    selected: usize,
    diff_lines: Vec<String>,
    ui: StatusUiState,
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
        let mut view = Self {
            commit_list_state: Some(commit_list_state),
            entries,
            tree_rows: Vec::new(),
            selected: 0,
            diff_lines: Vec::new(),
            ui: StatusUiState {
                focus: FocusArea::Files,
                ..StatusUiState::default()
            },
            ctx,
            tx,
        };
        view.rebuild_tree_rows();
        view.refresh_diff();
        view
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.cycle_focus(matches!(key.code, KeyCode::BackTab));
            return;
        }

        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => self.tx.send(AppEvent::Quit),
            UserEvent::Cancel | UserEvent::Close => self.tx.send(AppEvent::CloseStatus),
            UserEvent::HelpToggle => self.tx.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.refresh(),
            UserEvent::StatusCommit => self.commit_from_form(),
            _ => self.handle_focus_event(event_with_count, key, count),
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [upper_area, lower_area] =
            Layout::vertical([Constraint::Min(8), Constraint::Length(7)]).areas(area);
        let [sidebar_area, diff_area] =
            Layout::horizontal([Constraint::Length(36), Constraint::Min(0)]).areas(upper_area);

        self.render_file_tree(f, sidebar_area);
        self.render_diff(f, diff_area);
        self.render_commit_form(f, lower_area);
    }

    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn reset_status_with(&mut self, ctx: StatusRefreshViewContext) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.ui.file_offset = 0;
        } else {
            self.selected = ctx.selected.min(self.entries.len().saturating_sub(1));
            self.ui.file_offset = self.selected.saturating_sub(1);
        }
        self.rebuild_tree_rows();
        self.refresh_diff();
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

    fn handle_focus_event(
        &mut self,
        event_with_count: UserEventWithCount,
        key: KeyEvent,
        count: usize,
    ) {
        match self.ui.focus {
            FocusArea::Files => match event_with_count.event {
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
                _ => {}
            },
            FocusArea::Diff => match event_with_count.event {
                UserEvent::NavigateDown | UserEvent::ScrollDown => {
                    self.ui.diff_offset = self.ui.diff_offset.saturating_add(count);
                }
                UserEvent::NavigateUp | UserEvent::ScrollUp => {
                    self.ui.diff_offset = self.ui.diff_offset.saturating_sub(count);
                }
                UserEvent::PageDown | UserEvent::HalfPageDown => {
                    self.ui.diff_offset = self.ui.diff_offset.saturating_add(10 * count);
                }
                UserEvent::PageUp | UserEvent::HalfPageUp => {
                    self.ui.diff_offset = self.ui.diff_offset.saturating_sub(10 * count);
                }
                UserEvent::GoToTop => self.ui.diff_offset = 0,
                UserEvent::GoToBottom => {
                    self.ui.diff_offset = self.diff_lines.len().saturating_sub(1);
                }
                _ => {}
            },
            FocusArea::Title => Self::handle_text_input(&mut self.ui.title, key),
            FocusArea::Description => Self::handle_text_input(&mut self.ui.description, key),
            FocusArea::Button => {
                if matches!(
                    event_with_count.event,
                    UserEvent::Confirm | UserEvent::StatusCommit
                ) {
                    self.commit_from_form();
                }
            }
        }
    }

    fn handle_text_input(input: &mut Input, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        input.handle_event(&Event::Key(key));
    }

    fn cycle_focus(&mut self, reverse: bool) {
        self.ui.focus = match (self.ui.focus, reverse) {
            (FocusArea::Files, false) => FocusArea::Diff,
            (FocusArea::Diff, false) => FocusArea::Title,
            (FocusArea::Title, false) => FocusArea::Description,
            (FocusArea::Description, false) => FocusArea::Button,
            (FocusArea::Button, false) => FocusArea::Files,
            (FocusArea::Files, true) => FocusArea::Button,
            (FocusArea::Diff, true) => FocusArea::Files,
            (FocusArea::Title, true) => FocusArea::Diff,
            (FocusArea::Description, true) => FocusArea::Title,
            (FocusArea::Button, true) => FocusArea::Description,
        };
    }

    fn render_file_tree(&mut self, f: &mut Frame, area: Rect) {
        let visible_height = area.height.saturating_sub(2) as usize;
        let selected_row = self.row_index_for_selected();
        if selected_row < self.ui.file_offset {
            self.ui.file_offset = selected_row;
        } else if selected_row >= self.ui.file_offset + visible_height && visible_height > 0 {
            self.ui.file_offset = selected_row.saturating_sub(visible_height - 1);
        }

        let lines = self
            .tree_rows
            .iter()
            .enumerate()
            .skip(self.ui.file_offset)
            .take(visible_height)
            .map(|(_, row)| {
                let is_selected = row.entry_index == Some(self.selected);
                let mut line = Line::raw(row.label.clone());
                if is_selected {
                    line = line
                        .fg(self.ctx.color_theme.ref_selected_fg)
                        .bg(self.ctx.color_theme.ref_selected_bg);
                } else if row.entry_index.is_none() {
                    line = line
                        .fg(self.ctx.color_theme.detail_label_fg)
                        .add_modifier(Modifier::BOLD);
                } else if let Some(entry_index) = row.entry_index {
                    line = line.fg(status_color(&self.entries[entry_index], &self.ctx));
                    if entry_index == self.selected {
                        line = line
                            .fg(self.ctx.color_theme.ref_selected_fg)
                            .bg(self.ctx.color_theme.ref_selected_bg);
                    }
                }
                if is_selected && self.ui.focus == FocusArea::Files && !self.entries.is_empty() {
                    line = line.add_modifier(Modifier::BOLD);
                }
                line
            })
            .collect::<Vec<_>>();

        let selected_path = self
            .entries
            .get(self.selected)
            .map(|entry| entry.path.as_str())
            .unwrap_or("clean");
        let title = format!(
            "Changes [{}]{} {}",
            self.entries.len(),
            if self.ui.focus == FocusArea::Files {
                " *"
            } else {
                ""
            },
            selected_path
        );
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);
    }

    fn render_diff(&mut self, f: &mut Frame, area: Rect) {
        let visible_height = area.height.saturating_sub(2) as usize;
        let offset = min(
            self.ui.diff_offset,
            self.diff_lines.len().saturating_sub(visible_height),
        );
        self.ui.diff_offset = offset;
        let lines = self
            .diff_lines
            .iter()
            .skip(offset)
            .take(visible_height)
            .map(|line| style_diff_line(line, &self.ctx))
            .collect::<Vec<_>>();

        let title = format!(
            "Diff{}",
            if self.ui.focus == FocusArea::Diff {
                " *"
            } else {
                ""
            }
        );
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);
    }

    fn render_commit_form(&self, f: &mut Frame, area: Rect) {
        let [title_area, desc_area, button_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(3),
        ])
        .areas(area);
        let current_branch =
            git::get_current_branch(Path::new(".")).unwrap_or_else(|| "detached".into());
        let (base_branch, prefix) = repo_status_metadata(&self.ctx);

        let title_label = format!(
            "Title{}: {}",
            if self.ui.focus == FocusArea::Title {
                " *"
            } else {
                ""
            },
            self.ui.title.value()
        );
        let desc_label = format!(
            "Desc{}: {}",
            if self.ui.focus == FocusArea::Description {
                " *"
            } else {
                ""
            },
            self.ui.description.value()
        );
        let button_label = if self.ui.focus == FocusArea::Button {
            "[ Commit ]"
        } else {
            "Commit"
        };

        let staged_count = self.entries.iter().filter(|entry| entry.staged).count();
        let header = format!(
            "Commit  branch:{current_branch}  base:{base_branch}  prefix:{prefix}  staged:{staged_count}"
        );
        f.render_widget(
            Paragraph::new(Line::raw(title_label)).block(
                Block::default()
                    .title(header)
                    .borders(Borders::ALL)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::horizontal(1)),
            ),
            title_area,
        );
        f.render_widget(
            Paragraph::new(Line::raw(desc_label)).block(
                Block::default()
                    .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::horizontal(1)),
            ),
            desc_area,
        );

        let button_line = if self.ui.focus == FocusArea::Button {
            Line::raw(button_label)
                .fg(self.ctx.color_theme.ref_selected_fg)
                .bg(self.ctx.color_theme.ref_selected_bg)
        } else {
            Line::raw(button_label).fg(self.ctx.color_theme.status_success_fg)
        };
        f.render_widget(
            Paragraph::new(button_line).block(
                Block::default()
                    .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::horizontal(1)),
            ),
            button_area,
        );
    }

    fn row_index_for_selected(&self) -> usize {
        self.tree_rows
            .iter()
            .position(|row| row.entry_index == Some(self.selected))
            .unwrap_or(0)
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            self.refresh_diff();
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.refresh_diff();
        }
    }

    fn select_first(&mut self) {
        self.selected = 0;
        self.refresh_diff();
    }

    fn select_last(&mut self) {
        if !self.entries.is_empty() {
            self.selected = self.entries.len() - 1;
            self.refresh_diff();
        }
    }

    fn toggle_selected(&self) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        let result = if entry.untracked || entry.unstaged {
            git::stage_path(Path::new("."), &entry.path)
        } else if entry.staged {
            git::unstage_path(Path::new("."), &entry.path)
        } else {
            Ok(())
        };

        match result {
            Ok(()) => self.refresh(),
            Err(err) => self.tx.send(AppEvent::NotifyError(err.to_string())),
        }
    }

    fn commit_from_form(&self) {
        if !git::has_staged_changes(Path::new(".")) {
            self.tx
                .send(AppEvent::NotifyError("No staged changes to commit".into()));
            return;
        }

        let current_branch =
            git::get_current_branch(Path::new(".")).unwrap_or_else(|| "detached".into());
        if self
            .ctx
            .core_config
            .git_helper
            .protected_base_branches
            .iter()
            .any(|branch| branch == &current_branch)
        {
            self.tx.send(AppEvent::NotifyError(format!(
                "Direct commits to protected branch '{current_branch}' are blocked"
            )));
            return;
        }

        let title = self.ui.title.value().trim();
        if title.is_empty() {
            self.tx
                .send(AppEvent::NotifyError("Commit title cannot be empty".into()));
            return;
        }

        let desc = self.ui.description.value().trim();
        let message = if desc.is_empty() {
            title.to_string()
        } else {
            format!("{title}\n\n{desc}")
        };

        match git::commit_staged_changes(Path::new("."), &message) {
            Ok(()) => self.refresh(),
            Err(err) => self.tx.send(AppEvent::NotifyError(err.to_string())),
        }
    }

    fn rebuild_tree_rows(&mut self) {
        self.tree_rows.clear();
        if self.entries.is_empty() {
            self.tree_rows.push(TreeRow {
                label: "No changes".into(),
                entry_index: None,
            });
            return;
        }

        let mut root = TreeNode::default();
        for (entry_index, entry) in self.entries.iter().enumerate() {
            insert_entry(&mut root, &entry.path, entry_index);
        }
        build_tree_rows(&mut self.tree_rows, &root, "", true, &self.entries);
    }

    fn refresh_diff(&mut self) {
        self.ui.diff_offset = 0;
        let Some(entry) = self.entries.get(self.selected) else {
            self.diff_lines = vec!["No file selected".into()];
            return;
        };
        self.diff_lines = git::get_status_diff(Path::new("."), entry)
            .unwrap_or_else(|err| format!("Failed to load diff: {err}"))
            .lines()
            .map(|line| line.to_string())
            .collect();
        if self.diff_lines.is_empty() {
            self.diff_lines.push("No diff available".into());
        }
    }
}

fn repo_status_metadata(ctx: &AppContext) -> (String, String) {
    let default_prefix = ctx.core_config.git_helper.branch_prefix.clone();
    let Some(git_dir) = git::get_git_dir(Path::new(".")) else {
        return ("unset".into(), default_prefix);
    };
    let Ok(state) = load_repo_state(&git_dir) else {
        return ("unset".into(), default_prefix);
    };
    let current_branch =
        git::get_current_branch(Path::new(".")).unwrap_or_else(|| "detached".into());
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

fn status_flags(entry: &StatusEntry) -> &'static str {
    match (entry.staged, entry.unstaged, entry.untracked) {
        (_, _, true) => "??",
        (true, true, false) => "SM",
        (true, false, false) => "S ",
        (false, true, false) => " M",
        _ => "  ",
    }
}

fn status_color(entry: &StatusEntry, ctx: &AppContext) -> ratatui::style::Color {
    if entry.untracked {
        ctx.color_theme.status_warn_fg
    } else if entry.staged && entry.unstaged {
        ctx.color_theme.list_ref_tag_fg
    } else if entry.staged {
        ctx.color_theme.status_success_fg
    } else {
        ctx.color_theme.status_error_fg
    }
}

fn insert_entry(root: &mut TreeNode, path: &str, entry_index: usize) {
    let mut node = root;
    for component in path.split('/') {
        node = node.children.entry(component.to_string()).or_default();
    }
    node.entry_index = Some(entry_index);
}

fn build_tree_rows(
    rows: &mut Vec<TreeRow>,
    node: &TreeNode,
    prefix: &str,
    is_root: bool,
    entries: &[StatusEntry],
) {
    let child_count = node.children.len();
    for (position, (name, child)) in node.children.iter().enumerate() {
        let is_last = position + 1 == child_count;
        let connector = if is_root {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };

        if let Some(entry_index) = child.entry_index {
            let state = status_flags(&entries[entry_index]);
            rows.push(TreeRow {
                label: format!("{prefix}{connector}[{state}] {name}"),
                entry_index: Some(entry_index),
            });
        } else {
            rows.push(TreeRow {
                label: format!("{prefix}{connector}{name}/"),
                entry_index: None,
            });
        }

        if !child.children.is_empty() {
            let next_prefix = if is_root {
                String::new()
            } else if is_last {
                format!("{prefix}   ")
            } else {
                format!("{prefix}│  ")
            };
            build_tree_rows(rows, child, &next_prefix, false, entries);
        }
    }
}

fn style_diff_line(line: &str, ctx: &AppContext) -> Line<'static> {
    if line.starts_with('+') && !line.starts_with("+++") {
        Line::raw(line.to_string()).fg(ctx.color_theme.status_success_fg)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Line::raw(line.to_string()).fg(ctx.color_theme.status_error_fg)
    } else if line.starts_with("@@") {
        Line::raw(line.to_string()).fg(ctx.color_theme.detail_label_fg)
    } else {
        Line::raw(line.to_string())
    }
}
