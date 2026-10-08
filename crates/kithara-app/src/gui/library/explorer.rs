use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use kithara::{
    platform::tokio::{
        runtime::Handle,
        sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
        task,
    },
    ui::{error::UiDocError, module::IconName, render::TableRow, text::TextDoc},
};
use kithara_app_library::{BranchNode, LibrarySource, PageStatus, Registration, worded};
use tracing::debug;

use super::{
    folders::{FolderPicker, MusicFolders},
    listing::{self, Listing},
};

/// What a thread Explorer started hands back, taken in on the app's tick.
pub(super) enum Found {
    /// What listing the folder found.
    Listed(PathBuf, Listing),
    /// The folder the user picked to add to Music Folders.
    Picked(PathBuf),
}

/// The user's folders on disk: the ones added to Music Folders, and Home.
pub(in crate::gui) struct Explorer {
    runtime: Handle,
    branch: BranchNode,
    /// The latest listing of every folder listed so far.
    listings: HashMap<PathBuf, Listing>,
    /// The folders whose listing is running.
    listing: HashSet<PathBuf>,
    /// The folder each node of the branch stands for, by node key.
    nodes: HashMap<String, PathBuf>,
    folders: MusicFolders,
    home: Option<PathBuf>,
    names: Names,
    /// The folder of the selected node; none for a node that is no folder.
    shown: Option<PathBuf>,
    arrivals: UnboundedReceiver<Found>,
    found: UnboundedSender<Found>,
}

/// Explorer's fixed labels, as the catalog words them.
struct Names {
    home: String,
    music_folders: String,
}

impl Explorer {
    const FOLDERS: &str = "folders";
    const HOME: &str = "home";
    const ID: &'static str = "explorer";

    pub(in crate::gui) fn registered(
        home: Option<PathBuf>,
        runtime: Handle,
    ) -> (Registration, FolderPicker) {
        let (found, arrivals) = mpsc::unbounded_channel();
        let picker = FolderPicker::new(found.clone(), runtime.clone());
        let registration = super::listed(Self::ID, move |text| {
            Ok(Box::new(Self::new(home, runtime, found, arrivals, text)?))
        });
        (registration, picker)
    }

    fn new(
        home: Option<PathBuf>,
        runtime: Handle,
        found: UnboundedSender<Found>,
        arrivals: UnboundedReceiver<Found>,
        text: &TextDoc,
    ) -> Result<Self, UiDocError> {
        let id = Self::ID;
        let label = worded(text, "library.source.explorer", id)?;
        let names = Names {
            home: worded(text, "library.node.home", id)?,
            music_folders: worded(text, "library.node.music_folders", id)?,
        };
        let mut explorer = Self {
            runtime,
            home,
            found,
            arrivals,
            names,
            branch: BranchNode::new(id, label, IconName::Monitor),
            folders: MusicFolders::default(),
            listings: HashMap::new(),
            listing: HashSet::new(),
            nodes: HashMap::new(),
            shown: None,
        };
        explorer.rebuild();
        Ok(explorer)
    }

    fn folder(
        &self,
        top: &str,
        path: &Path,
        (label, icon): (String, IconName),
        nodes: &mut HashMap<String, PathBuf>,
    ) -> BranchNode {
        let key = format!("{top}:{}", path.display());
        let mut folder = BranchNode::new(&key, label, icon);
        folder.page = true;
        nodes.insert(key, path.to_path_buf());
        match self.listings.get(path) {
            Some(Listing::Listed(listed)) => {
                folder.children = listed
                    .folders
                    .iter()
                    .map(|sub| self.folder(top, sub, named(sub), nodes))
                    .collect();
            }
            Some(Listing::Failed) | None => folder.unlisted = true,
        }
        folder
    }

    fn list(&mut self, folder: PathBuf) {
        if !self.listing.insert(folder.clone()) {
            return;
        }
        let found = self.found.clone();
        drop(task::spawn_blocking_on(&self.runtime, move || {
            let listing = listing::list(&folder);
            if found.send(Found::Listed(folder, listing)).is_err() {
                debug!("the library closed before a folder listing arrived");
            }
        }));
    }

    fn rebuild(&mut self) {
        let mut nodes = HashMap::new();
        let mut folders = BranchNode::new(
            Self::FOLDERS,
            self.names.music_folders.clone(),
            IconName::Folder,
        );
        folders.count = u32::try_from(self.folders.list().len()).ok();
        folders.children = self
            .folders
            .list()
            .iter()
            .map(|folder| self.folder(Self::FOLDERS, folder, named(folder), &mut nodes))
            .collect();
        let mut children = vec![folders];
        if let Some(home) = &self.home {
            let label = (self.names.home.clone(), IconName::Home);
            children.push(self.folder(Self::HOME, home, label, &mut nodes));
        }
        self.branch.children = children;
        self.nodes = nodes;
    }

    fn shown(&self) -> Option<&Listing> {
        self.shown
            .as_ref()
            .and_then(|folder| self.listings.get(folder))
    }
}

impl LibrarySource for Explorer {
    fn analysis_key(&self, row: usize) -> Option<&str> {
        match self.shown() {
            Some(Listing::Listed(folder)) => folder.tracks.get(row).map(super::track::Track::key),
            _ => None,
        }
    }

    fn branch(&self) -> &BranchNode {
        &self.branch
    }

    fn expand(&mut self, node: &str) {
        if let Some(folder) = self.nodes.get(node).cloned() {
            self.list(folder);
        }
    }

    fn id(&self) -> &str {
        Self::ID
    }

    fn rows(&self, selected: Option<&str>) -> Vec<TableRow<'_>> {
        match self.shown() {
            Some(Listing::Listed(folder)) => folder
                .tracks
                .iter()
                .map(|track| track.row(selected == Some(track.url())))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn row_key(&self, row: usize) -> Option<&str> {
        match self.shown() {
            Some(Listing::Listed(folder)) => folder.tracks.get(row).map(super::track::Track::url),
            _ => None,
        }
    }

    fn select(&mut self, node: &str) {
        self.shown = self.nodes.get(node).cloned();
        if let Some(folder) = self.shown.clone() {
            self.list(folder);
        }
    }

    fn status(&self) -> PageStatus {
        let Some(folder) = &self.shown else {
            return PageStatus::Empty;
        };
        match self.listings.get(folder) {
            None => PageStatus::Loading,
            Some(Listing::Failed) => PageStatus::Unreadable,
            Some(Listing::Listed(listed)) if listed.tracks.is_empty() => PageStatus::Empty,
            Some(Listing::Listed(_)) => PageStatus::Ready,
        }
    }

    fn tick(&mut self) {
        let mut arrived = false;
        while let Ok(found) = self.arrivals.try_recv() {
            match found {
                Found::Listed(folder, listing) => {
                    self.listing.remove(&folder);
                    self.listings.insert(folder, listing);
                }
                Found::Picked(folder) => self.folders.add(folder),
            }
            arrived = true;
        }
        if arrived {
            self.rebuild();
        }
    }
}

fn named(folder: &Path) -> (String, IconName) {
    let name = folder.file_name().map_or_else(
        || folder.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    (name, IconName::Folder)
}
