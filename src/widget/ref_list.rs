use std::rc::Rc;
use std::{collections::HashSet, iter};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    widgets::{Block, Borders, Padding, StatefulWidget},
};
use semver::Version;
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::{app::AppContext, color::ColorTheme, git::Ref, widget::branch_visual::BranchVisuals};

const TREE_BRANCH_ROOT_IDENT: &str = "__branches__";
const TREE_REMOTE_ROOT_IDENT: &str = "__remotes__";
const TREE_TAG_ROOT_IDENT: &str = "__tags__";
const TREE_STASH_ROOT_IDENT: &str = "__stashes__";

const TREE_BRANCH_ROOT_TEXT: &str = "Branches";
const TREE_REMOTE_ROOT_TEXT: &str = "Remotes";
const TREE_TAG_ROOT_TEXT: &str = "Tags";
const TREE_STASH_ROOT_TEXT: &str = "Stashes";

#[derive(Debug, Default)]
pub struct RefListState {
    tree_state: TreeState<String>,
}

impl RefListState {
    pub fn new() -> Self {
        let mut tree_state = TreeState::default();
        tree_state.select(vec![TREE_BRANCH_ROOT_IDENT.into()]);
        tree_state.open(vec![TREE_BRANCH_ROOT_IDENT.into()]);
        Self { tree_state }
    }

    pub fn select_next(&mut self) {
        self.tree_state.key_down();
    }

    pub fn select_prev(&mut self) {
        self.tree_state.key_up();
    }

    pub fn select_first(&mut self) {
        self.tree_state.select_first();
    }

    pub fn select_last(&mut self) {
        self.tree_state.select_last();
    }

    pub fn open_node(&mut self) {
        self.tree_state.key_right();
    }

    pub fn close_node(&mut self) {
        self.tree_state.key_left();
    }

    pub fn selected_ref_name(&self) -> Option<String> {
        self.tree_state.selected().last().cloned()
    }

    pub fn selected_branch(&self) -> Option<String> {
        let selected = self.tree_state.selected();
        if selected.len() > 1
            && (selected[0] == TREE_BRANCH_ROOT_IDENT || selected[0] == TREE_REMOTE_ROOT_IDENT)
        {
            selected.last().cloned()
        } else {
            None
        }
    }

    pub fn selected_tag(&self) -> Option<String> {
        let selected = self.tree_state.selected();
        if selected.len() > 1 && selected[0] == TREE_TAG_ROOT_IDENT {
            selected.last().cloned()
        } else {
            None
        }
    }

    pub fn current_tree_status(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let selected = self.tree_state.selected().into();
        let opened = self.tree_state.opened().iter().cloned().collect();
        (selected, opened)
    }

    pub fn reset_tree_status(
        &mut self,
        refs: &[Ref],
        selected: Vec<String>,
        opened: Vec<Vec<String>>,
    ) {
        let valid_paths = collect_valid_tree_paths(refs);
        let selected = sanitize_selected_path(selected, &valid_paths);

        self.tree_state.close_all();
        for node in opened.into_iter().filter(|node| valid_paths.contains(node)) {
            self.tree_state.open(node);
        }
        self.tree_state.select(selected);
    }
}

pub struct RefList {
    items: Vec<TreeItem<'static, String>>,
    ctx: Rc<AppContext>,
    focused: bool,
}

impl RefList {
    pub fn new(
        refs: &[Ref],
        branch_visuals: Rc<BranchVisuals>,
        ctx: Rc<AppContext>,
        focused: bool,
    ) -> RefList {
        let items = build_ref_tree_items(refs, &branch_visuals, &ctx.color_theme);
        RefList {
            items,
            ctx,
            focused,
        }
    }
}

impl StatefulWidget for RefList {
    type State = RefListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let mut highlight_style = Style::default()
            .bg(self.ctx.color_theme.ref_selected_bg)
            .fg(self.ctx.color_theme.ref_selected_fg);
        if !self.focused {
            highlight_style = highlight_style.add_modifier(Modifier::DIM);
        }
        let tree = Tree::new(&self.items)
            .unwrap()
            .node_closed_symbol("\u{25b8} ")
            .node_open_symbol("\u{25be} ")
            .node_no_children_symbol("  ")
            .highlight_style(highlight_style)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::horizontal(1)),
            );
        tree.render(area, buf, &mut state.tree_state);
    }
}

