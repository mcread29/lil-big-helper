use std::{
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use laurier::highlight::highlight_matched_text;
use once_cell::sync::Lazy;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyEvent},
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, StatefulWidget, Widget},
};
use rustc_hash::{FxHashMap, FxHashSet};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::UserListColumnType,
    git::{Commit, CommitHash, Head, Ref},
    graph::GraphImageManager,
    widget::branch_visual::BranchVisuals,
};

static FUZZY_MATCHER: Lazy<SkimMatcherV2> = Lazy::new(|| SkimMatcherV2::default().respect_case());

const ELLIPSIS: &str = "...";
const STATUS_COLUMN_WIDTH: u16 = 15;
const FOCUSED_SELECTION_BG_FACTOR: f32 = 0.32;
const UNFOCUSED_SELECTION_BG_FACTOR: f32 = 0.12;

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    graph_color: Color,
}

impl<'a> CommitInfo<'a> {
    pub fn new(commit: &'a Commit, refs: Vec<&'a Ref>, graph_color: Color) -> Self {
        Self {
            commit,
            refs,
            graph_color,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Inactive,
    Searching {
        start_index: usize,
        match_index: usize,
        ignore_case: bool,
        fuzzy: bool,
        transient_message: TransientMessage,
    },
    Applied {
        match_index: usize,
        total_match: usize,
    },
}

impl SearchState {
    fn update_match_index(&mut self, index: usize) {
        match self {
            SearchState::Searching { match_index, .. } => *match_index = index,
            SearchState::Applied { match_index, .. } => *match_index = index,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientMessage {
    None,
    IgnoreCaseOff,
    IgnoreCaseOn,
    FuzzyOff,
    FuzzyOn,
}

#[derive(Debug, Default, Clone)]
struct SearchMatch {
    refs: FxHashMap<String, SearchMatchPosition>,
    subject: Option<SearchMatchPosition>,
    author_name: Option<SearchMatchPosition>,
    commit_hash: Option<SearchMatchPosition>,
    match_index: usize, // 1-based
}

impl SearchMatch {
    fn set(&mut self, c: &Commit, refs: &[&Ref], matcher: &SearchMatcher) {
        self.refs = refs
            .iter()
            .filter(|r| !matches!(*r, Ref::Stash { .. }))
            .filter_map(|r| {
                matcher
                    .matched_position(r.name())
                    .map(|pos| (r.name().into(), pos))
            })
            .collect();
        self.subject = matcher.matched_position(&c.subject);
        self.author_name = matcher.matched_position(&c.author_name);
        self.commit_hash = matcher.matched_position(c.commit_hash.as_short_hash());
        self.match_index = 0;
    }

    fn matched(&self) -> bool {
        !self.refs.is_empty()
            || self.subject.is_some()
            || self.author_name.is_some()
            || self.commit_hash.is_some()
    }

    fn clear(&mut self) {
        self.refs.clear();
        self.subject = None;
        self.author_name = None;
        self.commit_hash = None;
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatchPosition {
    matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
    }
}

struct SearchMatcher {
    query: String,
    ignore_case: bool,
    fuzzy: bool,
}

impl SearchMatcher {
    fn new(query: &str, ignore_case: bool, fuzzy: bool) -> Self {
        let query = if ignore_case {
            query.to_lowercase()
        } else {
            query.into()
        };
        Self {
            query,
            ignore_case,
            fuzzy,
        }
    }

    fn matched_position(&self, s: &str) -> Option<SearchMatchPosition> {
        if self.fuzzy {
            let result = if self.ignore_case {
                FUZZY_MATCHER.fuzzy_indices(&s.to_lowercase(), &self.query)
            } else {
                FUZZY_MATCHER.fuzzy_indices(s, &self.query)
            };
            result
                .map(|(_, indices)| indices)
                .map(SearchMatchPosition::new)
        } else {
            let result = if self.ignore_case {
                s.to_lowercase().find(&self.query)
            } else {
                s.find(&self.query)
            };
            result
                .map(|p| (p..(p + self.query.len())).collect())
                .map(SearchMatchPosition::new)
        }
    }
}

#[derive(Debug)]
pub struct CommitListState<'a> {
    commits: Vec<CommitInfo<'a>>,
    commit_hash_set: FxHashSet<&'a CommitHash>,
    graph_image_manager: GraphImageManager<'a>,
    graph_cell_width: u16,
    head: &'a Head,
    branch_visuals: Rc<BranchVisuals>,

    ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,

    search_state: SearchState,
    search_input: Input,
    search_matches: Vec<SearchMatch>,

    selected: usize,
    offset: usize,
    total: usize,
    height: usize,

    default_ignore_case: bool,
    default_fuzzy: bool,
}

impl<'a> CommitListState<'a> {
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph_image_manager: GraphImageManager<'a>,
        graph_cell_width: u16,
        head: &'a Head,
        branch_visuals: Rc<BranchVisuals>,
        ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,
        default_ignore_case: bool,
        default_fuzzy: bool,
    ) -> CommitListState<'a> {
        let total = commits.len();
        let commit_hash_set = commits.iter().map(|c| &c.commit.commit_hash).collect();
        CommitListState {
            commits,
            commit_hash_set,
            graph_image_manager,
            graph_cell_width,
            head,
            branch_visuals,
            ref_name_to_commit_index_map,
            search_state: SearchState::Inactive,
            search_input: Input::default(),
            search_matches: vec![SearchMatch::default(); total],
            selected: 0,
            offset: 0,
            total,
            height: 0,
            default_ignore_case,
            default_fuzzy,
        }
    }

    pub fn graph_area_cell_width(&self) -> u16 {
        self.graph_cell_width + 1 // right pad
    }

    pub fn branch_visuals(&self) -> Rc<BranchVisuals> {
        self.branch_visuals.clone()
    }

    pub fn head(&self) -> &Head {
        self.head
    }

    pub fn select_next(&mut self) {
        if self.selected < (self.total - 1).min(self.height - 1) {
            self.selected += 1;
        } else if self.selected + self.offset < self.total - 1 {
            self.offset += 1;
        }
    }

    pub fn select_parent(&mut self) {
        if let Some(target_commit) = self.selected_commit_parent_hash().cloned() {
            if self.commit_hash_set.contains(&target_commit) {
                while target_commit.as_str() != self.selected_commit_hash().as_str() {
                    self.select_next();
                }
            }
        }
    }

    pub fn selected_commit_parent_hash(&self) -> Option<&CommitHash> {
        self.commits[self.current_selected_index()]
            .commit
            .parent_commit_hashes
            .first()
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        } else if self.offset > 0 {
            self.offset -= 1;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = (self.height - 1).min(self.total - 1);
        if self.height < self.total {
            self.offset = self.total - self.height;
        }
    }

    pub fn scroll_down(&mut self) {
        if self.offset + self.height < self.total {
            self.offset += 1;
            if self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.offset > 0 {
            self.offset -= 1;
            if self.selected < self.height - 1 {
                self.selected += 1;
            }
        }
    }

    pub fn scroll_down_page(&mut self) {
        self.scroll_down_height(self.height);
    }

    pub fn scroll_up_page(&mut self) {
        self.scroll_up_height(self.height);
    }

    pub fn scroll_down_half(&mut self) {
        self.scroll_down_height(self.height / 2);
    }

    pub fn scroll_up_half(&mut self) {
        self.scroll_up_height(self.height / 2);
    }

    fn scroll_down_height(&mut self, scroll_height: usize) {
        if self.offset + self.height + scroll_height < self.total {
            self.offset += scroll_height;
        } else {
            let old_offset = self.offset;
            let size = self.height.min(self.total);
            self.offset = self.total - size;
            self.selected += scroll_height - (self.offset - old_offset);
            if self.selected >= size {
                self.selected = size - 1;
            }
        }
    }

    fn scroll_up_height(&mut self, scroll_height: usize) {
        if self.offset > scroll_height {
            self.offset -= scroll_height;
        } else {
            let old_offset = self.offset;
            self.offset = 0;
            self.selected = self
                .selected
                .saturating_sub(scroll_height - (old_offset - self.offset));
        }
    }

    pub fn select_high(&mut self) {
        self.selected = 0;
    }

    pub fn select_middle(&mut self) {
        if self.total > self.height {
            self.selected = self.height / 2;
        } else {
            self.selected = self.total / 2;
        }
    }

    pub fn select_low(&mut self) {
        if self.total > self.height {
            self.selected = self.height - 1;
        } else {
            self.selected = self.total - 1;
        }
    }

    fn select_index(&mut self, index: usize) {
        if index < self.total {
            if self.total > self.height {
                self.selected = 0;
                self.offset = index;
            } else {
                self.selected = index;
            }
        }
    }

    pub fn select_next_match(&mut self) {
        self.select_next_match_index(self.current_selected_index());
    }

    pub fn select_prev_match(&mut self) {
        self.select_prev_match_index(self.current_selected_index());
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        &self.commits[self.current_selected_index()]
            .commit
            .commit_hash
    }

    pub fn selected_commit_graph_color(&self) -> Color {
        self.commits[self.current_selected_index()].graph_color
    }

    fn current_selected_index(&self) -> usize {
        self.offset + self.selected
    }

    pub fn current_list_status(&self) -> (usize, usize, usize) {
        (self.selected, self.offset, self.height)
    }

    pub fn reset_height(&mut self, height: usize) {
        self.height = height;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        if let Some(&index) = self.ref_name_to_commit_index_map.get(ref_name) {
            if self.total > self.height {
                self.selected = 0;
                self.offset = index;
            } else {
                self.selected = index;
            }
        }
    }

    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) {
        if !self.commit_hash_set.contains(commit_hash) {
            return;
        }
        for (i, commit_info) in self.commits.iter().enumerate() {
            if commit_info.commit.commit_hash == *commit_hash {
                if self.total > self.height {
                    self.selected = 0;
                    self.offset = i;
                } else {
                    self.selected = i;
                }
                break;
            }
        }
    }

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_index(),
                match_index: 0,
                ignore_case: self.default_ignore_case,
                fuzzy: self.default_fuzzy,
                transient_message: TransientMessage::None,
            };
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        if let SearchState::Searching {
            transient_message, ..
        } = &mut self.search_state
        {
            *transient_message = TransientMessage::None;
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.search_input.handle_event(&Event::Key(key));
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn apply_search(&mut self) {
        if let SearchState::Searching { match_index, .. } = self.search_state {
            if self.search_input.value().is_empty() {
                self.search_state = SearchState::Inactive;
            } else {
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index,
                    total_match,
                };
            }
        }
    }

    pub fn cancel_search(&mut self) {
        if let SearchState::Searching { .. } | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Inactive;
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn toggle_ignore_case(&mut self) {
        if let SearchState::Searching {
            ignore_case,
            transient_message,
            ..
        } = &mut self.search_state
        {
            *ignore_case = !*ignore_case;
            *transient_message = if *ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            };
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn toggle_fuzzy(&mut self) {
        if let SearchState::Searching {
            fuzzy,
            transient_message,
            ..
        } = &mut self.search_state
        {
            *fuzzy = !*fuzzy;
            *transient_message = if *fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            };
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn search_query_string(&self) -> Option<String> {
        if let SearchState::Searching { .. } = self.search_state {
            let query = self.search_input.value();
            Some(format!("/{query}"))
        } else {
            None
        }
    }

    pub fn matched_query_string(&self) -> Option<(String, bool)> {
        if let SearchState::Applied {
            match_index,
            total_match,
            ..
        } = self.search_state
        {
            let query = self.search_input.value();
            if total_match == 0 {
                let msg = format!("No matches found (query: \"{query}\")");
                Some((msg, false))
            } else {
                let msg = format!("Match {match_index} of {total_match} (query: \"{query}\")");
                Some((msg, true))
            }
        } else {
            None
        }
    }

    pub fn search_query_cursor_position(&self) -> u16 {
        self.search_input.visual_cursor() as u16 + 1 // add 1 for "/"
    }

    pub fn transient_message_string(&self) -> Option<String> {
        if let SearchState::Searching {
            transient_message, ..
        } = self.search_state
        {
            match transient_message {
                TransientMessage::None => None,
                TransientMessage::IgnoreCaseOn => Some("Ignore case: ON ".to_string()),
                TransientMessage::IgnoreCaseOff => Some("Ignore case: OFF".to_string()),
                TransientMessage::FuzzyOn => Some("Fuzzy match: ON ".to_string()),
                TransientMessage::FuzzyOff => Some("Fuzzy match: OFF".to_string()),
            }
        } else {
            None
        }
    }

    fn update_search_matches(&mut self, ignore_case: bool, fuzzy: bool) {
        let matcher = SearchMatcher::new(self.search_input.value(), ignore_case, fuzzy);
        let mut match_index = 1;
        for (i, commit_info) in self.commits.iter().enumerate() {
            let m = &mut self.search_matches[i];
            m.set(commit_info.commit, commit_info.refs.as_slice(), &matcher);
            if m.matched() {
                m.match_index = match_index;
                match_index += 1;
            }
        }
    }

    fn clear_search_matches(&mut self) {
        self.search_matches.iter_mut().for_each(|m| m.clear());
    }

    fn select_current_or_next_match_index(&mut self, current_index: usize) {
        if self.search_matches[current_index].matched() {
            self.select_index(current_index);
            self.search_state
                .update_match_index(self.search_matches[current_index].match_index);
        } else {
            self.select_next_match_index(current_index)
        }
    }

    fn select_next_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == self.total - 1 {
                i = 0;
            } else {
                i += 1;
            }
        }
    }

    fn select_prev_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + self.total - 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == 0 {
                i = self.total - 1;
            } else {
                i -= 1;
            }
        }
    }

    fn encoded_image(&self, commit_info: &'a CommitInfo) -> &str {
        self.graph_image_manager
            .encoded_image(&commit_info.commit.commit_hash)
    }
}

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    focused: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>, focused: bool) -> Self {
        Self {
            ctx,
            focused,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitList<'a> {
    type State = CommitListState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        self.update_state(area, state);

        let columns = visible_columns(&self.ctx.ui_config.list.columns);
        let constraints = calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_area_cell_width(),
            self.ctx.ui_config.list.name_width,
            self.ctx.ui_config.list.date_width,
            &columns,
        );
        let chunks = Layout::horizontal(constraints).split(area);

        for (i, col) in columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, chunks[i], state);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, chunks[i], state);
                }
                UserListColumnType::Status => {
                    self.render_status(buf, chunks[i], state);
                }
                UserListColumnType::Subject => {
                    self.render_subject(buf, chunks[i], state);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, chunks[i], state);
                }
                UserListColumnType::Hash => {
                    continue;
                }
                UserListColumnType::Date => {
                    continue;
                }
            }
        }
    }
}

