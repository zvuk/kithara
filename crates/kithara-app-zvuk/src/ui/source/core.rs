use std::collections::{BTreeMap, HashMap};

use kithara_app_library::{
    BranchNode, Cause, Context, Environment, Factory, KeyAccess, LibrarySource, PAGES, PageStatus,
    Registration, SECTIONS, Secrets, worded,
};
use kithara_net::{HttpClient, Net};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Duration, Instant},
    tokio::runtime::Handle,
};
use kithara_ui::{
    error::UiDocError,
    module::IconName,
    render::{ReadValue, TableRow, WriteValue},
    text::TextDoc,
};
use url::Url;

use crate::{
    Account, Client, Config, Error, Opener, Playlist, TrackId,
    job::Job,
    ui::{
        catalogue::Catalogue,
        consts, page,
        request::Batch,
        section::{self, Section},
    },
};

/// The branch's Playlists node.
pub(super) fn playlists(branch: &mut BranchNode) -> Option<&mut BranchNode> {
    branch
        .children
        .iter_mut()
        .find(|node| node.key == consts::PLAYLISTS)
}

/// Lists the collection's playlists under the branch's Playlists node.
pub(super) fn accept_playlists(branch: &mut BranchNode, listed: Vec<Playlist>) {
    let Some(node) = playlists(branch) else {
        return;
    };
    node.unlisted = false;
    node.count = u32::try_from(listed.len()).ok();
    node.children = listed
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

/// Forgets the listed playlists.
pub(super) fn unlist_playlists(branch: &mut BranchNode) {
    if let Some(node) = playlists(branch) {
        node.unlisted = true;
        node.count = None;
        node.children.clear();
    }
}

/// Owns the catalogue; its jobs return results and never mutate this state.
pub struct Source<N> {
    pub(super) branch: BranchNode,
    pub(super) client: Client<N>,
    pub(super) runtime: Handle,
    /// The plugin's cancellation, parent of every job.
    pub(super) cancel: CancelToken,
    pub(super) catalogue: Catalogue,
    pub(super) load: Load,
    /// The Playlists node was expanded; a new token lists it again.
    pub(super) expanded: bool,
    pub(super) listing: Option<Job<Result<Vec<Playlist>, Error>>>,
    /// Reactions in flight, by track; each confirms its reaction.
    pub(super) likes: HashMap<TrackId, Job<Result<bool, Error>>>,
    pub(super) faults: Faults,
    /// The account the catalogue reads its token from once per operation.
    pub(super) account: Account,
    section: Section,
    /// Whether the account held a token at the last tick.
    pub(super) has_token: bool,
    /// What the page states while the account holds no token.
    not_connected: String,
}

/// The current node's page request.
pub(super) enum Load {
    Idle,
    /// Waits until the instant to start.
    Due(Instant),
    Running(Job<Result<Batch, Error>>),
}

/// An operation whose failure the page shows; in the order they are shown.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    each: BTreeMap<Operation, String>,
    text: String,
}

impl Faults {
    /// Keeps the failure of `result` as `operation`'s, or clears it, and
    /// yields the value.
    pub(super) fn record<T>(
        &mut self,
        operation: Operation,
        result: Result<T, Error>,
    ) -> Option<T> {
        match &result {
            Ok(_) => self.each.remove(&operation),
            Err(error) => self.each.insert(operation, error.to_string()),
        };
        self.text = self
            .each
            .values()
            .map(String::as_str)
            .collect::<Vec<&str>>()
            .join("; ");
        result.ok()
    }

    fn has(&self, operation: Operation) -> bool {
        self.each.contains_key(&operation)
    }
}

