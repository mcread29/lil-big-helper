use std::{
    cmp::min,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    rc::Rc,
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{self, StatusEntry},
    view::{ListRefreshViewContext, RefreshViewContext, StatusRefreshViewContext},
    widget::commit_list::CommitListState,
    workflow::WorkflowAction,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    Files,
    DiscardButton,
    CommitButton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionState {
    None,
    Partial,
    Full,
}

impl Default for FocusArea {
    fn default() -> Self {
        Self::Files
    }
}

#[derive(Debug, Clone)]
struct TreeRow {
    path: String,
    is_dir: bool,
    is_expanded: bool,
    has_children: bool,
    label: String,
    entry_indexes: Vec<usize>,
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
    expanded_dirs: BTreeSet<String>,
    focus: FocusArea,
    selected_paths: BTreeSet<String>,
}

#[derive(Debug)]
pub struct StatusView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    entries: Vec<StatusEntry>,
    tree_rows: Vec<TreeRow>,
    selected_row: usize,
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
            selected_row: 0,
            diff_lines: Vec::new(),
            ui: StatusUiState::default(),
            ctx,
            tx,
        };
        view.ui.expanded_dirs = collect_directory_paths(&view.entries);
        view.rebuild_tree_rows();
        view.refresh_diff();
        view
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        if is_forward_tab(key) || is_reverse_tab(key) {
            self.cycle_focus(is_reverse_tab(key));
            return;
        }

        match event_with_count.event {
            UserEvent::Quit => self.tx.send(AppEvent::Quit),
            UserEvent::Cancel | UserEvent::Close => self.tx.send(AppEvent::CloseStatus),
            UserEvent::HelpToggle => self.tx.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.refresh(),
            UserEvent::StatusCommit => self.commit_selected(),
            UserEvent::StatusDiscard => self.discard_selected(),
            _ => self.handle_focus_event(event_with_count),
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [upper_area, lower_area] =
            Layout::vertical([Constraint::Min(8), Constraint::Length(3)]).areas(area);
        let [sidebar_area, diff_area] =
            Layout::horizontal([Constraint::Length(40), Constraint::Min(0)]).areas(upper_area);

        self.render_file_tree(f, sidebar_area);
        self.render_diff(f, diff_area);
        self.render_action_bar(f, lower_area);
    }

    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn captures_text_input(&self) -> bool {
        false
    }

    pub fn reset_status_with(&mut self, ctx: StatusRefreshViewContext) {
        if self.entries.is_empty() {
            self.selected_row = 0;
            self.ui.file_offset = 0;
        } else {
            self.selected_row = ctx.selected.min(self.tree_rows.len().saturating_sub(1));
            self.ui.file_offset = self.selected_row.saturating_sub(1);
        }
        self.rebuild_tree_rows();
        if self.selected_row >= self.tree_rows.len() {
            self.selected_row = self.tree_rows.len().saturating_sub(1);
        }
        self.refresh_diff();
    }

    pub fn refresh(&self) {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        let status_context = StatusRefreshViewContext {
            selected: self.selected_row,
        };
        self.tx.send(AppEvent::Refresh(RefreshViewContext::Status {
            list_context,
            status_context,
        }));
    }

    fn handle_focus_event(&mut self, event_with_count: UserEventWithCount) {
        let count = event_with_count.count;
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
                UserEvent::NavigateRight => self.expand_selected_dir(),
                UserEvent::NavigateLeft => self.collapse_selected_dir(),
                UserEvent::Confirm | UserEvent::StatusToggle => self.activate_selected(),
                _ => {}
            },
            FocusArea::DiscardButton => {
                if matches!(event_with_count.event, UserEvent::Confirm) {
                    self.discard_selected();
                }
            }
            FocusArea::CommitButton => {
                if matches!(event_with_count.event, UserEvent::Confirm) {
                    self.commit_selected();
                }
            }
        }
    }

    fn cycle_focus(&mut self, reverse: bool) {
        self.ui.focus = next_focus(self.ui.focus, reverse);
    }

    fn render_file_tree(&mut self, f: &mut Frame, area: Rect) {
        let visible_height = area.height.saturating_sub(2) as usize;
        let selected_row = self
            .selected_row
            .min(self.tree_rows.len().saturating_sub(1));
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
            .map(|(index, row)| {
                let is_cursor = index == selected_row;
                let selection_state =
                    selection_state_for_row(row, &self.entries, &self.ui.selected_paths);
                let marker = if row.entry_indexes.is_empty() {
                    "   "
                } else {
                    selection_marker(selection_state)
                };
                let prefix = if is_cursor { "›" } else { " " };
                let mut line = Line::raw(format!("{prefix} {marker} {}", row.label));
                if is_cursor {
                    let highlight_fg = if self.ui.focus == FocusArea::Files {
                        self.ctx.color_theme.ref_selected_fg
                    } else {
                        self.ctx.color_theme.divider_fg
                    };
                    let highlight_bg = if self.ui.focus == FocusArea::Files {
                        self.ctx.color_theme.ref_selected_bg
                    } else {
                        self.ctx.color_theme.bg
                    };
                    line = line.fg(highlight_fg).bg(highlight_bg);
                } else if !row.entry_indexes.is_empty() {
                    line = line.fg(status_color_for_indexes(
                        &row.entry_indexes,
                        &self.entries,
                        &self.ctx,
                    ));
                }
                if row.is_dir {
                    line = line.add_modifier(Modifier::BOLD);
                }
                if matches!(
                    selection_state,
                    SelectionState::Partial | SelectionState::Full
                )
                {
                    line = line.add_modifier(Modifier::BOLD);
                }
                line
            })
            .collect::<Vec<_>>();

        let title = format!("Changes [{}]", self.entries.len());
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(if self.ui.focus == FocusArea::Files {
                    self.ctx.color_theme.ref_selected_fg
                } else {
                    self.ctx.color_theme.divider_fg
                }))
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

        let selected_row = self.tree_rows.get(
            self.selected_row
                .min(self.tree_rows.len().saturating_sub(1)),
        );
        let title = match selected_row {
            Some(row) => format!("Diff {} ({})", row.path, row.entry_indexes.len()),
            None => "Diff".to_string(),
        };
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);
    }

    fn render_action_bar(&self, f: &mut Frame, area: Rect) {
        let [discard_area, commit_area] =
            Layout::horizontal([Constraint::Length(16), Constraint::Length(16)]).areas(area);
        let enabled = self.has_selected_entries();
        render_button(
            f,
            discard_area,
            "Discard",
            self.ui.focus == FocusArea::DiscardButton,
            enabled,
            self.ctx.color_theme.status_error_fg,
            self.ctx.color_theme.divider_fg,
            self.ctx.color_theme.ref_selected_fg,
        );
        render_button(
            f,
            commit_area,
            "Commit",
            self.ui.focus == FocusArea::CommitButton,
            enabled,
            self.ctx.color_theme.status_success_fg,
            self.ctx.color_theme.divider_fg,
            self.ctx.color_theme.ref_selected_fg,
        );
    }

    fn select_next(&mut self) {
        if self.selected_row + 1 < self.tree_rows.len() {
            self.selected_row += 1;
            self.refresh_diff();
        }
    }

    fn select_prev(&mut self) {
        if self.selected_row > 0 {
            self.selected_row -= 1;
            self.refresh_diff();
        }
    }

    fn select_first(&mut self) {
        self.selected_row = 0;
        self.refresh_diff();
    }

    fn select_last(&mut self) {
        if !self.tree_rows.is_empty() {
            self.selected_row = self.tree_rows.len() - 1;
            self.refresh_diff();
        }
    }

    fn activate_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_dir && row.has_children {
            self.toggle_dir_expanded(row.path.clone());
        }
        self.toggle_selected();
    }

    fn toggle_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let entry_paths = row_entry_paths(row, &self.entries);
        if entry_paths.is_empty() {
            return;
        }
        match selection_state_for_row(row, &self.entries, &self.ui.selected_paths) {
            SelectionState::Full => {
                for path in entry_paths {
                    self.ui.selected_paths.remove(&path);
                }
            }
            SelectionState::None | SelectionState::Partial => {
                for path in entry_paths {
                    self.ui.selected_paths.insert(path);
                }
            }
        }
    }

    fn discard_selected(&self) {
        let paths = selected_paths(&self.ui.selected_paths);
        if paths.is_empty() {
            self.tx
                .send(AppEvent::NotifyError("No selected paths to discard".into()));
            return;
        }
        self.tx
            .send(AppEvent::RunWorkflowAction(WorkflowAction::StatusDiscard {
                paths,
            }));
    }

    fn commit_selected(&self) {
        let paths = selected_paths(&self.ui.selected_paths);
        if paths.is_empty() {
            self.tx
                .send(AppEvent::NotifyError("No selected paths to commit".into()));
            return;
        }
        self.tx
            .send(AppEvent::RunWorkflowAction(WorkflowAction::StatusCommit {
                paths,
            }));
    }

    fn has_selected_entries(&self) -> bool {
        !self.ui.selected_paths.is_empty()
    }

    fn rebuild_tree_rows(&mut self) {
        self.tree_rows.clear();
        if self.entries.is_empty() {
            self.tree_rows.push(TreeRow {
                path: String::new(),
                is_dir: false,
                is_expanded: false,
                has_children: false,
                label: "No changes".into(),
                entry_indexes: Vec::new(),
            });
            return;
        }
        let mut root = TreeNode::default();
        for (entry_index, entry) in self.entries.iter().enumerate() {
            insert_entry(&mut root, &entry.path, entry_index);
        }
        build_tree_rows(
            &mut self.tree_rows,
            &root,
            "",
            "",
            true,
            &self.entries,
            &self.ui.expanded_dirs,
        );
    }

    fn refresh_diff(&mut self) {
        self.ui.diff_offset = 0;
        let Some(row) = self.selected_row() else {
            self.diff_lines = vec!["No selection".into()];
            return;
        };
        self.diff_lines = if row.entry_indexes.is_empty() {
            vec!["No diff available".into()]
        } else if row.entry_indexes.len() == 1 {
            let entry = &self.entries[row.entry_indexes[0]];
            git::get_status_diff(Path::new("."), entry)
                .unwrap_or_else(|err| format!("Failed to load diff: {err}"))
                .lines()
                .map(|line| line.to_string())
                .collect()
        } else {
            let mut lines = Vec::new();
            for (position, &index) in row.entry_indexes.iter().enumerate() {
                let entry = &self.entries[index];
                if position > 0 {
                    lines.push(String::new());
                }
                lines.push(format!("=== {} ===", entry.path));
                let diff = git::get_status_diff(Path::new("."), entry)
                    .unwrap_or_else(|err| format!("Failed to load diff: {err}"));
                if diff.trim().is_empty() {
                    lines.push("No diff available".into());
                } else {
                    lines.extend(diff.lines().map(|line| line.to_string()));
                }
            }
            lines
        };
        if self.diff_lines.is_empty() {
            self.diff_lines.push("No diff available".into());
        }
    }

    fn selected_row(&self) -> Option<&TreeRow> {
        self.tree_rows.get(self.selected_row)
    }

    fn toggle_dir_expanded(&mut self, path: String) {
        if !self.ui.expanded_dirs.insert(path.clone()) {
            self.ui.expanded_dirs.remove(&path);
        }
        self.rebuild_tree_rows();
        self.restore_selected_path(&path);
        self.refresh_diff();
    }

    fn expand_selected_dir(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if !(row.is_dir && row.has_children) || row.is_expanded {
            return;
        }
        let path = row.path.clone();
        self.ui.expanded_dirs.insert(path.clone());
        self.rebuild_tree_rows();
        self.restore_selected_path(&path);
        self.refresh_diff();
    }

    fn collapse_selected_dir(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_dir && row.has_children && row.is_expanded {
            let path = row.path.clone();
            self.ui.expanded_dirs.remove(&path);
            self.rebuild_tree_rows();
            self.restore_selected_path(&path);
            self.refresh_diff();
            return;
        }

        let Some((parent_path, _)) = row.path.rsplit_once('/') else {
            return;
        };
        let parent_path = parent_path.to_string();
        self.restore_selected_path(&parent_path);
        self.refresh_diff();
    }

    fn restore_selected_path(&mut self, path: &str) {
        if let Some(index) = self.tree_rows.iter().position(|row| row.path == path) {
            self.selected_row = index;
        } else {
            self.selected_row = self
                .selected_row
                .min(self.tree_rows.len().saturating_sub(1));
        }
    }
}