impl CommitList<'_> {
    fn update_state(&self, area: Rect, state: &mut CommitListState) {
        state.height = area.height as usize;

        if state.total > state.height && state.total - state.height < state.offset {
            let diff = state.offset - (state.total - state.height);
            state.selected += diff;
            state.offset -= diff;
        }
        if state.selected >= state.height {
            let diff = state.selected - state.height + 1;
            state.selected -= diff;
            state.offset += diff;
        }

        state
            .commits
            .iter()
            .skip(state.offset)
            .take(state.height)
            .for_each(|commit_info| {
                state
                    .graph_image_manager
                    .load_encoded_image(&commit_info.commit.commit_hash);
            });
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(i, commit_info)| {
                buf[(area.left(), area.top() + i as u16)]
                    .set_symbol(state.encoded_image(commit_info));

                // width - 1 for right pad
                for w in 1..area.width - 1 {
                    buf[(area.left() + w, area.top() + i as u16)].set_skip(true);
                }
            });
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(_, commit_info)| ListItem::new("│".fg(commit_info.graph_color)))
            .collect();
        Widget::render(List::new(items), area, buf)
    }

    fn render_subject(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let commit = commit_info.commit;
                let spans = if max_width > ELLIPSIS.len() {
                    let truncate = console::measure_text_width(&commit.subject) > max_width;
                    let subject = if truncate {
                        console::truncate_str(&commit.subject, max_width, ELLIPSIS).to_string()
                    } else {
                        commit.subject.to_string()
                    };

                    if let Some(pos) = state.search_matches[state.offset + i].subject.clone() {
                        highlighted_spans(
                            subject.into(),
                            pos,
                            commit_info.graph_color,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            truncate,
                        )
                    } else {
                        vec![subject.fg(commit_info.graph_color)]
                    }
                } else {
                    Vec::new()
                };
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_status(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let spans = status_spans(
                    commit_info,
                    state.head,
                    &state.branch_visuals,
                    &state.search_matches[state.offset + i].refs,
                    &self.ctx.color_theme,
                );
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_name(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_iter(state)
            .map(|(i, commit)| {
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let color = author_color(commit);
                let name_width = console::measure_text_width(&name);
                let pad_width = max_width.saturating_sub(name_width);
                let mut spans = Vec::new();
                if pad_width > 0 {
                    spans.push(Span::raw(" ".repeat(pad_width)));
                }
                let name_spans =
                    if let Some(pos) = state.search_matches[state.offset + i].author_name.clone() {
                        highlighted_spans(
                            name.into(),
                            pos,
                            color,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            truncate,
                        )
                    } else {
                        vec![name.fg(color)]
                    };
                spans.extend(name_spans);
                self.to_commit_list_item_with_alignment(i, spans, state, Alignment::Right)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn rendering_commit_info_iter<'a>(
        &'a self,
        state: &'a CommitListState,
    ) -> impl Iterator<Item = (usize, &'a CommitInfo<'a>)> {
        state
            .commits
            .iter()
            .skip(state.offset)
            .take(state.height)
            .enumerate()
    }

    fn rendering_commit_iter<'a>(
        &'a self,
        state: &'a CommitListState,
    ) -> impl Iterator<Item = (usize, &'a Commit)> {
        self.rendering_commit_info_iter(state)
            .map(|(i, commit_info)| (i, commit_info.commit))
    }

    fn to_commit_list_item<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState,
    ) -> ListItem<'a> {
        self.to_commit_list_item_with_alignment(i, spans, state, Alignment::Left)
    }

    fn to_commit_list_item_with_alignment<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState,
        alignment: Alignment,
    ) -> ListItem<'a> {
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans).alignment(alignment);
        if i == state.selected {
            let bg = if self.focused {
                dim_selection_color(
                    self.ctx.color_theme.list_selected_bg,
                    FOCUSED_SELECTION_BG_FACTOR,
                )
            } else {
                dim_selection_color(
                    self.ctx.color_theme.list_selected_bg,
                    UNFOCUSED_SELECTION_BG_FACTOR,
                )
            };
            line = line.bg(bg).fg(self.ctx.color_theme.list_selected_fg);
        }
        ListItem::new(line)
    }
}

