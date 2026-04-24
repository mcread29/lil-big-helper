use std::{
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use ratatui::{
    style::{Color, Modifier, Stylize},
    text::Span,
};
use rustc_hash::FxHashMap;

use crate::{
    color::GraphColorSet,
    git::{Head, Ref, Repository},
    graph::Graph,
};

const REMOTE_ICON: &str = "☁";
const HEAD_ICON: &str = "◎";

#[derive(Debug)]
pub struct BranchVisuals {
    pub(crate) ref_colors: FxHashMap<String, Color>,
    pub(crate) fallback_colors: Vec<Color>,
}

impl BranchVisuals {
    pub fn new(repository: &Repository, graph: &Graph, graph_color_set: &GraphColorSet) -> Self {
        let fallback_colors = graph_color_set
            .colors
            .iter()
            .map(|color| color.to_ratatui_color())
            .collect::<Vec<_>>();
        let mut ref_colors = FxHashMap::default();

        for commit in &graph.commits {
            let (pos_x, _) = graph.commit_pos_map[&commit.commit_hash];
            let color = graph_color_set.get(pos_x).to_ratatui_color();
            for reference in repository.refs(&commit.commit_hash) {
                if matches!(reference, Ref::Branch { .. } | Ref::RemoteBranch { .. }) {
                    ref_colors.insert(reference.name().to_string(), color);
                }
            }
        }

        Self {
            ref_colors,
            fallback_colors,
        }
    }

    pub fn rc(self) -> Rc<Self> {
        Rc::new(self)
    }

    pub fn color_for_ref(&self, reference: &Ref) -> Color {
        self.color_for_name(reference.name())
    }

    pub fn color_for_name(&self, name: &str) -> Color {
        self.ref_colors
            .get(name)
            .copied()
            .unwrap_or_else(|| self.fallback_color(name))
    }

    fn fallback_color(&self, name: &str) -> Color {
        if self.fallback_colors.is_empty() {
            return Color::Reset;
        }

        let mut hasher = DefaultHasher::new();
        canonical_branch_name(name).hash(&mut hasher);
        let index = (hasher.finish() as usize) % self.fallback_colors.len();
        self.fallback_colors[index]
    }

    pub fn display_label(&self, reference: &Ref, shorten: bool) -> String {
        match reference {
            Ref::Branch { .. } => self.display_text(reference, shorten),
            Ref::RemoteBranch { .. } => {
                let label = self.display_text(reference, shorten);
                format!("{REMOTE_ICON} {label}")
            }
            Ref::Tag { name, .. } | Ref::Stash { name, .. } => name.clone(),
        }
    }

    pub fn display_text(&self, reference: &Ref, shorten: bool) -> String {
        match reference {
            Ref::Branch { name, .. } => {
                if shorten {
                    final_branch_segment(name).to_string()
                } else {
                    name.clone()
                }
            }
            Ref::RemoteBranch { name, .. } => {
                let branch_name = strip_remote_prefix(name);
                if shorten {
                    final_branch_segment(branch_name).to_string()
                } else {
                    branch_name.to_string()
                }
            }
            Ref::Tag { name, .. } | Ref::Stash { name, .. } => name.clone(),
        }
    }

    pub fn canonical_branch_name(&self, reference: &Ref) -> Option<String> {
        match reference {
            Ref::Branch { name, .. } => Some(name.clone()),
            Ref::RemoteBranch { name, .. } => Some(strip_remote_prefix(name).to_string()),
            Ref::Tag { .. } | Ref::Stash { .. } => None,
        }
    }

    pub fn head_marker<'a>(
        &self,
        branch_name: Option<&str>,
        shorten: bool,
        color: Color,
    ) -> Vec<Span<'a>> {
        let mut spans = vec![Span::raw(HEAD_ICON).fg(color).add_modifier(Modifier::BOLD)];
        if let Some(branch_name) = branch_name {
            spans.push(Span::raw(" ").fg(color).add_modifier(Modifier::BOLD));
            let label = if shorten {
                final_branch_segment(branch_name).to_string()
            } else {
                branch_name.to_string()
            };
            spans.push(Span::raw(label).fg(color).add_modifier(Modifier::BOLD));
        }
        spans
    }

    pub fn head_attached_to<'a>(&self, head: &'a Head, reference: &Ref) -> bool {
        matches!(
            (head, reference),
            (Head::Branch { name }, Ref::Branch { name: ref_name, .. }) if name == ref_name
        )
    }

    pub fn remote_icon_marker<'a>(&self, color: Color) -> Span<'a> {
        Span::raw(REMOTE_ICON).fg(color).bold()
    }

    #[cfg(test)]
    pub fn remote_spacing<'a>(&self, color: Color) -> Span<'a> {
        Span::raw(" ").fg(color).bold()
    }
}