fn next_focus(current: FocusArea, reverse: bool) -> FocusArea {
    match (current, reverse) {
        (FocusArea::Files, false) => FocusArea::DiscardButton,
        (FocusArea::DiscardButton, false) => FocusArea::CommitButton,
        (FocusArea::CommitButton, false) => FocusArea::Files,
        (FocusArea::Files, true) => FocusArea::CommitButton,
        (FocusArea::DiscardButton, true) => FocusArea::Files,
        (FocusArea::CommitButton, true) => FocusArea::DiscardButton,
    }
}

fn render_button(
    f: &mut Frame,
    area: Rect,
    label: &str,
    focused: bool,
    enabled: bool,
    enabled_fg: ratatui::style::Color,
    border_fg: ratatui::style::Color,
    focus_border_fg: ratatui::style::Color,
) {
    let style = if enabled {
        Style::default().fg(enabled_fg)
    } else {
        Style::default().fg(border_fg)
    };
    f.render_widget(
        Paragraph::new(Line::raw(label))
            .alignment(Alignment::Center)
            .style(style)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .style(Style::default().fg(if focused {
                        focus_border_fg
                    } else {
                        border_fg
                    }))
                    .padding(Padding::horizontal(1)),
            ),
        area,
    );
}

fn selection_marker(state: SelectionState) -> &'static str {
    match state {
        SelectionState::None => "[ ]",
        SelectionState::Partial => "[/]",
        SelectionState::Full => "[x]",
    }
}