fn dim_selection_color(color: Color, factor: f32) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::DarkGray => (128, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::White => (255, 255, 255),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        other => return other,
    };
    Color::Rgb((r as f32 * factor) as u8, (g as f32 * factor) as u8, (b as f32 * factor) as u8)
}

fn status_spans<'a>(
    commit_info: &'a CommitInfo,
    head: &'a Head,
    branch_visuals: &'a BranchVisuals,
    refs_matches: &'a FxHashMap<String, SearchMatchPosition>,
    color_theme: &'a ColorTheme,
) -> Vec<Span<'a>> {
    let refs = &commit_info.refs;

    if refs.len() == 1 {
        if let Ref::Stash { name, .. } = refs[0] {
            return vec![Span::raw(name).fg(commit_info.graph_color).bold()];
        }
    }

    let local_branch_names = refs
        .iter()
        .filter_map(|reference| match reference {
            Ref::Branch { .. } => branch_visuals.canonical_branch_name(reference),
            _ => None,
        })
        .collect::<FxHashSet<_>>();
    let mut ref_spans: Vec<(Vec<Span>, &String)> = Vec::new();
    for reference in refs.iter().filter(|reference| !matches!(reference, Ref::Stash { .. })) {
        let name = match reference {
            Ref::Branch { name, .. } | Ref::RemoteBranch { name, .. } | Ref::Tag { name, .. } => {
                name
            }
            Ref::Stash { .. } => continue,
        };

        let fg = match reference {
            Ref::Branch { .. } | Ref::RemoteBranch { .. } => branch_visuals.color_for_ref(reference),
            Ref::Tag { .. } => color_theme.list_ref_tag_fg,
            Ref::Stash { .. } => continue,
        };

        if matches!(reference, Ref::RemoteBranch { .. }) {
            let Some(canonical_name) = branch_visuals.canonical_branch_name(reference) else {
                continue;
            };
            if local_branch_names.contains(&canonical_name) {
                continue;
            }
        }

        let spans = match reference {
            Ref::Branch { .. } | Ref::RemoteBranch { .. } => display_branch_ref_spans(
                branch_visuals,
                reference,
                name,
                refs_matches.get(name),
                fg,
                color_theme,
            ),
            Ref::Tag { .. } => refs_matches
                .get(name)
                .and_then(|pos| display_match_position(name, name, pos))
                .map(|pos| {
                    highlighted_spans(
                        Span::raw(name.clone()),
                        pos,
                        fg,
                        Modifier::BOLD,
                        color_theme,
                        false,
                    )
                })
                .unwrap_or_else(|| vec![Span::raw(name.clone()).fg(fg).bold()]),
            Ref::Stash { .. } => continue,
        };
        ref_spans.push((spans, name));
    }

    let mut spans = Vec::new();

    if let Head::Detached { target } = head {
        if commit_info.commit.commit_hash == *target {
            spans.extend(branch_visuals.head_marker(None, false, commit_info.graph_color));
            if !ref_spans.is_empty() {
                spans.push(Span::raw(", ").fg(commit_info.graph_color).bold());
            }
        }
    }

    let total_ref_spans = ref_spans.len();
    for (i, ss) in ref_spans.into_iter().enumerate() {
        let (ref_spans, ref_name) = ss;
        if let Head::Branch { name } = head {
            if ref_name == name {
                spans.extend(branch_visuals.head_marker(Some(name), true, commit_info.graph_color));
                if i + 1 < total_ref_spans {
                    spans.push(Span::raw(", ").fg(commit_info.graph_color).bold());
                }
                continue;
            }
        }
        spans.extend(ref_spans);
        if i + 1 < total_ref_spans {
            spans.push(Span::raw(", ").fg(commit_info.graph_color).bold());
        }
    }

    spans
}