fn canonical_branch_name(name: &str) -> &str {
    strip_remote_prefix(name)
}

fn strip_remote_prefix(name: &str) -> &str {
    name.split_once('/').map(|(_, rest)| rest).unwrap_or(name)
}

pub fn final_branch_segment(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        color::{GraphColor, GraphColorSet},
        git::{Commit, CommitHash, Head, Ref, Repository},
        graph::Graph,
    };
    use rustc_hash::FxHashMap;
    use std::path::PathBuf;

    fn graph_color_set() -> GraphColorSet {
        GraphColorSet {
            colors: vec![
                GraphColor::from_rgb(0x10, 0x20, 0x30),
                GraphColor::from_rgb(0x40, 0x50, 0x60),
            ],
            edge_color: GraphColor::from_rgb(0, 0, 0),
            background_color: GraphColor::from_rgb(0, 0, 0),
        }
    }

    #[test]
    fn final_segment_is_used_for_short_labels() {
        let visuals = BranchVisuals {
            ref_colors: FxHashMap::default(),
            fallback_colors: vec![Color::Green],
        };
        let branch = Ref::Branch {
            name: "mason/audio-feedback".into(),
            target: CommitHash::from("abc1234"),
        };
        let remote = Ref::RemoteBranch {
            name: "origin/mason/audio-feedback".into(),
            target: CommitHash::from("abc1234"),
        };

        assert_eq!(visuals.display_label(&branch, true), "audio-feedback");
        assert_eq!(visuals.display_text(&branch, true), "audio-feedback");
        assert_eq!(visuals.display_label(&remote, true), "☁ audio-feedback");
        assert_eq!(visuals.display_text(&remote, true), "audio-feedback");
    }

    #[test]
    fn remote_and_local_fallbacks_share_canonical_name() {
        let visuals = BranchVisuals {
            ref_colors: FxHashMap::default(),
            fallback_colors: graph_color_set()
                .colors
                .iter()
                .map(|color| color.to_ratatui_color())
                .collect(),
        };

        assert_eq!(
            visuals.color_for_name("dev"),
            visuals.color_for_name("origin/dev")
        );
    }

    #[test]
    fn head_marker_uses_icon() {
        let visuals = BranchVisuals {
            ref_colors: FxHashMap::default(),
            fallback_colors: vec![Color::Green],
        };
        let spans = visuals.head_marker(Some("mason/audio-feedback"), true, Color::Green);

        assert_eq!(spans[0].content.as_ref(), "◎");
        assert_eq!(spans[1].content.as_ref(), " ");
        assert_eq!(spans[2].content.as_ref(), "audio-feedback");
    }

    #[test]
    fn remote_icon_and_spacing_render_separately() {
        let visuals = BranchVisuals {
            ref_colors: FxHashMap::default(),
            fallback_colors: vec![Color::Blue],
        };

        assert_eq!(
            visuals.remote_icon_marker(Color::Blue).content.as_ref(),
            "☁"
        );
        assert_eq!(visuals.remote_spacing(Color::Blue).content.as_ref(), " ");
    }

    #[test]
    fn visible_branches_use_graph_lane_colors() {
        let local_target = CommitHash::from("abc1234");
        let remote_target = CommitHash::from("def5678");
        let local_commit = Commit {
            commit_hash: local_target.clone(),
            ..Commit::default()
        };
        let remote_commit = Commit {
            commit_hash: remote_target.clone(),
            ..Commit::default()
        };
        let repository = Repository::new(
            PathBuf::from("."),
            FxHashMap::from_iter([
                (local_target.clone(), local_commit),
                (remote_target.clone(), remote_commit),
            ]),
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::from_iter([
                (
                    local_target.clone(),
                    vec![Ref::Branch {
                        name: "dev".into(),
                        target: local_target.clone(),
                    }],
                ),
                (
                    remote_target.clone(),
                    vec![Ref::RemoteBranch {
                        name: "origin/dev".into(),
                        target: remote_target.clone(),
                    }],
                ),
            ]),
            Head::None,
            vec![local_target.clone(), remote_target.clone()],
        );
        let local_commit = repository.commit(&local_target).unwrap();
        let remote_commit = repository.commit(&remote_target).unwrap();
        let graph = Graph {
            commits: vec![local_commit, remote_commit],
            commit_pos_map: FxHashMap::from_iter([
                (&local_commit.commit_hash, (0, 0)),
                (&remote_commit.commit_hash, (1, 1)),
            ]),
            edges: Vec::new(),
            max_pos_x: 1,
        };

        let visuals = BranchVisuals::new(&repository, &graph, &graph_color_set());

        assert_eq!(visuals.color_for_name("dev"), Color::Rgb(0x10, 0x20, 0x30));
        assert_eq!(
            visuals.color_for_name("origin/dev"),
            Color::Rgb(0x40, 0x50, 0x60)
        );
    }
}
