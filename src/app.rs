use std::rc::Rc;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, Paragraph},
    DefaultTerminal, Frame,
};
use rustc_hash::FxHashMap;
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CursorType, GitHelperConfig, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_user_command, exec_user_command_suspend, ExternalCommandParameters,
    },
    git::{self, Commit, FileChange, Head, Ref, Repository},
    graph::{CellWidthType, Graph, GraphImageManager},
    keybind::KeyBind,
    protection,
    protocol::ImageProtocol,
    repo_state::{load_repo_state, save_repo_state},
    view::{RefreshViewContext, View},
    widget::{
        branch_visual::BranchVisuals,
        commit_list::{CommitInfo, CommitListState},
    },
};

#[derive(Debug, Default)]
enum StatusLine {
    #[default]
    None,
    Input(String, Option<u16>, Option<String>),
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
}

#[derive(Debug)]
enum PromptKind {
    ActionMenu,
    CreateBranchBase,
    CreateBranchSuffix { base_branch: String },
    SwitchBranch,
    SetBase,
    SetBranchPrefix,
}

#[derive(Debug)]
struct PromptState {
    kind: PromptKind,
    label: String,
    input: Input,
    transient: Option<String>,
    selector_options: Vec<String>,
    selector_index: usize,
}

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

pub enum Ret {
    Quit,
    Refresh(RefreshRequest),
}

pub struct RefreshRequest {
    pub context: RefreshViewContext,
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
}

#[derive(Debug, Default)]
struct AppStatus {
    status_line: StatusLine,
    numeric_prefix: String,
    view_area: Rect,
    prompt: Option<PromptState>,
}

#[derive(Debug)]
pub struct App<'a> {
    repository: &'a Repository,
    view: View<'a>,
    app_status: AppStatus,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
}

impl<'a> App<'a> {
    pub fn new(
        repository: &'a Repository,
        graph_image_manager: GraphImageManager<'a>,
        graph: &'a Graph,
        graph_color_set: &'a GraphColorSet,
        cell_width_type: CellWidthType,
        initial_selection: InitialSelection,
        ctx: Rc<AppContext>,
        ec: &'a EventController,
        refresh_view_context: Option<RefreshViewContext>,
    ) -> Self {
        let branch_visuals = BranchVisuals::new(repository, graph, graph_color_set).rc();
        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let commits = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let refs = repository.refs(&commit.commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name(), i);
                }
                let (pos_x, _) = graph.commit_pos_map[&commit.commit_hash];
                let graph_color = graph_color_set.get(pos_x).to_ratatui_color();
                CommitInfo::new(commit, refs, graph_color)
            })
            .collect();
        let graph_cell_width = match cell_width_type {
            CellWidthType::Double => (graph.max_pos_x + 1) as u16 * 2,
            CellWidthType::Single => (graph.max_pos_x + 1) as u16,
        };
        let head = repository.head();
        let mut commit_list_state = CommitListState::new(
            commits,
            graph_image_manager,
            graph_cell_width,
            head,
            branch_visuals,
            ref_name_to_commit_index_map,
            ctx.core_config.search.ignore_case,
            ctx.core_config.search.fuzzy,
        );
        if let InitialSelection::Head = initial_selection {
            match repository.head() {
                Head::Branch { name } => commit_list_state.select_ref(name),
                Head::Detached { target } => commit_list_state.select_commit_hash(target),
                Head::None => {}
            }
        }
        let refs = repository.all_refs().into_iter().cloned().collect();
        let view = View::of_list(commit_list_state, refs, ctx.clone(), ec.sender());

        let mut app = Self {
            repository,
            view,
            app_status: AppStatus::default(),
            ctx,
            ec,
        };

        if let Some(context) = refresh_view_context {
            app.init_with_context(context);
        }

        app
    }
}