fn display_match_position(
    full_name: &str,
    display_label: &str,
    pos: &SearchMatchPosition,
) -> Option<SearchMatchPosition> {
    let offset = full_name.len().saturating_sub(display_label.len());
    let matched_indices = pos
        .matched_indices
        .iter()
        .copied()
        .filter(|index| *index >= offset)
        .map(|index| index - offset)
        .collect::<Vec<_>>();

    if matched_indices.is_empty() {
        None
    } else {
        Some(SearchMatchPosition::new(matched_indices))
    }
}

fn display_branch_ref_spans(
    branch_visuals: &BranchVisuals,
    reference: &Ref,
    full_name: &str,
    pos: Option<&SearchMatchPosition>,
    fg: Color,
    color_theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let (prefix, visible_name) = match reference {
        Ref::RemoteBranch { .. } => ("☁ ", branch_visuals.display_text(reference, true)),
        Ref::Branch { .. } => ("", branch_visuals.display_text(reference, true)),
        _ => ("", full_name.to_string()),
    };

    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::raw(prefix).fg(fg).bold());
    }

    let visible_spans = pos
        .and_then(|pos| display_match_position(full_name, &visible_name, pos))
        .map(|pos| {
            highlighted_spans(
                Span::raw(visible_name.to_string()),
                pos,
                fg,
                Modifier::BOLD,
                color_theme,
                false,
            )
        })
        .unwrap_or_else(|| vec![Span::raw(visible_name.to_string()).fg(fg).bold()]);
    spans.extend(visible_spans);
    spans
}