fn collect_valid_tree_paths(refs: &[Ref]) -> HashSet<Vec<String>> {
    let mut valid_paths = HashSet::from([
        vec![TREE_BRANCH_ROOT_IDENT.into()],
        vec![TREE_REMOTE_ROOT_IDENT.into()],
        vec![TREE_TAG_ROOT_IDENT.into()],
        vec![TREE_STASH_ROOT_IDENT.into()],
    ]);

    for r in refs {
        match r {
            Ref::Branch { name, .. } => {
                add_ref_path(&mut valid_paths, TREE_BRANCH_ROOT_IDENT, name)
            }
            Ref::RemoteBranch { name, .. } => {
                add_ref_path(&mut valid_paths, TREE_REMOTE_ROOT_IDENT, name)
            }
            Ref::Tag { name, .. } => {
                valid_paths.insert(vec![TREE_TAG_ROOT_IDENT.into(), name.clone()]);
            }
            Ref::Stash { name, .. } => {
                valid_paths.insert(vec![TREE_STASH_ROOT_IDENT.into(), name.clone()]);
            }
        }
    }

    valid_paths
}

fn add_ref_path(valid_paths: &mut HashSet<Vec<String>>, root: &str, name: &str) {
    let mut path = vec![root.to_string()];
    let mut identifier = String::new();
    for part in name.split('/') {
        if identifier.is_empty() {
            identifier = part.to_string();
        } else {
            identifier = format!("{identifier}/{part}");
        }
        path.push(identifier.clone());
        valid_paths.insert(path.clone());
    }
}

fn sanitize_selected_path(
    selected: Vec<String>,
    valid_paths: &HashSet<Vec<String>>,
) -> Vec<String> {
    for idx in (1..=selected.len()).rev() {
        let candidate = selected[..idx].to_vec();
        if valid_paths.contains(&candidate) {
            return candidate;
        }
    }

    iter::once(TREE_BRANCH_ROOT_IDENT.to_string()).collect()
}

fn build_ref_tree_items(
    refs: &[Ref],
    branch_visuals: &BranchVisuals,
    color_theme: &ColorTheme,
) -> Vec<TreeItem<'static, String>> {
    let mut branch_refs = Vec::new();
    let mut remote_refs = Vec::new();
    let mut tag_refs = Vec::new();
    let mut stash_refs = Vec::new();

    for r in refs {
        match r {
            Ref::Tag { name, .. } => tag_refs.push(name.into()),
            Ref::Branch { name, .. } => branch_refs.push(name.into()),
            Ref::RemoteBranch { name, .. } => remote_refs.push(name.into()),
            Ref::Stash { name, message, .. } => stash_refs.push((name.into(), message.into())),
        }
    }

    let mut branch_nodes = refs_to_ref_tree_nodes(branch_refs);
    let mut remote_nodes = refs_to_ref_tree_nodes(remote_refs);
    let mut tag_nodes = refs_to_ref_tree_nodes(tag_refs);
    let mut stash_nodes = refs_to_stash_ref_tree_nodes(stash_refs);

    sort_branch_tree_nodes(&mut branch_nodes);
    sort_branch_tree_nodes(&mut remote_nodes);
    sort_tag_tree_nodes(&mut tag_nodes);
    sort_stash_tree_nodes(&mut stash_nodes);

    let branch_items = ref_tree_nodes_to_tree_items(
        branch_nodes,
        branch_visuals,
        color_theme,
        TreeNodeKind::Branch,
    );
    let remote_items = ref_tree_nodes_to_tree_items(
        remote_nodes,
        branch_visuals,
        color_theme,
        TreeNodeKind::RemoteBranch,
    );
    let tag_items =
        ref_tree_nodes_to_tree_items(tag_nodes, branch_visuals, color_theme, TreeNodeKind::Other);
    let stash_items = ref_tree_nodes_to_tree_items(
        stash_nodes,
        branch_visuals,
        color_theme,
        TreeNodeKind::Other,
    );

    vec![
        tree_item(
            TREE_BRANCH_ROOT_IDENT.into(),
            TREE_BRANCH_ROOT_TEXT.into(),
            branch_items,
            branch_visuals,
            color_theme,
            TreeNodeKind::Other,
        ),
        tree_item(
            TREE_REMOTE_ROOT_IDENT.into(),
            TREE_REMOTE_ROOT_TEXT.into(),
            remote_items,
            branch_visuals,
            color_theme,
            TreeNodeKind::Other,
        ),
        tree_item(
            TREE_TAG_ROOT_IDENT.into(),
            TREE_TAG_ROOT_TEXT.into(),
            tag_items,
            branch_visuals,
            color_theme,
            TreeNodeKind::Other,
        ),
        tree_item(
            TREE_STASH_ROOT_IDENT.into(),
            TREE_STASH_ROOT_TEXT.into(),
            stash_items,
            branch_visuals,
            color_theme,
            TreeNodeKind::Other,
        ),
    ]
}