fn selection_state_for_row(
    row: &TreeRow,
    entries: &[StatusEntry],
    selected_paths: &BTreeSet<String>,
) -> SelectionState {
    let entry_paths = row_entry_paths(row, entries);
    if entry_paths.is_empty() {
        return SelectionState::None;
    }
    let selected_count = entry_paths
        .iter()
        .filter(|path| selected_paths.contains(*path))
        .count();
    if selected_count == 0 {
        SelectionState::None
    } else if selected_count == entry_paths.len() {
        SelectionState::Full
    } else {
        SelectionState::Partial
    }
}

fn row_entry_paths(row: &TreeRow, entries: &[StatusEntry]) -> Vec<String> {
    row.entry_indexes
        .iter()
        .filter_map(|index| entries.get(*index))
        .map(|entry| entry.path.clone())
        .collect()
}

fn is_forward_tab(key: KeyEvent) -> bool {
    key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT)
}

fn is_reverse_tab(key: KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn status_icon(entry: &StatusEntry) -> &'static str {
    if entry.deleted {
        "✖"
    } else if entry.untracked {
        "✚"
    } else {
        "●"
    }
}

fn status_icon_for_indexes(indexes: &[usize], entries: &[StatusEntry]) -> &'static str {
    let mut any_untracked = false;
    let mut any_deleted = false;
    let mut any_modified = false;
    for &index in indexes {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        any_untracked |= entry.untracked;
        any_deleted |= entry.deleted;
        any_modified |= !entry.untracked && !entry.deleted;
    }
    if any_deleted {
        "✖"
    } else if any_untracked {
        "✚"
    } else if any_modified {
        "●"
    } else {
        "·"
    }
}

