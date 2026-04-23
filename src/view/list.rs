use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::CommitHash,
    git::Ref,
    view::{ListRefreshViewContext, RefreshViewContext, RefsRefreshViewContext},
    widget::{
        commit_list::{CommitList, CommitListState, SearchState},
        ref_list::{RefList, RefListState},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Sidebar,
    CommitList,
}

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,
    refs: Vec<Ref>,
    focus_pane: FocusPane,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ListView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ListView<'a> {
        let mut ref_list_state = RefListState::new();
        if let crate::git::Head::Branch { name } = commit_list_state.head() {
            ref_list_state.select_branch_name(&refs, name);
        }
        ListView {
            commit_list_state: Some(commit_list_state),
            ref_list_state,
            refs,
            focus_pane: FocusPane::CommitList,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;
        if self.focus_pane == FocusPane::Sidebar {
            self.handle_sidebar_event(event, count);
            return;
        }

        if let SearchState::Searching { .. } = self.as_list_state().search_state() {
            match event {
                UserEvent::Confirm => {
                    self.as_mut_list_state().apply_search();
                    self.update_matched_message();
                }
                UserEvent::Cancel => {
                    self.as_mut_list_state().cancel_search();
                    self.clear_search_query();
                }
                UserEvent::IgnoreCaseToggle => {
                    self.as_mut_list_state().toggle_ignore_case();
                    self.update_search_query();
                }
                UserEvent::FuzzyToggle => {
                    self.as_mut_list_state().toggle_fuzzy();
                    self.update_search_query();
                }
                _ => {
                    self.as_mut_list_state().handle_search_input(key);
                    self.update_search_query();
                }
            }
            return;
        } else {
            match event {
                UserEvent::Quit => {
                    self.tx.send(AppEvent::Quit);
                }
                UserEvent::ActionMenu => {
                    self.tx.send(AppEvent::OpenActionMenu);
                }
                UserEvent::CreateBranch => {
                    self.tx.send(AppEvent::OpenCreateBranchPrompt);
                }
                UserEvent::SwitchBranch => {
                    self.tx.send(AppEvent::OpenSwitchBranchPrompt);
                }
                UserEvent::OpenStatus => {
                    self.tx.send(AppEvent::OpenStatus);
                }
                UserEvent::PushCurrent => {
                    self.tx.send(AppEvent::PushCurrentBranch);
                }
                UserEvent::MergeBase => {
                    self.tx.send(AppEvent::MergeBaseIntoCurrent);
                }
                UserEvent::InstallHook => {
                    self.tx.send(AppEvent::InstallHook);
                }
                UserEvent::SetBase => {
                    self.tx.send(AppEvent::OpenSetBasePrompt);
                }
                UserEvent::SetBranchPrefix => {
                    self.tx.send(AppEvent::OpenSetBranchPrefixPrompt);
                }
                UserEvent::NavigateDown | UserEvent::SelectDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_next();
                    }
                }
                UserEvent::NavigateUp | UserEvent::SelectUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_prev();
                    }
                }
                UserEvent::GoToParent => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_parent();
                    }
                }
                UserEvent::GoToTop => {
                    self.as_mut_list_state().select_first();
                }
                UserEvent::GoToBottom => {
                    self.as_mut_list_state().select_last();
                }
                UserEvent::ScrollDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down();
                    }
                }
                UserEvent::ScrollUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up();
                    }
                }
                UserEvent::PageDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down_page();
                    }
                }
                UserEvent::PageUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up_page();
                    }
                }
                UserEvent::HalfPageDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down_half();
                    }
                }
                UserEvent::HalfPageUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up_half();
                    }
                }
                UserEvent::SelectTop => {
                    self.as_mut_list_state().select_high();
                }
                UserEvent::SelectMiddle => {
                    self.as_mut_list_state().select_middle();
                }
                UserEvent::SelectBottom => {
                    self.as_mut_list_state().select_low();
                }
                UserEvent::ShortCopy => {
                    self.copy_commit_short_hash();
                }
                UserEvent::FullCopy => {
                    self.copy_commit_hash();
                }
                UserEvent::Search => {
                    self.as_mut_list_state().start_search();
                    self.update_search_query();
                }
                UserEvent::UserCommand(n) => {
                    self.tx.send(AppEvent::OpenUserCommand(n));
                }
                UserEvent::HelpToggle => {
                    self.tx.send(AppEvent::OpenHelp);
                }
                UserEvent::Cancel => {
                    self.as_mut_list_state().cancel_search();
                    self.clear_search_query();
                }
                UserEvent::Confirm => {
                    self.tx.send(AppEvent::OpenDetail);
                }
                UserEvent::RefList => {
                    self.focus_pane = FocusPane::Sidebar;
                }
                UserEvent::Refresh => {
                    self.refresh();
                }
                _ => {}
            }
        }

        if let SearchState::Applied { .. } = self.as_list_state().search_state() {
            match event {
                UserEvent::GoToNext => {
                    self.as_mut_list_state().select_next_match();
                    self.update_matched_message();
                }
                UserEvent::GoToPrevious => {
                    self.as_mut_list_state().select_prev_match();
                    self.update_matched_message();
                }
                _ => {}
            }
            // Do not return here
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let graph_width = self.as_list_state().graph_area_cell_width() + 1;
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);
        let [refs_area, list_area] =
            Layout::horizontal([Constraint::Length(refs_width), Constraint::Min(0)]).areas(area);

        let branch_visuals = self.as_list_state().branch_visuals();
        let ref_list = RefList::new(
            &self.refs,
            branch_visuals,
            self.ctx.clone(),
            self.focus_pane == FocusPane::Sidebar,
        );
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);

        let commit_list =
            CommitList::new(self.ctx.clone(), self.focus_pane == FocusPane::CommitList);
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());
    }
}