struct RefTreeNode {
    identifier: String,
    name: String,
    children: Vec<RefTreeNode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeNodeKind {
    Branch,
    RemoteBranch,
    Other,
}

fn refs_to_stash_ref_tree_nodes(ref_name_messages: Vec<(String, String)>) -> Vec<RefTreeNode> {
    ref_name_messages
        .into_iter()
        .map(|(name, message)| RefTreeNode {
            identifier: name,
            name: message,
            children: Vec::new(),
        })
        .collect()
}

fn refs_to_ref_tree_nodes(ref_names: Vec<String>) -> Vec<RefTreeNode> {
    let mut nodes: Vec<RefTreeNode> = Vec::new();

    for ref_name in ref_names {
        let mut parts = ref_name.split('/').collect::<Vec<_>>();
        let mut current_nodes = &mut nodes;
        let mut parent_identifier = String::new();

        while !parts.is_empty() {
            let part = parts.remove(0);
            if let Some(index) = current_nodes.iter().position(|n| n.name == part) {
                let node = &mut current_nodes[index];
                current_nodes = &mut node.children;
                parent_identifier.clone_from(&node.identifier);
            } else {
                let identifier = if parent_identifier.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent_identifier}/{part}")
                };
                current_nodes.push(RefTreeNode {
                    identifier: identifier.clone(),
                    name: part.to_string(),
                    children: Vec::new(),
                });
                current_nodes = current_nodes.last_mut().unwrap().children.as_mut();
                parent_identifier = identifier;
            }
        }
    }

    nodes
}

fn ref_tree_nodes_to_tree_items(
    nodes: Vec<RefTreeNode>,
    branch_visuals: &BranchVisuals,
    color_theme: &ColorTheme,
    kind: TreeNodeKind,
) -> Vec<TreeItem<'static, String>> {
    let mut items = Vec::new();
    for node in nodes {
        if node.children.is_empty() {
            items.push(tree_leaf_item(
                node.identifier,
                node.name,
                branch_visuals,
                color_theme,
                kind,
            ));
        } else {
            let children =
                ref_tree_nodes_to_tree_items(node.children, branch_visuals, color_theme, kind);
            items.push(tree_item(
                node.identifier,
                node.name,
                children,
                branch_visuals,
                color_theme,
                kind,
            ));
        }
    }
    items
}

fn sort_branch_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        b.children
            .len()
            .cmp(&a.children.len())
            .then(a.name.cmp(&b.name))
    });
    for node in nodes {
        sort_branch_tree_nodes(&mut node.children);
    }
}

fn sort_tag_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        let a_version = parse_semantic_version_tag(&a.name);
        let b_version = parse_semantic_version_tag(&b.name);
        if a_version.is_none() && b_version.is_none() {
            a.name.cmp(&b.name)
        } else {
            b_version.cmp(&a_version)
        }
    });
}

fn sort_stash_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| a.identifier.cmp(&b.identifier));
}

fn parse_semantic_version_tag(tag: &str) -> Option<Version> {
    let tag = tag.trim_start_matches('v');
    Version::parse(tag).ok()
}

