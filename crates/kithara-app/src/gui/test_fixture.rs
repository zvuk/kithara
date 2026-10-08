use std::{cell::RefCell, collections::BTreeMap, io::Cursor, path::Path, rc::Rc};

use arc_swap::ArcSwap;
use iced::window::Id;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use kithara::{
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{
            runtime::Handle,
            sync::mpsc::{self, UnboundedSender},
        },
    },
    play::{PlayWorkerConfig, policy::DomainKeyPolicy},
    ui::{
        error::UiDocError,
        module::IconName,
        render::{ReadValue, TableRow, WriteValue},
        source::UiConfig,
        text::TextDoc,
    },
};
use kithara_app_library::{BranchNode, LibrarySource, PageStatus, Playable, Registration, worded};

use super::{
    app::Kithara,
    frontend::Boot,
    library::{Library, SourceAdditions, StartupSource},
    ui::{AppUi, package::Package},
};
use crate::{
    config::{AppConfig, AppDrm},
    engine::{EngineSnapshot, Envelope},
    pools::{self, AppStore, AppWorker, PoolsSection},
    theme::Palette,
};

pub(super) fn config() -> AppConfig {
    let shutdown = CancelToken::root();
    let pools = pools::build(&PoolsSection::default()).expect("valid app pool policy");
    let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let net = HttpClient::new(
        NetOptions::builder().build(),
        pools.clone(),
        shutdown.child(),
    );
    let downloader = Downloader::new(DownloaderConfig::for_client(net.clone()).build());
    let store = AppStore::builder(pools)
        .backend(StorageBackend::Memory)
        .build();
    AppConfig::builder()
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .net(net)
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build()
}

pub(super) fn runtime() -> kithara::platform::tokio::runtime::Runtime {
    kithara::platform::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds")
}

pub(super) fn boot(
    runtime: &Handle,
    config: &AppConfig,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    commands: UnboundedSender<Envelope>,
) -> Boot {
    Boot::builder()
        .settings(&config.ui)
        .tracks(vec![
            "/music/local.flac".to_string(),
            "https://example.test/stream.m3u8".to_string(),
        ])
        .palette(config.palette)
        .snapshots(snapshots)
        .commands(commands)
        .runtime(runtime.clone())
        .net(&config.net)
        .sources(&config.sources)
        .shutdown(&config.shutdown)
        .build()
        .expect("shipped UI compiles")
}

pub(super) fn mount(
    root: Option<&Path>,
    registered: Vec<Registration>,
) -> Result<(Rc<Package>, Library), UiDocError> {
    let package = Package::load(
        root,
        SourceAdditions::new(&registered),
        &UiConfig::default().limits,
    )?;
    let library = Library::new(registered, package.text())?;
    Ok((package, library))
}

/// The application mounted on `package` and `library`, with no engine behind it.
pub(super) fn mounted(package: Rc<Package>, library: Library, runtime: &Handle) -> Kithara {
    let (commands, _) = mpsc::unbounded_channel();
    let boot = Boot {
        ui: AppUi::new(package, &UiConfig::default(), runtime.clone()).expect("the UI compiles"),
        snapshots: Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished())),
        library,
        #[cfg(not(target_arch = "wasm32"))]
        picker: super::library::Explorer::registered(None, runtime.clone()).1,
        palette: Palette::default(),
        #[cfg(feature = "masonry")]
        settings: UiConfig::default(),
        commands,
    };
    Kithara::mounted(boot, Id::unique())
}

/// The record a library row drags onto a deck, naming the source it plays.
pub(super) fn dragged(source: &str) -> BTreeMap<String, String> {
    Playable::new(source.to_owned()).into()
}

/// A 4x2 cover of one `color`, encoded as `format`.
pub(super) fn cover(color: [u8; 3], format: ImageFormat) -> Arc<Vec<u8>> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 2, Rgb(color)));
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, format)
        .expect("fixture encodes");
    Arc::new(output.into_inner())
}

pub(super) fn package(root: Option<&Path>) -> Result<Rc<Package>, UiDocError> {
    Package::load(
        root,
        SourceAdditions::new(&[StartupSource::registered(Vec::new())]),
        &UiConfig::default().limits,
    )
}

/// A source only a test registers: a folder holding one more, and a leaf.
pub(super) struct Probe {
    query: String,
    branch: BranchNode,
    calls: Rc<RefCell<Calls>>,
}

/// The nodes the library has told a [`Probe`] about so far, in order.
#[derive(Default)]
pub(super) struct Calls {
    pub(super) expanded: Vec<String>,
    pub(super) selected: Vec<String>,
    /// How many times its rows were built.
    pub(super) rows: usize,
}

impl Probe {
    pub(super) const ID: &'static str = "probe";

    pub(super) fn registered(label: &'static str) -> (Registration, Rc<RefCell<Calls>>) {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let told = Rc::clone(&calls);
        let registration = super::library::listed(Self::ID, move |text| {
            Ok(Box::new(Self::new(label, text, told)?))
        });
        (registration, calls)
    }

    fn new(label: &str, text: &TextDoc, calls: Rc<RefCell<Calls>>) -> Result<Self, UiDocError> {
        let node = |key: &str, children| BranchNode {
            children,
            ..BranchNode::new(key, key.to_owned(), IconName::Folder)
        };
        let mut branch = node(
            Self::ID,
            vec![
                node("crate", vec![node("digger", Vec::new())]),
                node("leaf", Vec::new()),
            ],
        );
        branch.label = worded(text, label, Self::ID)?;
        Ok(Self {
            branch,
            calls,
            query: String::new(),
        })
    }
}

impl LibrarySource for Probe {
    fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == "query").then_some(ReadValue::Text(&self.query))
    }
    fn write(&mut self, endpoint: &str, value: &WriteValue) {
        if let ("query", WriteValue::Text(query)) = (endpoint, value) {
            self.query.clone_from(query);
        }
    }

    fn analysis_key(&self, _row: usize) -> Option<&str> {
        None
    }

    fn branch(&self) -> &BranchNode {
        &self.branch
    }

    fn expand(&mut self, node: &str) {
        self.calls.borrow_mut().expanded.push(node.to_owned());
    }

    fn id(&self) -> &str {
        Self::ID
    }

    fn row_key(&self, _row: usize) -> Option<&str> {
        None
    }

    fn rows(&self, _selected: Option<&str>) -> Vec<TableRow<'_>> {
        self.calls.borrow_mut().rows += 1;
        Vec::new()
    }

    fn select(&mut self, node: &str) {
        self.calls.borrow_mut().selected.push(node.to_owned());
    }

    fn status(&self) -> PageStatus {
        PageStatus::Empty
    }

    fn tick(&mut self) {}
}
