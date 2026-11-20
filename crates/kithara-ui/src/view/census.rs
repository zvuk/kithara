use std::collections::{BTreeMap, BTreeSet};

use crate::{
    error::UiDocError,
    expand::{ControlSite, scoped_key},
    ids::SourceUri,
    interact::recognizers::Edge,
    module::{BindingRef, ControlNode, ViewSet},
    validate::{Gesture, column_writes, write_slots},
    view::ViewState,
};

/// What one press writes into the state it names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, kithara_derive::Mirror)]
#[mirror(from_ref = Write)]
#[non_exhaustive]
pub enum ViewWrite<'a> {
    Flag(#[mirror(copy)] ViewSet),
    Page(#[mirror(as_ref)] &'a str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Write {
    Flag(ViewSet),
    Page(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Target {
    View(String, Write),
    Endpoint(Declared),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Declared {
    pub(super) key: String,
    pub(super) edge: Option<(Edge, String)>,
    pub(super) close: Option<String>,
}

/// Where one page-turning state stood when a screen was compiled.
///
/// A screen shows the page its state stands at, and the document's own initial
/// page while it stands at none. Both are kept: a host looking for a screen it
/// already compiled has only the state to go by, and a state standing nowhere
/// asks for the same screen as one standing at the initial page.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PageStanding {
    /// Every page the screen offers, so what a harness may turn to comes from
    /// the document rather than from a list beside it that can disagree.
    pub offered: BTreeSet<String>,
    pub initial: String,
    /// The page that picked this screen. A selection picks none: it turns on
    /// the screen it stands on.
    pub shown: Option<String>,
}

/// Where each press writes, by the path of the control that publishes it.
///
/// A host draining a press looks it up here before the application is told, so
/// a document turning its own state needs no application code to do it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewWrites {
    by_path: BTreeMap<String, BTreeMap<Gesture, Target>>,
    pages: BTreeMap<String, PageStanding>,
    named: BTreeSet<String>,
}

impl ViewWrites {
    /// Whether any control on this screen declares a write to `key`.
    pub(crate) fn declares(&self, key: &str) -> bool {
        self.by_path
            .values()
            .flat_map(BTreeMap::values)
            .any(|target| matches!(target, Target::Endpoint(declared) if declared.key == key))
    }

    pub(super) fn target(&self, path: &str, gesture: Gesture) -> Option<&Target> {
        self.by_path.get(path)?.get(&gesture)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Every state this screen names, on either side.
    #[must_use]
    pub const fn named(&self) -> &BTreeSet<String> {
        &self.named
    }

    /// Where every page-turning state stood when this screen was compiled.
    #[must_use]
    pub const fn pages(&self) -> &BTreeMap<String, PageStanding> {
        &self.pages
    }

    /// The page one state stands at on this screen: the page the view was
    /// turned to, or the one the document calls initial while it has been
    /// turned nowhere.
    #[must_use]
    pub fn standing<'a>(&'a self, view: &'a ViewState, state: &str) -> Option<&'a str> {
        let at = self.pages.get(state)?;
        Some(view.page(state).unwrap_or(&at.initial))
    }
}

/// One `Tabs` as it compiled: the pages it offers and the one it showed.
pub(crate) struct Tabs<'a> {
    pub(crate) origin: &'a SourceUri,
    pub(crate) initial: &'a str,
    pub(crate) path: &'a str,
    pub(crate) shown: Option<&'a str>,
    pub(crate) state: &'a str,
    pub(crate) pages: BTreeSet<String>,
}

impl<'a> Tabs<'a> {
    /// The pages a `select` slot reading a page state declares: its fill keys,
    /// the first standing until the state is written.
    fn selected(site: ControlSite<'a>, origin: &'a SourceUri) -> Option<Self> {
        let (Some(first), Some(BindingRef::Page { id, name: None })) =
            (site.fills.first(), site.read)
        else {
            return None;
        };
        Some(Self {
            origin,
            initial: &first.key,
            path: site.path,
            shown: None,
            state: &id.0,
            pages: site.fills.iter().map(|fill| fill.key.clone()).collect(),
        })
    }
}

/// One naming of a page, kept until the pages a `Tabs` declares are known.
struct Named {
    origin: SourceUri,
    page: String,
    path: String,
    state: String,
}

/// What one document says about the states it names, gathered while it expands.
///
/// A state the document writes and never reads is a name nothing shows, which
/// is a typo rather than a screen: a misspelt name on either side leaves the
/// one it was meant to be unwritten and the one it became unread. A state only
/// read is left alone, because an application is allowed to be the only thing
/// that moves it.
#[derive(Default)]
pub(crate) struct Census {
    declared: BTreeMap<String, BTreeSet<String>>,
    origin: BTreeMap<String, (SourceUri, String)>,
    pages: BTreeMap<String, PageStanding>,
    writes: BTreeMap<String, BTreeMap<Gesture, Target>>,
    read: BTreeSet<String>,
    named: Vec<Named>,
}

impl Census {
    pub(crate) fn finish(self) -> Result<ViewWrites, UiDocError> {
        if let Some(named) = self.named.iter().find(|named| {
            !self
                .declared
                .get(&named.state)
                .is_some_and(|pages| pages.contains(&named.page))
        }) {
            return Err(UiDocError::UnknownPage {
                origin: named.origin.clone(),
                id: named.state.clone(),
                page: named.page.clone(),
                path: named.path.clone(),
            });
        }
        if let Some((state, (origin, path))) = self
            .origin
            .iter()
            .find(|(state, _)| !self.read.contains(*state))
        {
            return Err(UiDocError::UnreadState {
                origin: origin.clone(),
                id: state.clone(),
                path: path.clone(),
            });
        }
        let named = self
            .read
            .iter()
            .cloned()
            .chain(self.writes.values().flat_map(BTreeMap::values).filter_map(
                |target| match target {
                    Target::View(state, _) => Some(state.clone()),
                    Target::Endpoint(_) => None,
                },
            ))
            .collect();
        Ok(ViewWrites {
            named,
            by_path: self.writes,
            pages: self.pages,
        })
    }

    /// Notes one binding a control reads.
    pub(crate) fn note_read(&mut self, path: &str, binding: &BindingRef, origin: &SourceUri) {
        if let BindingRef::Page { id, name: None } = binding {
            self.read.insert(id.0.clone());
        } else if let Some((state, _)) = self.view_target(path, binding, origin) {
            self.read.insert(state);
        }
    }

    /// The state a view or page binding names and what a write does to it.
    fn view_target(
        &mut self,
        path: &str,
        binding: &BindingRef,
        origin: &SourceUri,
    ) -> Option<(String, Write)> {
        match binding {
            BindingRef::View { id, set, .. } => Some((id.0.clone(), Write::Flag(*set))),
            BindingRef::Page {
                id,
                name: Some(name),
            } => {
                self.named.push(Named {
                    origin: origin.clone(),
                    page: name.clone(),
                    path: path.to_owned(),
                    state: id.0.clone(),
                });
                Some((id.0.clone(), Write::Page(name.clone())))
            }
            BindingRef::Page { name: None, .. }
            | BindingRef::Command { .. }
            | BindingRef::Model { .. }
            | BindingRef::Parameter { .. }
            | BindingRef::Telemetry { .. } => None,
        }
    }

    pub(crate) fn note_write(&mut self, path: String, binding: &BindingRef, at: WriteAt) {
        let WriteAt {
            gesture,
            origin,
            edge,
            close,
        } = at;
        let target = match binding {
            BindingRef::Command { id, with }
            | BindingRef::Model { id, with }
            | BindingRef::Parameter { id, with } => Target::Endpoint(Declared {
                edge,
                close,
                key: scoped_key(&id.0, with),
            }),
            BindingRef::Telemetry { .. } => return,
            BindingRef::View { .. } | BindingRef::Page { .. } => {
                let Some((state, write)) = self.view_target(&path, binding, origin) else {
                    return;
                };
                self.origin
                    .entry(state.clone())
                    .or_insert_with(|| (origin.clone(), path.clone()));
                Target::View(state, write)
            }
        };
        self.writes.entry(path).or_default().insert(gesture, target);
    }

    /// Notes the pages one `Tabs` offers, which of them it showed, and that it
    /// reads the state naming which of them stands.
    pub(crate) fn note_pages(&mut self, tabs: Tabs<'_>) {
        let pages = tabs.pages;
        self.read.insert(tabs.state.to_owned());
        self.declared
            .entry(tabs.state.to_owned())
            .or_default()
            .extend(pages.iter().cloned());
        self.origin
            .entry(tabs.state.to_owned())
            .or_insert_with(|| (tabs.origin.clone(), tabs.path.to_owned()));
        self.pages.insert(
            tabs.state.to_owned(),
            PageStanding {
                initial: tabs.initial.to_owned(),
                offered: pages,
                shown: tabs.shown.map(str::to_owned),
            },
        );
    }

    /// Notes every binding one control site carries.
    ///
    /// A popover dismisses itself on its own path, so the state it reads for whether it stands open
    /// is the same state that dismissal shuts.
    pub(crate) fn note_site(&mut self, site: ControlSite<'_>, origin: &SourceUri) {
        if let Some(selection) = Tabs::selected(site, origin) {
            self.note_pages(selection);
        }
        for binding in [
            site.read,
            site.active,
            site.color_read,
            site.columns_state,
            site.status,
            site.query,
            site.scope,
            site.zoom,
        ]
        .into_iter()
        .flatten()
        {
            self.note_read(site.path, binding, origin);
        }
        let interval = site.read.and_then(endpoint_key);
        let close = match site.shuts {
            Some(BindingRef::View { id, .. }) => Some(id.0.clone()),
            _ => None,
        };
        for slot in write_slots(site) {
            let path = slot.child.map_or_else(
                || site.path.to_owned(),
                |child| format!("{}/{child}", site.path),
            );
            let at = WriteAt {
                origin,
                gesture: slot.gesture,
                edge: slot.edge.zip(interval.clone()),
                close: close.clone(),
            };
            self.note_write(path, slot.binding, at);
        }
        for (child, binding, gesture, _) in column_writes(site) {
            let at = WriteAt {
                origin,
                gesture,
                edge: None,
                close: close.clone(),
            };
            self.note_write(format!("{}/{child}", site.path), &binding, at);
        }
        if let (ControlNode::Popover { .. }, Some(BindingRef::View { id, .. })) =
            (site.control, site.read)
        {
            self.writes.entry(site.path.to_owned()).or_default().insert(
                Gesture::Press,
                Target::View(id.0.clone(), Write::Flag(ViewSet::Off)),
            );
        }
    }
}

pub(crate) struct WriteAt<'a> {
    pub(crate) gesture: Gesture,
    pub(crate) origin: &'a SourceUri,
    pub(crate) edge: Option<(Edge, String)>,
    pub(crate) close: Option<String>,
}

impl<'a> WriteAt<'a> {
    pub(crate) const fn plain(gesture: Gesture, origin: &'a SourceUri) -> Self {
        Self {
            gesture,
            origin,
            edge: None,
            close: None,
        }
    }
}

fn endpoint_key(binding: &BindingRef) -> Option<String> {
    match binding {
        BindingRef::Command { id, with }
        | BindingRef::Model { id, with }
        | BindingRef::Parameter { id, with }
        | BindingRef::Telemetry { id, with } => Some(scoped_key(&id.0, with)),
        BindingRef::View { .. } | BindingRef::Page { .. } => None,
    }
}