impl App<'_> {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        // Clearing the screen here, as it should be cleared upon refresh
        self.clear_image(None)?;
        terminal.clear()?;

        loop {
            terminal.draw(|f| self.render(f))?;
            match self.ec.recv() {
                AppEvent::Key(key) => {
                    if self.app_status.prompt.is_some() {
                        self.handle_prompt_key(key);
                        continue;
                    }
                    match self.app_status.status_line {
                        StatusLine::None | StatusLine::Input(_, _, _) => {
                            // do nothing
                        }
                        StatusLine::NotificationInfo(_)
                        | StatusLine::NotificationSuccess(_)
                        | StatusLine::NotificationWarn(_) => {
                            // Clear message and pass key input as is
                            self.clear_status_line();
                        }
                        StatusLine::NotificationError(_) => {
                            // Clear message and cancel key input
                            self.clear_status_line();
                            continue;
                        }
                    }

                    let user_event = self.ctx.keybind.get(&key);

                    if self.view.captures_text_input() {
                        match user_event {
                            Some(UserEvent::ForceQuit) => {
                                self.ec.send(AppEvent::Quit);
                            }
                            Some(UserEvent::StatusCommit) => {
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::StatusCommit),
                                    key,
                                );
                            }
                            _ => {
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            }
                        }
                        continue;
                    }

                    if let Some(UserEvent::Cancel) = user_event {
                        if !self.app_status.numeric_prefix.is_empty() {
                            // Clear numeric prefix and cancel the event
                            self.app_status.numeric_prefix.clear();
                            continue;
                        }
                    }

                    match user_event {
                        Some(UserEvent::ForceQuit) => {
                            self.ec.send(AppEvent::Quit);
                        }
                        Some(ue) => {
                            let event_with_count =
                                process_numeric_prefix(&self.app_status.numeric_prefix, *ue, key);
                            self.view.handle_event(event_with_count, key);
                            self.app_status.numeric_prefix.clear();
                        }
                        None => {
                            if is_reverse_tab_key(key) {
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                                continue;
                            }
                            if let StatusLine::Input(_, _, _) = self.app_status.status_line {
                                // In input mode, pass all key events to the view
                                // fixme: currently, the only thing that processes key_event is searching the list,
                                //        so this probably works, but it's not the right process...
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            } else if let KeyCode::Char(c) = key.code {
                                // Accumulate numeric prefix
                                if c.is_ascii_digit()
                                    && (c != '0' || !self.app_status.numeric_prefix.is_empty())
                                {
                                    self.app_status.numeric_prefix.push(c);
                                }
                            }
                        }
                    }
                }
                AppEvent::Resize(w, h) => {
                    let _ = (w, h);
                }
                AppEvent::Quit => {
                    return Ok(Ret::Quit);
                }
                AppEvent::OpenActionMenu => {
                    self.open_action_menu();
                }
                AppEvent::OpenCreateBranchPrompt => {
                    self.open_create_branch_prompt();
                }
                AppEvent::OpenSwitchBranchPrompt => {
                    self.open_switch_branch_prompt();
                }
                AppEvent::OpenSetBasePrompt => {
                    self.open_set_base_prompt();
                }
                AppEvent::OpenSetBranchPrefixPrompt => {
                    self.open_set_branch_prefix_prompt();
                }
                AppEvent::OpenStatus => {
                    terminal.clear()?;
                    self.open_status();
                }
                AppEvent::CloseStatus => {
                    terminal.clear()?;
                    self.close_status();
                }
                AppEvent::OpenDetail => {
                    self.clear_image(Some(terminal))?;
                    self.open_detail();
                }
                AppEvent::CloseDetail => {
                    terminal.clear()?;
                    self.close_detail();
                }
                AppEvent::OpenUserCommand(n) => {
                    self.clear_image(Some(terminal))?;
                    self.open_user_command(n, Some(terminal));
                }
                AppEvent::CloseUserCommand => {
                    terminal.clear()?;
                    self.close_user_command();
                }
                AppEvent::OpenRefs => {
                    self.clear_image(None)?;
                    terminal.clear()?;
                    self.open_refs();
                }
                AppEvent::CloseRefs => {
                    self.clear_image(None)?;
                    terminal.clear()?;
                    self.close_refs();
                }
                AppEvent::OpenHelp => {
                    self.clear_image(None)?;
                    self.open_help();
                }
                AppEvent::CloseHelp => {
                    terminal.clear()?;
                    self.close_help();
                }
                AppEvent::SelectOlderCommit => {
                    self.select_older_commit();
                }
                AppEvent::SelectNewerCommit => {
                    self.select_newer_commit();
                }
                AppEvent::SelectParentCommit => {
                    self.select_parent_commit();
                }
                AppEvent::CopyToClipboard { name, value } => {
                    self.copy_to_clipboard(name, value);
                }
                AppEvent::Refresh(context) => {
                    let request = RefreshRequest { context };
                    return Ok(Ret::Refresh(request));
                }
                AppEvent::ClearStatusLine => {
                    self.clear_status_line();
                }
                AppEvent::UpdateStatusInput(msg, cursor_pos, msg_r) => {
                    self.update_status_input(msg, cursor_pos, msg_r);
                }
                AppEvent::NotifyInfo(msg) => {
                    self.info_notification(msg);
                }
                AppEvent::NotifySuccess(msg) => {
                    self.success_notification(msg);
                }
                AppEvent::NotifyWarn(msg) => {
                    self.warn_notification(msg);
                }
                AppEvent::NotifyError(msg) => {
                    self.error_notification(msg);
                }
                AppEvent::PushCurrentBranch => {
                    self.push_current_branch();
                }
                AppEvent::MergeBaseIntoCurrent => {
                    self.merge_base_into_current();
                }
                AppEvent::InstallHook => {
                    self.install_hook();
                }
            }
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let base = Block::default()
            .fg(self.ctx.color_theme.fg)
            .bg(self.ctx.color_theme.bg);
        f.render_widget(base, f.area());

        let [view_area, status_line_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());

        self.update_state(view_area);

        self.view.render(f, view_area);
        self.render_status_line(f, status_line_area);
    }
}

