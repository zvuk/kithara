use std::collections::BTreeMap;

use kithara::ui::{
    error::UiDocError,
    render::{TreeRow, WriteValue},
    text::TextDoc,
};
use kithara_app_library::{BranchNode, LibrarySource, PageStatus, Registration, worded};

/// The library shell: sources, selection, expanded nodes and page states.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
pub(in crate::gui) struct Library {
    sources: Vec<Mounted>,
    statuses: StatusWords,
    expanded: Vec<NodeAt>,
    selected: Option<NodeAt>,
}

struct Mounted {
    source: Box<dyn LibrarySource>,
    row: Option<String>,
    widths: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NodeAt {
    source: usize,
    key: String,
}

impl NodeAt {
    fn is(&self, source: usize, key: &str) -> bool {
        self.source == source && self.key == key
    }
}

/// The catalog's words for every page state, taken when the shell mounts.
struct StatusWords {
    empty: String,
    loading: String,
    unreadable: String,
}

impl StatusWords {
    fn new(text: &TextDoc) -> Result<Self, UiDocError> {
        Ok(Self {
            empty: worded(text, "library.status.empty", "source.status")?,
            loading: worded(text, "library.status.loading", "source.status")?,
            unreadable: worded(text, "library.status.error", "source.status")?,
        })
    }

    fn of(&self, status: PageStatus) -> &str {
        match status {
            PageStatus::Ready => "",
            PageStatus::Loading => &self.loading,
            PageStatus::Empty => &self.empty,
            PageStatus::Unreadable => &self.unreadable,
        }
    }
}

struct Shown<'a> {
    source: usize,
    node: &'a BranchNode,
    row: TreeRow<'a>,
}

impl Library {
    pub(in crate::gui) fn new(
        registered: Vec<Registration>,
        text: &TextDoc,
    ) -> Result<Self, UiDocError> {
        let sources = registered
            .into_iter()
            .map(|source| {
                source.build(text).map(|source| Mounted {
                    source,
                    row: None,
                    widths: BTreeMap::new(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut library = Self {
            sources,
            statuses: StatusWords::new(text)?,
            expanded: Vec::new(),
            selected: None,
        };
        let mut path: Vec<String> = Vec::new();
        let mut node = library.sources.first().map(|source| source.source.branch());
        while let Some(branch) = node {
            path.push(branch.key.clone());
            node = branch.children.first();
        }
        let roots: Vec<NodeAt> = library
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| {
                source.source.branch().unlisted || !source.source.branch().children.is_empty()
            })
            .map(|(source, mounted)| NodeAt {
                source,
                key: mounted.source.branch().key.clone(),
            })
            .collect();
        for root in roots {
            library.open(root);
        }
        if let Some(leaf) = path.pop() {
            for key in path.into_iter().skip(1) {
                library.open(NodeAt { source: 0, key });
            }
            library.select_at(NodeAt {
                source: 0,
                key: leaf,
            });
        }
        Ok(library)
    }

    pub(in crate::gui) fn index_of(&self, id: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|mounted| mounted.source.id() == id)
    }

    delegate::delegate! {
        to self.sources {
            #[expr(Some($?.source.as_ref()))]
            #[call(get)]
            pub(in crate::gui) fn source(&self, index: usize) -> Option<&dyn LibrarySource>;
            #[expr($?.row.as_deref())]
            #[call(get)]
            pub(in crate::gui) fn selected_row(&self, at: usize) -> Option<&str>;
            #[expr($.map(|mounted| mounted.source.as_ref()))]
            #[call(iter)]
            pub(in crate::gui) fn sources(&self) -> impl Iterator<Item = &dyn LibrarySource>;
        }
    }

    /// Hands a write the source's page declares to that source.
    pub(in crate::gui) fn write(&mut self, id: &str, endpoint: &str, value: &WriteValue) {
        if let Some(at) = self.index_of(id) {
            self.sources[at].source.write(endpoint, value);
        }
    }

    pub(in crate::gui) fn page(&self) -> Option<&str> {
        let selected = self.selected.as_ref()?;
        Some(self.sources.get(selected.source)?.source.id())
    }

    pub(in crate::gui) fn column_width(&self, source: usize, column: &str) -> Option<f64> {
        self.sources.get(source)?.widths.get(column).copied()
    }

    pub(in crate::gui) fn set_column_width(&mut self, source: &str, column: &str, width: f64) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        if let Some(at) = self.index_of(source) {
            self.sources[at].widths.insert(column.to_owned(), width);
        }
    }

    pub(in crate::gui) fn select_row(&mut self, id: &str, row: usize) {
        let Some(at) = self.index_of(id) else {
            return;
        };
        let key = self.sources[at].source.row_key(row).map(str::to_owned);
        self.sources[at].row = key;
    }

    pub(in crate::gui) fn select(&mut self, row: usize) {
        if let Some(at) = self.at(row) {
            self.select_at(at);
        }
    }

    pub(in crate::gui) fn tick(&mut self) {
        for source in &mut self.sources {
            source.source.tick();
        }
    }

    pub(in crate::gui) fn toggle(&mut self, row: usize) {
        let Some(at) = self.at(row) else {
            return;
        };
        if let Some(index) = self.expanded.iter().position(|open| *open == at) {
            self.expanded.swap_remove(index);
            return;
        }
        self.open(at);
    }

    pub(in crate::gui) fn tree(&self) -> Vec<TreeRow<'_>> {
        self.shown().into_iter().map(|shown| shown.row).collect()
    }

    pub(in crate::gui) fn status_words(&self, status: PageStatus) -> &str {
        self.statuses.of(status)
    }

    fn at(&self, row: usize) -> Option<NodeAt> {
        self.shown().into_iter().nth(row).map(|shown| NodeAt {
            source: shown.source,
            key: shown.node.key.clone(),
        })
    }

    fn push<'a>(
        &'a self,
        out: &mut Vec<Shown<'a>>,
        source: usize,
        node: &'a BranchNode,
        depth: u8,
    ) {
        let open = self.expanded.iter().any(|at| at.is(source, &node.key));
        out.push(Shown {
            source,
            node,
            row: TreeRow {
                label: &node.label,
                depth,
                icon: node.icon,
                count: node.count,
                expanded: (node.unlisted || !node.children.is_empty()).then_some(open),
                page: node.page,
                muted: false,
                selected: self
                    .selected
                    .as_ref()
                    .is_some_and(|at| at.is(source, &node.key)),
            },
        });
        if open {
            for child in &node.children {
                self.push(out, source, child, depth.saturating_add(1));
            }
        }
    }

    fn open(&mut self, at: NodeAt) {
        if let Some(source) = self.sources.get_mut(at.source) {
            source.source.expand(&at.key);
        }
        self.expanded.push(at);
    }

    fn select_at(&mut self, at: NodeAt) {
        if let Some(source) = self.sources.get_mut(at.source) {
            source.source.select(&at.key);
        }
        if self.selected.as_ref() != Some(&at)
            && let Some(mounted) = self.sources.get_mut(at.source)
        {
            mounted.row = None;
        }
        self.selected = Some(at);
    }

    fn shown(&self) -> Vec<Shown<'_>> {
        let mut out = Vec::new();
        for (at, source) in self.sources.iter().enumerate() {
            self.push(&mut out, at, source.source.branch(), 0);
        }
        out
    }
}
