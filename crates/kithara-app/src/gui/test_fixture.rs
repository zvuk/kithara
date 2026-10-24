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
            runtime::{Builder, Handle, Runtime},
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

pub(super) fn config(runtime: &Handle) -> AppConfig {
    let shutdown = CancelToken::root();
    let pools = pools::build(&PoolsSection::default()).expect("valid app pool policy");
    let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let net = HttpClient::new(
        NetOptions::builder().build(),
        pools.clone(),
        shutdown.child(),
    );
    let downloader = Downloader::new({
        let config = DownloaderConfig::for_client(net.clone());
        #[cfg(not(target_arch = "wasm32"))]
        let config = config.runtime(runtime.clone());
        config.build()
    });
    let store = AppStore::builder(pools)
        .backend(StorageBackend::Memory)
        .build();
    AppConfig::builder()
        .eq_bands(4)
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .net(net)
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build()
}

pub(super) fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds")
}

pub(super) fn runtime_and_config() -> (Runtime, AppConfig) {
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("test engine runtime builds");
    let config = config(runtime.handle());
    (runtime, config)
}

pub(super) fn boot(
    runtime: &Handle,
    config: &AppConfig,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    commands: UnboundedSender<Envelope>,
    plugins: Vec<Registration>,
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
        .plugins(plugins)
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
        config_path: String::new(),
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

/// A source only a test registers: a folder holding one more, and a leaf; or
/// a plugin that fills one settings section.
pub(super) struct Probe {
    id: &'static str,
    query: String,
    branch: BranchNode,
    calls: Rc<RefCell<Calls>>,
    /// Why its page cannot be read; none leaves the page empty.
    reason: Option<&'static str>,
}

/// What the library has told a [`Probe`] so far, in order.
#[derive(Default)]
pub(super) struct Calls {
    pub(super) expanded: Vec<String>,
    pub(super) selected: Vec<String>,
    /// How many times its rows were built.
    pub(super) rows: usize,
    pub(super) written: Vec<String>,
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

    /// A probe whose page cannot be read, for `reason`.
    pub(super) fn unreadable(label: &'static str, reason: &'static str) -> Registration {
        super::library::listed(Self::ID, move |text| {
            Ok(Box::new(Self {
                reason: Some(reason),
                ..Self::new(label, text, Rc::default())?
            }))
        })
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
            id: Self::ID,
            branch,
            calls,
            query: String::new(),
            reason: None,
        })
    }
}

impl LibrarySource for Probe {
    fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == "query").then_some(ReadValue::Text(&self.query))
    }
    fn write(&mut self, endpoint: &str, value: &WriteValue) {
        self.calls.borrow_mut().written.push(endpoint.to_owned());
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
        self.id
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

    fn status(&self) -> PageStatus<'_> {
        self.reason.map_or(PageStatus::Empty, |reason| {
            PageStatus::Unreadable(Some(reason))
        })
    }

    fn tick(&mut self) {}
}

#[cfg(feature = "masonry")]
pub(super) mod retained {
    use std::{cell::RefCell, rc::Rc};

    use ::kithara::{
        platform::{sync::Arc, tokio::sync::mpsc},
        ui::{
            app::{App, Config, Ui},
            draw::{Pt, Rect},
            ids::SourceUri,
            interact::{Input, MOUSE, PointerInput, PointerPhase},
            module::IconName,
            registry::{EndpointCategory, ValueKind},
            source::FillDocument,
        },
    };
    use arc_swap::ArcSwap;
    use kithara_app_library::{BranchNode, Endpoint, Registration, SECTIONS, SourcePage};

    use super::{Calls, Probe, boot};
    use crate::{engine::EngineSnapshot, gui::retained::Studio};

    impl Probe {
        /// A section titled `title` whose one 36px row presses `source.toggle`.
        pub(in crate::gui) fn section(
            id: &'static str,
            title: &str,
        ) -> (Registration, Rc<RefCell<Calls>>) {
            let calls = Rc::new(RefCell::new(Calls::default()));
            let told = Rc::clone(&calls);
            let document = FillDocument::parse(
                &format!(
                    r#"(
    schema: "kithara.module", version: 1, id: "{id}-section", chrome: Plain, parameters: ["source"],
    item: {{ "title": "{title}", "icon": "Disc" }},
    root: Column(id: "rows", size: (w: Fill, h: Shrink), children: [
        Pressable(
            id: "row",
            press: Command(id: "source.toggle", with: {{ "source": "$source" }}),
            child: Spacer(id: "row-face", size: Some((w: Fill, h: Fixed(36.0)))),
        ),
    ]),
)"#
                ),
                SourceUri(format!("{id}-section.kmodule.ron")),
            )
            .expect("the section parses");
            let page = SourcePage {
                id,
                endpoints: vec![Endpoint {
                    category: EndpointCategory::Command,
                    name: "toggle",
                    value: ValueKind::Trigger,
                }],
                texts: Vec::new(),
            };
            let registration = Registration::new(page, move |_| {
                Ok(Box::new(Self {
                    id,
                    query: String::new(),
                    branch: BranchNode::new(id, id.to_owned(), IconName::Folder),
                    calls: told,
                    reason: None,
                }))
            })
            .fill(SECTIONS, document);
            (registration, calls)
        }
    }

    /// Runs `check` on the retained studio at `size` over `plugins`, on a
    /// runtime of its own.
    pub(in crate::gui) fn studio(
        plugins: Vec<Registration>,
        size: (u32, u32),
        check: impl FnOnce(&mut Ui<'_, Studio>),
    ) {
        let (runtime, config) = super::runtime_and_config();
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let (commands, _receiver) = mpsc::unbounded_channel();
        let boot = boot(runtime.handle(), &config, snapshots, commands, plugins);
        let package = Rc::clone(&boot.ui.package);
        let mut ui = Ui::new(
            Studio::new(boot),
            Config::builder()
                .endpoints(package.registry())
                .resolver(package.resolver())
                .text(package.text())
                .build(),
            size,
            1.0,
        )
        .unwrap_or_else(|error| panic!("the studio must mount: {error}"));
        check(&mut ui);
    }

    /// The centre of the library tree's `row`.
    pub(in crate::gui) fn tree_row(ui: &mut Ui<'_, Studio>, row: u8) -> Pt {
        let tree = laid(ui, "library/tree").expect("the library draws its tree");
        let skin = &ui.app().skin().tree;
        Pt {
            x: tree.x + tree.w / 2.0,
            y: tree.y + skin.panel_padding_top + skin.row_height * (f32::from(row) + 0.5),
        }
    }

    /// Where `path` is drawn, if it is drawn with an area.
    pub(in crate::gui) fn laid(ui: &mut Ui<'_, Studio>, path: &str) -> Option<Rect> {
        ui.scene()
            .unwrap_or_else(|error| panic!("the studio must draw: {error}"));
        ui.rect_of(path).filter(|rect| rect.w > 0.0 && rect.h > 0.0)
    }

    pub(in crate::gui) fn click(ui: &mut Ui<'_, Studio>, at: Pt) {
        for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
            ui.input(Input::Pointer(PointerInput::new(
                MOUSE,
                None,
                phase,
                Some(at),
                1,
            )));
        }
    }

    /// Clicks the centre of `path`.
    pub(in crate::gui) fn press(ui: &mut Ui<'_, Studio>, path: &str) {
        let at = laid(ui, path).unwrap_or_else(|| panic!("`{path}` must be laid out"));
        click(
            ui,
            Pt {
                x: at.x + at.w / 2.0,
                y: at.y + at.h / 2.0,
            },
        );
    }
}