impl App<'_> {
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let text: Line = match &self.app_status.status_line {
            StatusLine::None => {
                if self.app_status.numeric_prefix.is_empty() {
                    Line::raw("")
                } else {
                    Line::raw(self.app_status.numeric_prefix.as_str())
                        .fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input(msg, _, transient_msg) => {
                let msg_w = console::measure_text_width(msg.as_str());
                if let Some(t_msg) = transient_msg {
                    let t_msg_w = console::measure_text_width(t_msg.as_str());
                    let pad_w = area.width as usize - msg_w - t_msg_w - 2 /* pad */;
                    Line::from(vec![
                        msg.as_str().fg(self.ctx.color_theme.status_input_fg),
                        " ".repeat(pad_w).into(),
                        t_msg
                            .as_str()
                            .fg(self.ctx.color_theme.status_input_transient_fg),
                    ])
                } else {
                    Line::raw(msg).fg(self.ctx.color_theme.status_input_fg)
                }
            }
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(format!("ERROR: {msg}"))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };
        let paragraph = Paragraph::new(text);
        f.render_widget(paragraph, area);

        if let StatusLine::Input(_, Some(cursor_pos), _) = &self.app_status.status_line {
            let (x, y) = (area.x + cursor_pos, area.y);
            match &self.ctx.ui_config.common.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
                }
            }
        }
    }
}