fn author_color(commit: &Commit) -> Color {
    const AUTHOR_COLORS: [Color; 8] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::LightCyan,
        Color::LightGreen,
        Color::LightYellow,
    ];

    let identity = if commit.author_email.is_empty() {
        &commit.author_name
    } else {
        &commit.author_email
    };
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    AUTHOR_COLORS[(hasher.finish() as usize) % AUTHOR_COLORS.len()]
}

fn highlighted_spans(
    s: Span<'_>,
    pos: SearchMatchPosition,
    base_fg: Color,
    base_modifier: Modifier,
    color_theme: &ColorTheme,
    truncate: bool,
) -> Vec<Span<'static>> {
    let mut hm = highlight_matched_text(vec![s])
        .matched_indices(pos.matched_indices)
        .not_matched_style(Style::default().fg(base_fg).add_modifier(base_modifier))
        .matched_style(
            Style::default()
                .fg(color_theme.list_match_fg)
                .bg(color_theme.list_match_bg)
                .add_modifier(base_modifier),
        );
    if truncate {
        hm = hm.ellipsis(ELLIPSIS);
    }
    hm.into_spans()
}

fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_width: u16,
    name_width: u16,
    _date_width: u16,
    columns: &[UserListColumnType],
) -> Vec<Constraint> {
    let pad = 2;
    let (
        mut graph_cell_width,
        mut marker_cell_width,
        mut status_cell_width,
        mut name_cell_width,
        mut hash_cell_width,
        mut date_cell_width,
    ) = (0, 0, 0, 0, 0, 0);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_cell_width = graph_width;
            }
            UserListColumnType::Marker => {
                marker_cell_width = 1;
            }
            UserListColumnType::Status => {
                status_cell_width = STATUS_COLUMN_WIDTH;
            }
            UserListColumnType::Name => {
                name_cell_width = name_width + pad;
            }
            UserListColumnType::Hash => {
                hash_cell_width = 0;
            }
            UserListColumnType::Date => {
                date_cell_width = 0;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = graph_cell_width
        + marker_cell_width
        + status_cell_width
        + hash_cell_width
        + name_cell_width
        + date_cell_width
        + subject_min_width;

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_cell_width);
        name_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_cell_width);
        date_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(hash_cell_width);
        hash_cell_width = 0;
    }
    if total_width > area_width {
        status_cell_width = 0;
    }

    let mut constraints = Vec::new();
    for col in columns {
        match col {
            UserListColumnType::Graph => {
                constraints.push(Constraint::Length(graph_cell_width));
            }
            UserListColumnType::Marker => {
                constraints.push(Constraint::Length(marker_cell_width));
            }
            UserListColumnType::Status => {
                constraints.push(Constraint::Length(status_cell_width));
            }
            UserListColumnType::Subject => {
                constraints.push(Constraint::Min(0));
            }
            UserListColumnType::Name => {
                constraints.push(Constraint::Length(name_cell_width));
            }
            UserListColumnType::Hash => {
                constraints.push(Constraint::Length(hash_cell_width));
            }
            UserListColumnType::Date => {
                constraints.push(Constraint::Length(date_cell_width));
            }
        }
    }
    constraints
}