fn status_color_for_indexes(
    indexes: &[usize],
    entries: &[StatusEntry],
    ctx: &AppContext,
) -> ratatui::style::Color {
    let mut any_untracked = false;
    let mut any_deleted = false;
    for &index in indexes {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        any_untracked |= entry.untracked;
        any_deleted |= entry.deleted;
    }

    if any_deleted {
        ctx.color_theme.status_error_fg
    } else if any_untracked {
        ctx.color_theme.status_success_fg
    } else {
        ctx.color_theme.list_ref_stash_fg
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
    visual_prefix: &str,
    path_prefix: &str,
    is_root: bool,
    entries: &[StatusEntry],
    expanded_dirs: &BTreeSet<String>,
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
        let path = if path_prefix.is_empty() {
            name.clone()
        } else {
            format!("{path_prefix}/{name}")
        };

        if let Some(entry_index) = child.entry_index {
            let state = status_icon(&entries[entry_index]);
            rows.push(TreeRow {
                path: path.clone(),
                is_dir: false,
                is_expanded: false,
                has_children: false,
                label: format!("{visual_prefix}{connector}{state} {name}"),
                entry_indexes: vec![entry_index],
            });
        } else {
            let entry_indexes = collect_entry_indexes(child);
            let has_children = !child.children.is_empty();
            let is_expanded = expanded_dirs.contains(&path);
            let expand_marker = if has_children {
                if is_expanded {
                    "▾"
                } else {
                    "▸"
                }
            } else {
                " "
            };
            let state = status_icon_for_indexes(&entry_indexes, entries);
            rows.push(TreeRow {
                path: path.clone(),
                is_dir: true,
                is_expanded,
                has_children,
                label: format!("{visual_prefix}{connector}{state} {expand_marker} {name}/"),
                entry_indexes,
            });
        }

        if !child.children.is_empty() && expanded_dirs.contains(&path) {
            let next_visual_prefix = if is_root {
                String::new()
            } else if is_last {
                format!("{visual_prefix}   ")
            } else {
                format!("{visual_prefix}│  ")
            };
            build_tree_rows(
                rows,
                child,
                &next_visual_prefix,
                &path,
                false,
                entries,
                expanded_dirs,
            );
        }
    }
}