impl App<'_> {
    fn open_action_menu(&mut self) {
        self.open_prompt(
            PromptKind::ActionMenu,
            "Action [b:create s:switch t:status a:base f:prefix p:push m:merge i:hook r:refresh]"
                .into(),
            None,
            None,
        );
    }

    fn open_create_branch_prompt(&mut self) {
        if let Some(base_branch) = self.infer_base_branch_for_create() {
            let prefix = self.current_branch_prefix();
            self.open_prompt(
                PromptKind::CreateBranchSuffix { base_branch },
                format!("New branch suffix [{} / <name>]", prefix),
                None,
                None,
            );
            return;
        }

        let bases = self.git_helper_config().protected_base_branches.join(", ");
        self.open_prompt(
            PromptKind::CreateBranchBase,
            "Base branch".into(),
            Some(format!("Protected bases: {bases}")),
            None,
        );
    }

    fn open_switch_branch_prompt(&mut self) {
        let branches = git::get_local_branches(std::path::Path::new(".")).join(", ");
        self.open_prompt(
            PromptKind::SwitchBranch,
            "Switch branch".into(),
            if branches.is_empty() {
                None
            } else {
                Some(format!("Local branches: {branches}"))
            },
            None,
        );
    }

    fn open_set_base_prompt(&mut self) {
        let Some(branch) = git::get_current_branch(std::path::Path::new(".")) else {
            self.error_notification("Not on a branch".into());
            return;
        };
        let selector_options = git::get_local_branches(std::path::Path::new("."));
        if selector_options.is_empty() {
            self.error_notification("No local branches available to select as a base".into());
            return;
        }
        let current_base = self
            .load_repo_state()
            .ok()
            .and_then(|state| state.get_branch_origin(&branch).map(str::to_string))
            .unwrap_or_else(|| "unset".into());
        let selector_index = selector_options
            .iter()
            .position(|option| option == &current_base)
            .unwrap_or(0);
        let mut input = Input::default();
        if let Some(current) = selector_options.get(selector_index) {
            input = input.with_value(current.clone());
        }
        self.app_status.prompt = Some(PromptState {
            kind: PromptKind::SetBase,
            label: format!("Base branch for {branch}"),
            input,
            transient: Some(format!(
                "Current: {current_base}. Select any local branch with left/right or j/k."
            )),
            selector_options,
            selector_index,
        });
        self.refresh_prompt_status_line();
    }

    fn open_set_branch_prefix_prompt(&mut self) {
        let current_prefix = self.current_branch_prefix();
        let default_prefix = self.git_helper_config().branch_prefix.as_str();
        self.open_prompt(
            PromptKind::SetBranchPrefix,
            "Branch prefix".into(),
            Some(format!(
                "Current: {current_prefix}. Default: {default_prefix}. Empty resets to default."
            )),
            Some(current_prefix),
        );
    }

    fn open_prompt(
        &mut self,
        kind: PromptKind,
        label: String,
        transient: Option<String>,
        initial_value: Option<String>,
    ) {
        let mut input = Input::default();
        if let Some(value) = initial_value {
            input = input.with_value(value);
        }
        self.app_status.prompt = Some(PromptState {
            kind,
            label,
            input,
            transient,
            selector_options: Vec::new(),
            selector_index: 0,
        });
        self.refresh_prompt_status_line();
    }

    fn refresh_prompt_status_line(&mut self) {
        if let Some(prompt) = &self.app_status.prompt {
            if prompt.selector_options.is_empty() {
                let text = format!("{}: {}", prompt.label, prompt.input.value());
                let cursor = text.len() as u16;
                self.update_status_input(text, Some(cursor), prompt.transient.clone());
            } else {
                let current = prompt
                    .selector_options
                    .get(prompt.selector_index)
                    .map(String::as_str)
                    .unwrap_or("");
                let text = format!("{}: < {} >", prompt.label, current);
                self.update_status_input(text, None, prompt.transient.clone());
            }
        }
    }

    fn close_prompt(&mut self) {
        self.app_status.prompt = None;
        self.clear_status_line();
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_prompt();
            }
            KeyCode::Enter => {
                self.submit_prompt();
            }
            _ => {
                if let Some(prompt) = &mut self.app_status.prompt {
                    if prompt.selector_options.is_empty() {
                        prompt
                            .input
                            .handle_event(&ratatui::crossterm::event::Event::Key(key));
                    } else {
                        match key.code {
                            KeyCode::Left
                            | KeyCode::Up
                            | KeyCode::BackTab
                            | KeyCode::Char('h')
                            | KeyCode::Char('k') => {
                                if prompt.selector_index == 0 {
                                    prompt.selector_index = prompt.selector_options.len() - 1;
                                } else {
                                    prompt.selector_index -= 1;
                                }
                            }
                            KeyCode::Right
                            | KeyCode::Down
                            | KeyCode::Tab
                            | KeyCode::Char('j')
                            | KeyCode::Char('l') => {
                                prompt.selector_index =
                                    (prompt.selector_index + 1) % prompt.selector_options.len();
                            }
                            _ => {}
                        }
                    }
                    self.refresh_prompt_status_line();
                }
            }
        }
    }

    fn submit_prompt(&mut self) {
        let Some(prompt) = self.app_status.prompt.take() else {
            return;
        };
        self.clear_status_line();

        let value = if prompt.selector_options.is_empty() {
            prompt.input.value().trim().to_string()
        } else {
            prompt
                .selector_options
                .get(prompt.selector_index)
                .cloned()
                .unwrap_or_default()
        };
        match prompt.kind {
            PromptKind::ActionMenu => self.submit_action_menu(value.as_str()),
            PromptKind::CreateBranchBase => {
                if !self
                    .git_helper_config()
                    .protected_base_branches
                    .iter()
                    .any(|branch| branch == &value)
                {
                    self.error_notification(format!("Unknown protected base branch '{value}'"));
                    return;
                }
                self.open_prompt(
                    PromptKind::CreateBranchSuffix { base_branch: value },
                    format!(
                        "New branch suffix [{} / <name>]",
                        self.current_branch_prefix()
                    ),
                    None,
                    None,
                );
            }
            PromptKind::CreateBranchSuffix { base_branch } => {
                if let Err(err) = self.create_branch_from_base(&base_branch, value.as_str()) {
                    self.error_notification(err);
                }
            }
            PromptKind::SwitchBranch => {
                if let Err(err) = self.switch_branch(value.as_str()) {
                    self.error_notification(err);
                }
            }
            PromptKind::SetBase => {
                if let Err(err) = self.set_current_branch_base(value.as_str()) {
                    self.error_notification(err);
                }
            }
            PromptKind::SetBranchPrefix => {
                if let Err(err) = self.set_branch_prefix_override(value.as_str()) {
                    self.error_notification(err);
                }
            }
        }
    }

    fn submit_action_menu(&mut self, value: &str) {
        match value.chars().next() {
            Some('b') => self.open_create_branch_prompt(),
            Some('s') => self.open_switch_branch_prompt(),
            Some('t') => self.open_status(),
            Some('a') => self.open_set_base_prompt(),
            Some('f') => self.open_set_branch_prefix_prompt(),
            Some('p') => self.push_current_branch(),
            Some('m') => self.merge_base_into_current(),
            Some('i') => self.install_hook(),
            Some('r') => self.view.refresh(),
            Some(other) => self.error_notification(format!("Unknown action '{other}'")),
            None => self.error_notification("No action selected".into()),
        }
    }

    fn git_helper_config(&self) -> &GitHelperConfig {
        &self.ctx.core_config.git_helper
    }

    fn load_repo_state(&self) -> Result<crate::repo_state::RepoState, String> {
        let git_dir = git::get_git_dir(std::path::Path::new("."))
            .ok_or_else(|| "Failed to resolve git dir".to_string())?;
        load_repo_state(&git_dir).map_err(|err| err.to_string())
    }

    fn save_repo_state(&self, state: &crate::repo_state::RepoState) -> Result<(), String> {
        let git_dir = git::get_git_dir(std::path::Path::new("."))
            .ok_or_else(|| "Failed to resolve git dir".to_string())?;
        save_repo_state(&git_dir, state).map_err(|err| err.to_string())
    }

    fn current_branch_prefix(&self) -> String {
        self.load_repo_state()
            .ok()
            .and_then(|state| state.get_branch_prefix().map(str::to_string))
            .unwrap_or_else(|| self.git_helper_config().branch_prefix.clone())
    }

    fn infer_base_branch_for_create(&self) -> Option<String> {
        let protected = &self.git_helper_config().protected_base_branches;
        if let Some(selected) = self.selected_commit_hash() {
            for reference in self.repository.refs(&selected) {
                if let Ref::Branch { name, .. } = reference {
                    if protected.iter().any(|branch| branch == name) {
                        return Some(name.clone());
                    }
                }
            }
        }

        if let Some(current) = git::get_current_branch(std::path::Path::new(".")) {
            if protected.iter().any(|branch| branch == &current) {
                return Some(current);
            }
        }

        if protected.len() == 1 {
            return protected.first().cloned();
        }

        None
    }

    fn selected_commit_hash(&self) -> Option<crate::git::CommitHash> {
        match &self.view {
            View::List(view) => Some(view.as_list_state().selected_commit_hash().clone()),
            View::Detail(view) => Some(view.as_list_state().selected_commit_hash().clone()),
            View::UserCommand(view) => Some(view.as_list_state().selected_commit_hash().clone()),
            _ => None,
        }
    }

    fn ensure_base_worktree(&self, base_branch: &str) -> Result<std::path::PathBuf, String> {
        let repo_root = git::get_repo_root(std::path::Path::new("."))
            .ok_or_else(|| "Failed to resolve repository root".to_string())?;
        let worktree_path = repo_root
            .join(&self.git_helper_config().hidden_worktree_dir)
            .join(base_branch);

        if !worktree_path.exists() {
            if let Some(parent) = worktree_path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            git::add_hidden_base_worktree(std::path::Path::new("."), &worktree_path, base_branch)
                .map_err(|err| err.to_string())?;
        }

        if git::get_current_branch(&worktree_path).as_deref() != Some(base_branch) {
            return Err(format!(
                "Hidden worktree '{}' is not checked out to '{}'",
                worktree_path.display(),
                base_branch
            ));
        }

        if git::is_dirty(&worktree_path) {
            return Err(format!(
                "Hidden worktree '{}' is dirty",
                worktree_path.display()
            ));
        }

        git::refresh_hidden_base_worktree(&worktree_path, base_branch)
            .map_err(|err| err.to_string())?;
        Ok(worktree_path)
    }

    fn create_branch_from_base(&mut self, base_branch: &str, suffix: &str) -> Result<(), String> {
        let suffix = suffix.trim().trim_matches('/');
        if suffix.is_empty() {
            return Err("Branch suffix cannot be empty".into());
        }

        self.ensure_base_worktree(base_branch)?;

        let branch_name = format!("{}/{}", self.current_branch_prefix(), suffix);
        git::create_branch(std::path::Path::new("."), &branch_name, base_branch)
            .map_err(|err| err.to_string())?;
        git::switch_branch(std::path::Path::new("."), &branch_name)
            .map_err(|err| err.to_string())?;

        let mut state = self.load_repo_state()?;
        state.set_branch_origin(&branch_name, base_branch);
        self.save_repo_state(&state)?;

        self.view.refresh();
        Ok(())
    }

    fn switch_branch(&mut self, branch: &str) -> Result<(), String> {
        let branch = branch.trim();
        if branch.is_empty() {
            return Err("Branch name cannot be empty".into());
        }

        if git::is_dirty(std::path::Path::new(".")) {
            let current = git::get_current_branch(std::path::Path::new("."))
                .unwrap_or_else(|| "detached".into());
            let message = format!(
                "{}: {} -> {}",
                self.git_helper_config().stash_message_prefix,
                current,
                branch
            );
            git::stash_push(std::path::Path::new("."), &message).map_err(|err| err.to_string())?;
        }

        git::switch_branch(std::path::Path::new("."), branch).map_err(|err| err.to_string())?;
        self.view.refresh();
        Ok(())
    }

    fn push_current_branch(&mut self) {
        let result = (|| -> Result<(), String> {
            let branch = git::get_current_branch(std::path::Path::new("."))
                .ok_or_else(|| "Not on a branch".to_string())?;
            let set_upstream = git::get_upstream_branch(std::path::Path::new(".")).is_none()
                && self.git_helper_config().auto_set_upstream_on_first_push;
            git::push(std::path::Path::new("."), "origin", &branch, set_upstream)
                .map_err(|err| err.to_string())?;
            Ok(())
        })();

        match result {
            Ok(()) => self.view.refresh(),
            Err(err) => self.error_notification(err),
        }
    }

    fn merge_base_into_current(&mut self) {
        let result = (|| -> Result<(), String> {
            if git::is_dirty(std::path::Path::new(".")) {
                return Err("Worktree is dirty; stash or commit before merging base".into());
            }

            let branch = git::get_current_branch(std::path::Path::new("."))
                .ok_or_else(|| "Not on a branch".to_string())?;
            let state = self.load_repo_state()?;
            let base_branch = state
                .get_branch_origin(&branch)
                .ok_or_else(|| format!("No base branch recorded for '{branch}'"))?
                .to_string();

            self.ensure_base_worktree(&base_branch)?;
            git::merge_base_into_current(std::path::Path::new("."), &base_branch)
                .map_err(|err| err.to_string())?;
            Ok(())
        })();

        match result {
            Ok(()) => self.view.refresh(),
            Err(err) => self.error_notification(err),
        }
    }

    fn install_hook(&mut self) {
        let result = (|| -> Result<(), String> {
            let git_dir = git::get_git_dir(std::path::Path::new("."))
                .ok_or_else(|| "Failed to resolve git dir".to_string())?;
            protection::install_or_update_pre_commit_hook(&git_dir, self.git_helper_config())
                .map_err(|err| err.to_string())
        })();

        match result {
            Ok(()) => self.success_notification("Installed lil-big-helper pre-commit hook".into()),
            Err(err) => self.error_notification(err),
        }
    }

    fn set_current_branch_base(&mut self, base_branch: &str) -> Result<(), String> {
        let base_branch = base_branch.trim();
        if !git::get_local_branches(std::path::Path::new("."))
            .iter()
            .any(|branch| branch == base_branch)
        {
            return Err(format!("Unknown local branch '{base_branch}'"));
        }

        let current_branch = git::get_current_branch(std::path::Path::new("."))
            .ok_or_else(|| "Not on a branch".to_string())?;
        let mut state = self.load_repo_state()?;
        state.set_branch_origin(&current_branch, base_branch);
        self.save_repo_state(&state)?;
        self.success_notification(format!(
            "Base branch for '{current_branch}' set to '{base_branch}'"
        ));
        self.view.refresh();
        Ok(())
    }

    fn set_branch_prefix_override(&mut self, prefix: &str) -> Result<(), String> {
        let prefix = prefix.trim().trim_matches('/');
        let mut state = self.load_repo_state()?;
        if prefix.is_empty() {
            state.set_branch_prefix(None);
            self.save_repo_state(&state)?;
            self.success_notification(format!(
                "Branch prefix reset to default '{}'",
                self.git_helper_config().branch_prefix
            ));
            return Ok(());
        }

        state.set_branch_prefix(Some(prefix));
        self.save_repo_state(&state)?;
        self.success_notification(format!("Branch prefix set to '{prefix}'"));
        self.view.refresh();
        Ok(())
    }

    fn update_state(&mut self, view_area: Rect) {
        self.app_status.view_area = view_area;
    }

    fn clear_image(&self, terminal: Option<&mut DefaultTerminal>) -> Result<(), std::io::Error> {
        // Sometimes the first image fails to render after a full screen clear
        // As a workaround, the first area is preserved when a full clear is not required
        if let Some(t) = terminal {
            for y in 1..t.size()?.height {
                self.ctx.image_protocol.clear_line(y);
            }
        } else {
            self.ctx.image_protocol.clear();
        }
        Ok(())
    }

    fn open_detail(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let (commit, changes, refs) = selected_commit_details(self.repository, &commit_list_state);
        self.view = View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let refs = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_list(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_status(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            View::Refs(ref mut view) => view.take_list_state(),
            View::Status(_) | View::Help(_) | View::Default => return,
        };
        let entries = git::get_status_entries(std::path::Path::new("."));
        self.view = View::of_status(
            commit_list_state,
            entries,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_status(&mut self) {
        if let View::Status(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let refs = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_list(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_user_command(
        &mut self,
        user_command_number: usize,
        terminal: Option<&mut DefaultTerminal>,
    ) {
        let clear = match extract_user_command_by_number(user_command_number, &self.ctx)
            .map(|c| &c.r#type)
        {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
                false
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
                true
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
                true
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
                false
            }
        };
        if clear {
            if let Some(t) = terminal {
                if let Err(err) = t.clear() {
                    let msg = format!("Failed to clear terminal: {err:?}");
                    self.ec.send(AppEvent::NotifyError(msg));
                }
            }
        }
    }

    fn open_user_command_inline(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(output) => {
                // take list state only when the command execution is successful, to avoid losing the state when the command fails
                let commit_list_state = match self.view {
                    View::List(ref mut view) => view.take_list_state(),
                    View::Detail(ref mut view) => view.take_list_state(),
                    View::UserCommand(ref mut view) => view.take_list_state(),
                    _ => return,
                };
                self.view = View::of_user_command(
                    commit_list_state,
                    output,
                    user_command_number,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_silent(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(_) => {
                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn open_user_command_suspend(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.ec.suspend();
                let exec_result = exec_user_command_suspend(params);
                self.ec.resume();

                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }

                // notify after resuming and refreshing
                if let Err(err) = exec_result {
                    self.ec.send(AppEvent::NotifyError(err));
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn close_user_command(&mut self) {
        if let View::UserCommand(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let refs = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_list(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_refs(&mut self) {
        if let View::List(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let refs = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_refs(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn close_refs(&mut self) {
        if let View::Refs(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            let refs = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_list(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_help(&mut self) {
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_help(before_view, self.ctx.clone(), self.ec.sender());
    }

    fn close_help(&mut self) {
        if let View::Help(ref mut view) = self.view {
            self.view = view.take_before_view();
        }
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_newer_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_parent_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_parent_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_parent_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail { .. } => {
                self.open_detail();
            }
            RefreshViewContext::UserCommand {
                user_command_context,
                ..
            } => {
                self.open_user_command(user_command_context.n, None);
            }
            RefreshViewContext::Refs { refs_context, .. } => {
                if let View::List(ref mut view) = self.view {
                    view.reset_refs_with(refs_context);
                }
            }
            RefreshViewContext::Status { status_context, .. } => {
                self.open_status();
                if let View::Status(ref mut view) = self.view {
                    view.reset_status_with(status_context);
                }
            }
        }
    }

    fn clear_status_line(&mut self) {
        self.app_status.status_line = StatusLine::None;
    }

    fn update_status_input(
        &mut self,
        msg: String,
        cursor_pos: Option<u16>,
        transient_msg: Option<String>,
    ) {
        self.app_status.status_line = StatusLine::Input(msg, cursor_pos, transient_msg);
    }

    fn info_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationInfo(msg);
    }

    fn success_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationSuccess(msg);
    }

    fn warn_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationWarn(msg);
    }

    fn error_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationError(msg);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        match copy_to_clipboard(value, &self.ctx.core_config.external.clipboard) {
            Ok(_) => {
                let msg = format!("Copied {name} to clipboard successfully");
                self.ec.send(AppEvent::NotifySuccess(msg));
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }
}

fn selected_commit_details(
    repository: &Repository,
    commit_list_state: &CommitListState,
) -> (Commit, Vec<FileChange>, Vec<Ref>) {
    let selected = commit_list_state.selected_commit_hash().clone();
    let (commit, changes) = repository.commit_detail(&selected);
    let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
    (commit, changes, refs)
}

fn process_numeric_prefix(
    numeric_prefix: &str,
    user_event: UserEvent,
    _key_event: KeyEvent,
) -> UserEventWithCount {
    if user_event.is_countable() {
        let count = if numeric_prefix.is_empty() {
            1
        } else {
            numeric_prefix.parse::<usize>().unwrap_or(1)
        };
        UserEventWithCount::new(user_event, count)
    } else {
        UserEventWithCount::from_event(user_event)
    }
}

fn is_reverse_tab_key(key: KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab
            && key
                .modifiers
                .contains(ratatui::crossterm::event::KeyModifiers::SHIFT))
}

fn extract_user_command_by_number(
    user_command_number: usize,
    ctx: &AppContext,
) -> Result<&UserCommand, String> {
    ctx.core_config
        .user_command
        .commands
        .get(&user_command_number.to_string())
        .ok_or_else(|| format!("No user command configured for number {user_command_number}",))
}

fn extract_user_command_refresh_by_number(user_command_number: usize, ctx: &AppContext) -> bool {
    extract_user_command_by_number(user_command_number, ctx)
        .map(|c| c.refresh)
        .unwrap_or_default()
}

fn build_external_command_parameters_and_exec_command(
    commit: &Commit,
    refs: &[Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &AppContext,
) -> Result<String, String> {
    build_external_command_parameters(commit, refs, user_command_number, view_area, ctx)
        .and_then(exec_user_command)
}

fn build_external_command_parameters<'a>(
    commit: &'a Commit,
    refs: &'a [Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &'a AppContext,
) -> Result<ExternalCommandParameters<'a>, String> {
    let command = &extract_user_command_by_number(user_command_number, ctx)?.commands;
    let target_hash = commit.commit_hash.as_str();
    let parent_hashes = commit
        .parent_commit_hashes
        .iter()
        .map(|c| c.as_str())
        .collect();

    let mut all_refs = vec![];
    let mut branches = vec![];
    let mut remote_branches = vec![];
    let mut tags = vec![];
    for r in refs {
        match r {
            Ref::Tag { .. } => tags.push(r.name()),
            Ref::Branch { .. } => branches.push(r.name()),
            Ref::RemoteBranch { .. } => remote_branches.push(r.name()),
            Ref::Stash { .. } => continue, // skip stashes
        }
        all_refs.push(r.name());
    }

    let area_width = view_area.width.saturating_sub(4); // minus the left and right padding
    let area_height = (view_area.height.saturating_sub(1))
        .min(ctx.ui_config.user_command.height)
        .saturating_sub(1); // minus the top border
    Ok(ExternalCommandParameters {
        command,
        target_hash,
        parent_hashes,
        all_refs,
        branches,
        remote_branches,
        tags,
        area_width,
        area_height,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rustfmt::skip]
    #[rstest]
    #[case("",    UserEvent::NavigateDown, UserEventWithCount::new(UserEvent::NavigateDown, 1))] // no prefix
    #[case("5",   UserEvent::NavigateUp,   UserEventWithCount::new(UserEvent::NavigateUp, 5))] // with prefix
    #[case("0",   UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 1))] // zero should be converted to 1
    #[case("42",  UserEvent::ScrollDown,   UserEventWithCount::new(UserEvent::ScrollDown, 42))] // multi-digit number
    #[case("999", UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 999))] // large number
    #[case("abc", UserEvent::ScrollUp,     UserEventWithCount::new(UserEvent::ScrollUp, 1))] // should fallback to 1
    #[case("5",   UserEvent::Quit,         UserEventWithCount::new(UserEvent::Quit, 1))] // non-countable event with prefix
    #[case("",    UserEvent::Confirm,      UserEventWithCount::new(UserEvent::Confirm, 1))] // non-countable event without prefix
    fn test_process_numeric_prefix(
        #[case] numeric_prefix: &str,
        #[case] user_event: UserEvent,
        #[case] expected: UserEventWithCount,
    ) {
        let dummy_key_event = KeyEvent::from(KeyCode::Enter); // KeyEvent is not used in the logic
        let actual = process_numeric_prefix(numeric_prefix, user_event, dummy_key_event);
        assert_eq!(actual, expected);
    }
}
