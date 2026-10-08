use std::collections::HashSet;

use kithara_app_library::{
    BranchNode, Cause, Context, Environment, Factory, LibrarySource, PAGES, PageStatus,
    Registration, worded,
};
use kithara_net::{HttpClient, Net};
use kithara_platform::{
    CancelToken,
    time::{Duration, Instant},
    tokio::{
        runtime::Handle,
        sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    },
};
use kithara_ui::{
    error::UiDocError,
    module::IconName,
    render::{ReadValue, TableRow, WriteValue},
    text::TextDoc,
};

use crate::{
    Client, Config, Playlist, TrackId,
    ui::{catalogue::Catalogue, consts, page, request::Completed},
};

/// Lists the collection's playlists under the branch's Playlists node.
pub(super) fn accept_playlists(branch: &mut BranchNode, playlists: Vec<Playlist>) {
    let Some(node) = branch
        .children
        .iter_mut()
        .find(|node| node.key == consts::PLAYLISTS)
    else {
        return;
    };
    node.unlisted = false;
    node.count = u32::try_from(playlists.len()).ok();
    node.children = playlists
        .into_iter()
        .map(|playlist| {
            BranchNode::new(
                &format!("{}{}", consts::PLAYLIST_PREFIX, playlist.id.0),
                playlist.title,
                IconName::Playlist,
            )
        })
        .collect();
}

/// Owns the catalogue; tasks return results and never mutate this state.
pub struct Source<N> {
    pub(super) branch: BranchNode,
    pub(super) client: Client<N>,
    pub(super) runtime: Handle,
    pub(super) cancel: CancelToken,
    pub(super) catalogue: Catalogue,
    pub(super) generation: u64,
    pub(super) pending: Option<Instant>,
    pub(super) active: Option<Active>,
    pub(super) playlists_active: bool,
    /// Tracks whose reaction request is in flight.
    pub(super) likes: HashSet<TrackId>,
    pub(super) faults: Faults,
    pub(super) found: UnboundedSender<Completed>,
    arrivals: UnboundedReceiver<Completed>,
}

pub(super) struct Active {
    pub(super) generation: u64,
    pub(super) cancel: CancelToken,
}

#[derive(Clone, Copy)]
pub(super) enum Operation {
    /// The current node's catalogue page and its stream batch.
    Page,
    Playlists,
    Like,
}

/// The current failure of each operation; a playlist or reaction failure
/// stands beside the page's own.
#[derive(Default)]
pub(super) struct Faults {
    like: Option<String>,
    page: Option<String>,
    playlists: Option<String>,
    text: String,
}

impl Faults {
    fn is_empty(&self) -> bool {
        self.page.is_none() && self.playlists.is_none() && self.like.is_none()
    }

    pub(super) fn set(&mut self, operation: Operation, fault: Option<String>) {
        *match operation {
            Operation::Page => &mut self.page,
            Operation::Playlists => &mut self.playlists,
            Operation::Like => &mut self.like,
        } = fault;
        self.text = [&self.page, &self.playlists, &self.like]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<&str>>()
            .join("; ");
    }
}