fn visible_columns(columns: &[UserListColumnType]) -> Vec<UserListColumnType> {
    columns
        .iter()
        .filter(|column| !matches!(column, UserListColumnType::Hash | UserListColumnType::Date))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_match_position_handles_shortened_remote_labels() {
        let pos = SearchMatchPosition::new(vec![19, 20, 21]);
        let display = display_match_position("origin/mason/audio-feedback", "feedback", &pos)
            .expect("suffix match should be preserved");

        assert_eq!(display.matched_indices, vec![0, 1, 2]);
    }

    #[test]
    fn remote_branch_spans_render_single_cloud_icon() {
        let reference = Ref::RemoteBranch {
            name: "origin/mason/audio-feedback".into(),
            target: CommitHash::from("abc1234"),
        };
        let theme = ColorTheme::default();
        let branch_visuals = BranchVisuals {
            ref_colors: FxHashMap::default(),
            fallback_colors: vec![Color::Blue],
        };

        let spans = display_branch_ref_spans(
            &branch_visuals,
            &reference,
            "origin/mason/audio-feedback",
            None,
            Color::Blue,
            &theme,
        );

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "☁ ");
        assert_eq!(spans[1].content.as_ref(), "audio-feedback");
    }

    #[test]
    fn status_spans_dedupes_matching_local_and_remote_refs() {
        let commit = Commit {
            commit_hash: CommitHash::from("abc1234"),
            ..Commit::default()
        };
        let branch = Ref::Branch {
            name: "mason/audio-feedback".into(),
            target: commit.commit_hash.clone(),
        };
        let remote = Ref::RemoteBranch {
            name: "origin/mason/audio-feedback".into(),
            target: commit.commit_hash.clone(),
        };
        let commit_info = CommitInfo::new(&commit, vec![&branch, &remote], Color::Green);
        let branch_visuals = BranchVisuals {
            ref_colors: FxHashMap::from_iter([
                ("mason/audio-feedback".to_string(), Color::Green),
                ("origin/mason/audio-feedback".to_string(), Color::Green),
            ]),
            fallback_colors: vec![Color::Green],
        };
        let head = Head::Branch {
            name: "mason/audio-feedback".into(),
        };
        let refs_matches = FxHashMap::default();
        let color_theme = ColorTheme::default();

        let spans = status_spans(
            &commit_info,
            &head,
            &branch_visuals,
            &refs_matches,
            &color_theme,
        );

        let contents = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(contents, vec!["◎", " ", "audio-feedback"]);
    }

    #[test]
    fn visible_columns_filters_hash_and_date() {
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Hash,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
        ];

        assert_eq!(
            visible_columns(&columns),
            vec![
                UserListColumnType::Graph,
                UserListColumnType::Subject,
                UserListColumnType::Name,
            ]
        );
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(15),
            Constraint::Min(0),
            Constraint::Length(12),
            Constraint::Length(0),
            Constraint::Length(0),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date_hash() {
        let area_width = 30;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(0),
            Constraint::Min(0),
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Length(0),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date() {
        let area_width = 40;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(0),
            Constraint::Min(0),
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Length(0),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name() {
        let area_width = 60;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(15),
            Constraint::Min(0),
            Constraint::Length(12),
            Constraint::Length(0),
            Constraint::Length(0),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_keeps_status_when_space_allows() {
        let area_width = 70;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(15),
            Constraint::Min(0),
            Constraint::Length(0),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_columns_order() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Date,
            UserListColumnType::Status,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
            UserListColumnType::Graph,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(0),
            Constraint::Length(15),
            Constraint::Min(0),
            Constraint::Length(0),
            Constraint::Length(6),
        ];
        assert_eq!(actual, expected);
    }
}