impl<N: Net + Clone + 'static> Source<N> {
    /// # Errors
    /// Returns an error if a source caption is absent from the text document.
    fn new(
        client: Client<N>,
        runtime: Handle,
        cancel: CancelToken,
        mut account: Account,
        text: &TextDoc,
    ) -> Result<Self, UiDocError> {
        let mut branch = BranchNode::new(
            consts::ID,
            worded(text, "library.source.zvuk", consts::ID)?,
            IconName::Zvuk,
        );
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
            BranchNode::new(
                consts::PLAYLISTS,
                worded(text, "zvuk.node.playlists", consts::ID)?,
                IconName::Folder,
            ),
        ];
        unlist_playlists(&mut branch);
        let section = Section::new(text, account.row.borrow_and_update().clone())?;
        let has_token = account.token.borrow().is_some();
        let not_connected = worded(text, "zvuk.status.not_connected", "source.status")?;
        Ok(Self {
            branch,
            client,
            runtime,
            cancel,
            catalogue: Catalogue::default(),
            load: Load::Idle,
            expanded: false,
            listing: None,
            likes: HashMap::new(),
            faults: Faults::default(),
            account,
            section,
            has_token,
            not_connected,
        })
    }
}

#[bon::bon]
impl<N: Net + Clone + 'static> Source<N> {
    /// Registers the source over the caller's services and starts its account.
    ///
    /// # Errors
    /// Returns an error if a shipped document does not parse.
    #[builder]
    pub fn registered(
        client: Client<N>,
        secrets: Secrets,
        open: Arc<dyn Opener>,
        runtime: &Handle,
        cancel: CancelToken,
    ) -> Result<Registration, UiDocError> {
        let catalogue = page::document()?;
        let settings = page::section()?;
        let account = Account::spawn(client.clone(), secrets, open, runtime, cancel.child());
        let grant = KeyAccess::new(
            crate::consts::KEY_DOMAIN,
            crate::consts::AUTH_HEADER,
            account.token.clone(),
        );
        let runtime = runtime.clone();
        Ok(Registration::new(page::page(), move |text| {
            Ok(Box::new(Self::new(client, runtime, cancel, account, text)?))
        })
        .fill(PAGES, catalogue)
        .fill(SECTIONS, settings)
        .key_access(grant))
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
        let browser = environment.clone();
        Ok(Self::registered()
            .client(Client::new(environment.net().clone(), &config))
            .secrets(environment.secrets().clone())
            .open(Arc::new(move |url: &Url| browser.open_url(url)))
            .runtime(environment.runtime())
            .cancel(context.cancel())
            .call()?)
    }
}

impl<N: Net + Clone + 'static> LibrarySource for Source<N> {
    fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "fault_hidden" => Some(ReadValue::Bool(self.faults.each.is_empty())),
            "fault" => Some(ReadValue::Text(&self.faults.text)),
            _ => self
                .section
                .read(endpoint)
                .or_else(|| self.catalogue.read(endpoint)),
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
            (endpoint, WriteValue::Trigger) => {
                if let Some(command) = section::command(endpoint) {
                    let _ = self.account.commands.send(command);
                }
            }
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
        if node == consts::PLAYLISTS {
            self.expanded = true;
            self.list_playlists();
        }
    }

    fn id(&self) -> &str {
        consts::ID
    }

    /// Changes the catalogue node, or retries the current node after a recoverable failure.
    fn select(&mut self, node: &str) {
        let Some(changed) = self.catalogue.select(node) else {
            return;
        };
        if matches!(self.load, Load::Idle) && self.faults.has(Operation::Page) {
            self.faults.record(Operation::Page, Ok(()));
        } else if !changed {
            return;
        }
        self.queue(Instant::now());
    }

    /// Projects the current page state without hiding errors alongside rows.
    fn status(&self) -> PageStatus<'_> {
        if !self.has_token {
            PageStatus::Unreadable(Some(&self.not_connected))
        } else if !matches!(self.load, Load::Idle) {
            PageStatus::Loading
        } else if !self.catalogue.is_empty() {
            PageStatus::Ready
        } else if self.faults.has(Operation::Page) {
            PageStatus::Unreadable(None)
        } else {
            PageStatus::Empty
        }
    }

    /// Follows the account and takes the results of finished jobs, then
    /// starts due catalogue work.
    fn tick(&mut self) {
        if matches!(self.account.row.has_changed(), Ok(true)) {
            self.section.row = self.account.row.borrow_and_update().clone();
        }
        if matches!(self.account.token.has_changed(), Ok(true)) {
            self.has_token = self.account.token.borrow_and_update().is_some();
            self.reconnect();
        }
        self.settle();
        self.start();
    }
}