impl<N: Net + Clone + 'static> Source<N> {
    /// Derives the source's cancellation subtree from the supplied parent.
    ///
    /// # Errors
    /// Returns an error if a source caption is absent from the text document.
    fn new(
        client: Client<N>,
        runtime: Handle,
        cancel: &CancelToken,
        text: &TextDoc,
    ) -> Result<Self, UiDocError> {
        let mut branch = BranchNode::new(
            consts::ID,
            worded(text, "library.source.zvuk", consts::ID)?,
            IconName::Zvuk,
        );
        let mut playlists = BranchNode::new(
            consts::PLAYLISTS,
            worded(text, "zvuk.node.playlists", consts::ID)?,
            IconName::Folder,
        );
        playlists.unlisted = true;
        branch.children = vec![
            BranchNode::new(
                consts::SEARCH,
                worded(text, "zvuk.node.search", consts::ID)?,
                IconName::Search,
            ),
            BranchNode::new(
                consts::LIKED,
                worded(text, "zvuk.node.liked", consts::ID)?,
                IconName::Heart,
            ),
            playlists,
        ];
        let (found, arrivals) = mpsc::unbounded_channel();
        Ok(Self {
            branch,
            client,
            runtime,
            cancel: cancel.child(),
            found,
            arrivals,
            catalogue: Catalogue::default(),
            generation: 0,
            pending: None,
            active: None,
            playlists_active: false,
            likes: HashSet::new(),
            faults: Faults::default(),
        })
    }

    pub(super) fn queue(&mut self, due: Instant) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(active) = &self.active {
            active.cancel.cancel();
        }
        self.pending =
            (!self.cancel.is_cancelled() && self.catalogue.request().is_some()).then_some(due);
    }

    /// Registers the source's page, built once the package's text catalog is known.
    pub fn registered(client: Client<N>, runtime: Handle, cancel: CancelToken) -> Registration {
        Registration::new(page::page(), move |text| {
            Ok(Box::new(Self::new(client, runtime, &cancel, text)?))
        })
    }
}

impl Source<HttpClient> {
    /// Registers the Zvuk source and its library page from `sources.zvuk`.
    pub const FACTORY: Factory = Factory {
        id: consts::ID,
        register: Self::register,
    };

    fn register(environment: &Environment, context: Context) -> Result<Registration, Cause> {
        let config: Config = context.section()?;
        let page = page::document()?;
        let registration = Self::registered(
            Client::new(environment.net().clone(), &config),
            environment.runtime().clone(),
            context.cancel(),
        );
        Ok(registration.fill(PAGES, page))
    }
}

impl<N: Net + Clone + 'static> LibrarySource for Source<N> {
    fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if endpoint == "fault_hidden" {
            Some(ReadValue::Bool(self.faults.is_empty()))
        } else if endpoint == "fault" {
            Some(ReadValue::Text(&self.faults.text))
        } else {
            self.catalogue.read(endpoint)
        }
    }

    /// Row reactions carry their own stable track identity.
    fn write(&mut self, endpoint: &str, value: &WriteValue) {
        match (endpoint, value) {
            ("query", WriteValue::Text(query)) => {
                if self.catalogue.search(query) {
                    self.queue(Instant::now() + Duration::from_millis(consts::DEBOUNCE_MS));
                }
            }
            ("like_track", WriteValue::Text(id)) => self.like(id),
            _ => {}
        }
    }

    delegate::delegate! {
        to self.catalogue {
            fn analysis_key(&self, row: usize) -> Option<&str>;
            fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>>;
            fn row_key(&self, row: usize) -> Option<&str>;
        }
    }

    fn branch(&self) -> &BranchNode {
        &self.branch
    }

    /// Loads collection playlist nodes when their parent is expanded.
    fn expand(&mut self, node: &str) {
        self.list_playlists(node);
    }

    fn id(&self) -> &str {
        consts::ID
    }

    /// Changes the catalogue node, or retries the current node after a recoverable failure.
    fn select(&mut self, node: &str) {
        let Some(changed) = self.catalogue.select(node) else {
            return;
        };
        if self.faults.page.is_some()
            && self.active.is_none()
            && self.pending.is_none()
            && !self.cancel.is_cancelled()
        {
            self.faults.set(Operation::Page, None);
        } else if !changed {
            return;
        }
        self.queue(Instant::now());
    }

    /// Projects the current page state without hiding errors alongside rows.
    fn status(&self) -> PageStatus {
        if self.pending.is_some()
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.generation == self.generation)
        {
            PageStatus::Loading
        } else if !self.catalogue.is_empty() {
            PageStatus::Ready
        } else if self.faults.page.is_some() {
            PageStatus::Unreadable
        } else {
            PageStatus::Empty
        }
    }

    /// Drains asynchronous completions and starts due catalogue work.
    fn tick(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }
        while let Ok(completion) = self.arrivals.try_recv() {
            self.complete(completion);
            if self.cancel.is_cancelled() {
                return;
            }
        }
        self.start();
    }
}

impl<N> Drop for Source<N> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