fn collect_entry_indexes(node: &TreeNode) -> Vec<usize> {
    let mut indexes = Vec::new();
    if let Some(entry_index) = node.entry_index {
        indexes.push(entry_index);
    }
    for child in node.children.values() {
        indexes.extend(collect_entry_indexes(child));
    }
    indexes.sort_unstable();
    indexes
}

fn collect_directory_paths(entries: &[StatusEntry]) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let mut current = String::new();
        let mut components = entry.path.split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            if current.is_empty() {
                current.push_str(component);
            } else {
                current.push('/');
                current.push_str(component);
            }
            paths.insert(current.clone());
        }
    }
    paths
}

fn selected_paths(selected_paths: &BTreeSet<String>) -> Vec<String> {
    selected_paths.iter().cloned().collect()
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

#[cfg(test)]
mod tests {
    use super::{
        next_focus, row_entry_paths, selected_paths, selection_state_for_row, FocusArea,
        SelectionState, TreeRow,
    };
    use crate::git::StatusEntry;
    use std::collections::BTreeSet;

    #[test]
    fn focus_cycles_between_files_and_buttons() {
        assert_eq!(
            next_focus(FocusArea::Files, false),
            FocusArea::DiscardButton
        );
        assert_eq!(
            next_focus(FocusArea::DiscardButton, false),
            FocusArea::CommitButton
        );
        assert_eq!(next_focus(FocusArea::CommitButton, false), FocusArea::Files);
        assert_eq!(next_focus(FocusArea::Files, true), FocusArea::CommitButton);
    }

    #[test]
    fn folder_selection_becomes_partial_when_child_is_deselected() {
        let entries = vec![
            StatusEntry {
                path: "src/app.rs".into(),
                staged: false,
                unstaged: true,
                untracked: false,
                deleted: false,
            },
            StatusEntry {
                path: "src/view/status.rs".into(),
                staged: false,
                unstaged: true,
                untracked: false,
                deleted: false,
            },
            StatusEntry {
                path: "README.md".into(),
                staged: false,
                unstaged: true,
                untracked: false,
                deleted: false,
            },
        ];
        let src_row = TreeRow {
            path: "src".into(),
            is_dir: true,
            is_expanded: true,
            has_children: true,
            label: "src/".into(),
            entry_indexes: vec![0, 1],
        };
        let mut selected = BTreeSet::from_iter(row_entry_paths(&src_row, &entries));
        assert_eq!(
            selection_state_for_row(&src_row, &entries, &selected),
            SelectionState::Full
        );
        selected.remove("src/app.rs");
        assert_eq!(
            selection_state_for_row(&src_row, &entries, &selected),
            SelectionState::Partial
        );
        assert_eq!(
            selected_paths(&selected),
            vec!["src/view/status.rs".to_string()]
        );
    }
}
