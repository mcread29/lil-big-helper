use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Commit, FileChange, Ref, Repository},
    view::{ListRefreshViewContext, RefreshViewContext, RefsRefreshViewContext},
    widget::{
        commit_detail::{CommitDetail, CommitDetailState},
        commit_list::{CommitList, CommitListState},
        ref_list::{RefList, RefListState},
    },
};

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_detail_state: CommitDetailState,

    commit: Commit,
    changes: Vec<FileChange>,
    commit_refs: Vec<Ref>,
    all_refs: Vec<Ref>,
    ref_list_state: RefListState,
    refs_context: Option<RefsRefreshViewContext>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DetailView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        commit_refs: Vec<Ref>,
        all_refs: Vec<Ref>,
        refs_context: Option<RefsRefreshViewContext>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        let mut ref_list_state = RefListState::new();
        if let Some(refs_context) = refs_context.as_ref() {
            ref_list_state.reset_tree_status(
                &all_refs,
                refs_context.selected.clone(),
                refs_context.opened.clone(),
            );
        } else if let crate::git::Head::Branch { name } = commit_list_state.head() {
            ref_list_state.select_branch_name(&all_refs, name);
        }

        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state: CommitDetailState::default(),
            commit,
            changes,
            commit_refs,
            all_refs,
            ref_list_state,
            refs_context,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::NavigateDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_down();
                }
            }
            UserEvent::NavigateUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.commit_detail_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.commit_detail_state.select_last();
            }
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::GoToParent => {
                self.tx.send(AppEvent::SelectParentCommit);
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_hash();
            }
            UserEvent::UserCommand(n) => {
                self.tx.send(AppEvent::OpenUserCommand(n));
            }
            UserEvent::OpenStatus => {
                self.tx.send(AppEvent::OpenStatus);
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let detail_height = (area.height - 1).min(self.ctx.ui_config.detail.height);
        let [top_area, detail_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(detail_height)]).areas(area);

        let graph_width = self.as_list_state().graph_area_cell_width() + 1;
        let refs_width =
            (top_area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);
        let [refs_area, list_area] =
            Layout::horizontal([Constraint::Length(refs_width), Constraint::Min(0)])
                .areas(top_area);

        let branch_visuals = self.as_list_state().branch_visuals();
        let ref_list = RefList::new(
            &self.all_refs,
            branch_visuals.clone(),
            self.ctx.clone(),
            false,
        );
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);

        let commit_list = CommitList::new(self.ctx.clone(), true);
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let head = self.as_list_state().head().clone();
        let graph_color = self.as_list_state().selected_commit_graph_color();
        let commit_detail = CommitDetail::new(
            &self.commit,
            &self.changes,
            &self.commit_refs,
            &head,
            graph_color,
            branch_visuals,
            self.ctx.clone(),
        );
        f.render_stateful_widget(commit_detail, detail_area, &mut self.commit_detail_state);
    }
}

impl<'a> DetailView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn refs_context(&self) -> Option<RefsRefreshViewContext> {
        self.refs_context.clone()
    }

    pub fn set_refs_context(&mut self, refs_context: Option<RefsRefreshViewContext>) {
        if let Some(refs_context) = refs_context.as_ref() {
            self.ref_list_state.reset_tree_status(
                &self.all_refs,
                refs_context.selected.clone(),
                refs_context.opened.clone(),
            );
        }
        self.refs_context = refs_context;
    }

    pub fn select_older_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_next());
    }

    pub fn select_newer_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_prev());
    }

    pub fn select_parent_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_parent());
    }

    fn update_selected_commit<F>(&mut self, repository: &Repository, update_commit_list_state: F)
    where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let commit_list_state = self.as_mut_list_state();
        update_commit_list_state(commit_list_state);
        let selected = commit_list_state.selected_commit_hash().clone();
        let (commit, changes) = repository.commit_detail(&selected);
        let refs = repository.refs(&selected).into_iter().cloned().collect();
        self.commit = commit;
        self.changes = changes;
        self.commit_refs = refs;

        self.commit_detail_state.select_first();
    }

    fn copy_commit_short_hash(&self) {
        let selected = &self.commit.commit_hash;
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = &self.commit.commit_hash;
        self.copy_to_clipboard("Commit SHA".into(), selected.as_str().into());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::Detail {
            list_context,
            refs_context: self.refs_context.clone(),
        };
        self.tx.send(AppEvent::Refresh(context));
    }
}