fn tree_item(
    identifier: String,
    name: String,
    children: Vec<TreeItem<'static, String>>,
    branch_visuals: &BranchVisuals,
    color_theme: &ColorTheme,
    kind: TreeNodeKind,
) -> TreeItem<'static, String> {
    let color = tree_item_color(&identifier, branch_visuals, color_theme, kind);
    TreeItem::new(identifier, name.fg(color), children).unwrap()
}

fn tree_leaf_item(
    identifier: String,
    name: String,
    branch_visuals: &BranchVisuals,
    color_theme: &ColorTheme,
    kind: TreeNodeKind,
) -> TreeItem<'static, String> {
    tree_item(
        identifier,
        name,
        Vec::new(),
        branch_visuals,
        color_theme,
        kind,
    )
}

fn tree_item_color(
    identifier: &str,
    branch_visuals: &BranchVisuals,
    color_theme: &ColorTheme,
    kind: TreeNodeKind,
) -> Color {
    match kind {
        TreeNodeKind::Branch => branch_visuals.color_for_name(identifier),
        TreeNodeKind::RemoteBranch if identifier.contains('/') => {
            branch_visuals.color_for_name(identifier)
        }
        TreeNodeKind::RemoteBranch => color_theme.fg,
        TreeNodeKind::Other => color_theme.fg,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::DateTime;
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::{
        color::{GraphColor, GraphColorSet},
        git::{Commit, CommitHash, Head, Repository},
        graph::Graph,
    };

    fn commit(hash: &str) -> Commit {
        Commit {
            commit_hash: CommitHash::from(hash),
            author_date: DateTime::parse_from_rfc3339("2026-04-23T00:00:00+00:00").unwrap(),
            committer_date: DateTime::parse_from_rfc3339("2026-04-23T00:00:00+00:00").unwrap(),
            ..Commit::default()
        }
    }

    fn branch_visuals() -> BranchVisuals {
        let commit = commit("0123456789abcdef0123456789abcdef01234567");
        let commit_hash = commit.commit_hash.clone();
        let repository = Repository::new(
            PathBuf::new(),
            FxHashMap::from_iter([(commit_hash.clone(), commit)]),
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::from_iter([(
                commit_hash.clone(),
                vec![
                    Ref::Branch {
                        name: "dev".into(),
                        target: commit_hash.clone(),
                    },
                    Ref::RemoteBranch {
                        name: "origin/dev".into(),
                        target: commit_hash.clone(),
                    },
                ],
            )]),
            Head::None,
            vec![commit_hash.clone()],
        );
        let commit = repository.commit(&commit_hash).unwrap();
        let graph = Graph {
            commits: vec![commit],
            commit_pos_map: FxHashMap::from_iter([(&commit.commit_hash, (0, 0))]),
            edges: vec![vec![]],
            max_pos_x: 0,
        };
        let graph_color_set = GraphColorSet {
            colors: vec![GraphColor::from_rgb(0x11, 0x22, 0x33)],
            edge_color: GraphColor::from_rgb(0, 0, 0),
            background_color: GraphColor::from_rgb(0, 0, 0),
        };

        BranchVisuals::new(&repository, &graph, &graph_color_set)
    }

    #[test]
    fn branch_sidebar_items_use_graph_colors() {
        let visuals = branch_visuals();
        let theme = ColorTheme {
            fg: Color::White,
            ..ColorTheme::default()
        };

        assert_eq!(
            tree_item_color("dev", &visuals, &theme, TreeNodeKind::Branch),
            Color::Rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(
            tree_item_color("origin/dev", &visuals, &theme, TreeNodeKind::RemoteBranch),
            Color::Rgb(0x11, 0x22, 0x33)
        );
    }

    #[test]
    fn non_branch_sidebar_items_keep_theme_color() {
        let visuals = branch_visuals();
        let theme = ColorTheme {
            fg: Color::White,
            ..ColorTheme::default()
        };

        assert_eq!(
            tree_item_color("v1.0.0", &visuals, &theme, TreeNodeKind::Other),
            Color::White
        );
    }
}