impl<'a> ListView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    fn update_search_query(&self) {
        if let SearchState::Searching { .. } = self.as_list_state().search_state() {
            let list_state = self.as_list_state();
            if let Some(query) = list_state.search_query_string() {
                let cursor_pos = list_state.search_query_cursor_position();
                let transient_msg = list_state.transient_message_string();
                self.tx.send(AppEvent::UpdateStatusInput(
                    query,
                    Some(cursor_pos),
                    transient_msg,
                ));
            }
        }
    }

    fn clear_search_query(&self) {
        self.tx.send(AppEvent::ClearStatusLine);
    }

    fn update_matched_message(&self) {
        if let Some((msg, matched)) = self.as_list_state().matched_query_string() {
            if matched {
                self.tx.send(AppEvent::NotifyInfo(msg));
            } else {
                self.tx.send(AppEvent::NotifyWarn(msg));
            }
        } else {
            self.tx.send(AppEvent::ClearStatusLine);
        }
    }

    fn copy_commit_short_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA".into(), selected.as_str().into());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let (selected, opened) = self.ref_list_state.current_tree_status();
        let refs_context = RefsRefreshViewContext {
            selected,
            opened,
            focus_sidebar: self.focus_pane == FocusPane::Sidebar,
        };
        let context = RefreshViewContext::Refs {
            list_context,
            refs_context,
        };
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.reset_height(*height);
        if *scroll_to_top {
            list_state.select_first();
        } else {
            list_state.select_commit_hash(&CommitHash::from(commit_hash.as_str()));
            for _ in 0..*selected {
                list_state.scroll_up();
            }
        }
    }

    pub fn reset_refs_with(&mut self, refs_context: RefsRefreshViewContext) {
        self.ref_list_state.reset_tree_status(
            &self.refs,
            refs_context.selected,
            refs_context.opened,
        );
        self.focus_pane = if refs_context.focus_sidebar {
            FocusPane::Sidebar
        } else {
            FocusPane::CommitList
        };
    }

    fn handle_sidebar_event(&mut self, event: UserEvent, count: usize) {
        match event {
            UserEvent::Quit => self.tx.send(AppEvent::Quit),
            UserEvent::RefList | UserEvent::Cancel | UserEvent::Close => {
                self.focus_pane = FocusPane::CommitList;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.ref_list_state.select_next();
                }
                self.update_commit_list_selected();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.ref_list_state.select_prev();
                }
                self.update_commit_list_selected();
            }
            UserEvent::GoToTop => {
                self.ref_list_state.select_first();
                self.update_commit_list_selected();
            }
            UserEvent::GoToBottom => {
                self.ref_list_state.select_last();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateRight => {
                self.ref_list_state.open_node();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateLeft => {
                self.ref_list_state.close_node();
                self.update_commit_list_selected();
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => self.copy_ref_name(),
            UserEvent::OpenStatus => self.tx.send(AppEvent::OpenStatus),
            UserEvent::HelpToggle => self.tx.send(AppEvent::OpenHelp),
            UserEvent::Refresh => self.refresh(),
            _ => {}
        }
    }

    fn update_commit_list_selected(&mut self) {
        if let Some(selected) = self.ref_list_state.selected_ref_name() {
            self.as_mut_list_state().select_ref(&selected);
        }
    }

    fn copy_ref_name(&self) {
        if let Some(selected) = self.ref_list_state.selected_branch() {
            self.copy_to_clipboard("Branch Name".into(), selected);
        } else if let Some(selected) = self.ref_list_state.selected_tag() {
            self.copy_to_clipboard("Tag Name".into(), selected);
        }
    }
}
