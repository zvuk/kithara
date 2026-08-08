mod capture;
mod compare;
#[cfg(feature = "masonry-host")]
mod host;
#[cfg(feature = "masonry-host")]
mod masonry_shots;
mod mock;
mod mock_data;
mod mock_mixer;
mod mock_stress;
mod mock_transport;
mod offscreen;
mod sections;

use iced::{Element, Size, Subscription, Task, Theme, time as iced_time, window, window::Settings};
use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::{Skin, UiEvent, WindowCommand, fonts, tree},
    source::{MemResolver, UiConfig},
};

use self::{
    capture::{Capture, Shot},
    mock::MockReads,
    sections::{ModuleDemo, Tab},
};

struct Consts;

impl Consts {
    const HEIGHT: f32 = 720.0;
    const STRESS_TICK_MS: u64 = 16;
    const WIDTH: f32 = 1100.0;
}

const ASSETS: &[(&str, &str)] = &[
    (
        "gallery-atoms.klayout.ron",
        include_str!("assets/gallery-atoms.klayout.ron"),
    ),
    (
        "gallery-buttons.klayout.ron",
        include_str!("assets/gallery-buttons.klayout.ron"),
    ),
    (
        "gallery-cells.klayout.ron",
        include_str!("assets/gallery-cells.klayout.ron"),
    ),
    (
        "gallery-chrome.klayout.ron",
        include_str!("assets/gallery-chrome.klayout.ron"),
    ),
    (
        "gallery-faders.klayout.ron",
        include_str!("assets/gallery-faders.klayout.ron"),
    ),
    (
        "gallery-library2.klayout.ron",
        include_str!("assets/gallery-library2.klayout.ron"),
    ),
    (
        "gallery-menu.klayout.ron",
        include_str!("assets/gallery-menu.klayout.ron"),
    ),
    (
        "gallery-micro.klayout.ron",
        include_str!("assets/gallery-micro.klayout.ron"),
    ),
    (
        "gallery-mixer.klayout.ron",
        include_str!("assets/gallery-mixer.klayout.ron"),
    ),
    (
        "gallery-modules-deck-micro.klayout.ron",
        include_str!("assets/gallery-modules-deck-micro.klayout.ron"),
    ),
    (
        "gallery-modules-global-bar.klayout.ron",
        include_str!("assets/gallery-modules-global-bar.klayout.ron"),
    ),
    (
        "gallery-modules-layout.klayout.ron",
        include_str!("assets/gallery-modules-layout.klayout.ron"),
    ),
    (
        "gallery-modules-telemetry.klayout.ron",
        include_str!("assets/gallery-modules-telemetry.klayout.ron"),
    ),
    (
        "gallery-modules.klayout.ron",
        include_str!("assets/gallery-modules.klayout.ron"),
    ),
    (
        "gallery-sizes.klayout.ron",
        include_str!("assets/gallery-sizes.klayout.ron"),
    ),
    (
        "gallery-stress.klayout.ron",
        include_str!("assets/gallery-stress.klayout.ron"),
    ),
    (
        "gallery-titlebars.klayout.ron",
        include_str!("assets/gallery-titlebars.klayout.ron"),
    ),
    (
        "gallery-tokens.klayout.ron",
        include_str!("assets/gallery-tokens.klayout.ron"),
    ),
    (
        "gallery-tracklist.klayout.ron",
        include_str!("assets/gallery-tracklist.klayout.ron"),
    ),
    (
        "gallery-tree.klayout.ron",
        include_str!("assets/gallery-tree.klayout.ron"),
    ),
    (
        "gallery-typography.klayout.ron",
        include_str!("assets/gallery-typography.klayout.ron"),
    ),
    (
        "gallery-vis.klayout.ron",
        include_str!("assets/gallery-vis.klayout.ron"),
    ),
    (
        "modules/app-menu.kmodule.ron",
        include_str!("../../assets/modules/app-menu.kmodule.ron"),
    ),
    (
        "modules/app-menu/hint-row.kmodule.ron",
        include_str!("../../assets/modules/app-menu/hint-row.kmodule.ron"),
    ),
    (
        "modules/app-menu/layout-row.kmodule.ron",
        include_str!("../../assets/modules/app-menu/layout-row.kmodule.ron"),
    ),
    (
        "modules/app-menu/module-cell.kmodule.ron",
        include_str!("../../assets/modules/app-menu/module-cell.kmodule.ron"),
    ),
    (
        "modules/app-menu/toggle-row.kmodule.ron",
        include_str!("../../assets/modules/app-menu/toggle-row.kmodule.ron"),
    ),
    (
        "modules/app-menu/window-row.kmodule.ron",
        include_str!("../../assets/modules/app-menu/window-row.kmodule.ron"),
    ),
    (
        "modules/module-deck-micro.kmodule.ron",
        include_str!("assets/modules/module-deck-micro.kmodule.ron"),
    ),
    (
        "modules/module-deck.kmodule.ron",
        include_str!("assets/modules/module-deck.kmodule.ron"),
    ),
    (
        "modules/module-global-bar.kmodule.ron",
        include_str!("assets/modules/module-global-bar.kmodule.ron"),
    ),
    (
        "modules/module-layout.kmodule.ron",
        include_str!("assets/modules/module-layout.kmodule.ron"),
    ),
    (
        "modules/module-tabs.kmodule.ron",
        include_str!("assets/modules/module-tabs.kmodule.ron"),
    ),
    (
        "modules/module-telemetry.kmodule.ron",
        include_str!("assets/modules/module-telemetry.kmodule.ron"),
    ),
    (
        "modules/nav.kmodule.ron",
        include_str!("assets/modules/nav.kmodule.ron"),
    ),
    (
        "modules/nav/item.kmodule.ron",
        include_str!("assets/modules/nav/item.kmodule.ron"),
    ),
    (
        "modules/primitives/chips.kmodule.ron",
        include_str!("assets/modules/primitives/chips.kmodule.ron"),
    ),
    (
        "modules/primitives/knobs.kmodule.ron",
        include_str!("assets/modules/primitives/knobs.kmodule.ron"),
    ),
    (
        "modules/primitives/meters.kmodule.ron",
        include_str!("assets/modules/primitives/meters.kmodule.ron"),
    ),
    (
        "modules/primitives/readouts.kmodule.ron",
        include_str!("assets/modules/primitives/readouts.kmodule.ron"),
    ),
    (
        "modules/primitives/toggles.kmodule.ron",
        include_str!("assets/modules/primitives/toggles.kmodule.ron"),
    ),
    (
        "modules/stress.kmodule.ron",
        include_str!("assets/modules/stress.kmodule.ron"),
    ),
    (
        "modules/tabs/atoms.kmodule.ron",
        include_str!("assets/modules/tabs/atoms.kmodule.ron"),
    ),
    (
        "modules/tabs/buttons.kmodule.ron",
        include_str!("assets/modules/tabs/buttons.kmodule.ron"),
    ),
    (
        "modules/tabs/cells.kmodule.ron",
        include_str!("assets/modules/tabs/cells.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-full-all.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-full-all.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-join-left.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-join-left.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-join-right.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-join-right.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-open-top.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-open-top.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-row-a.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-row-a.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-row-b.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-row-b.kmodule.ron"),
    ),
    (
        "modules/tabs/chrome-row-c.kmodule.ron",
        include_str!("assets/modules/tabs/chrome-row-c.kmodule.ron"),
    ),
    (
        "modules/tabs/faders.kmodule.ron",
        include_str!("assets/modules/tabs/faders.kmodule.ron"),
    ),
    (
        "modules/tabs/library2.kmodule.ron",
        include_str!("assets/modules/tabs/library2.kmodule.ron"),
    ),
    (
        "modules/tabs/menu-context.kmodule.ron",
        include_str!("assets/modules/tabs/menu-context.kmodule.ron"),
    ),
    (
        "modules/tabs/menu-context/track-row.kmodule.ron",
        include_str!("assets/modules/tabs/menu-context/track-row.kmodule.ron"),
    ),
    (
        "modules/tabs/menu-notes.kmodule.ron",
        include_str!("assets/modules/tabs/menu-notes.kmodule.ron"),
    ),
    (
        "modules/tabs/micro-4a.kmodule.ron",
        include_str!("assets/modules/tabs/micro-4a.kmodule.ron"),
    ),
    (
        "modules/tabs/micro-4b.kmodule.ron",
        include_str!("assets/modules/tabs/micro-4b.kmodule.ron"),
    ),
    (
        "modules/tabs/micro-4c.kmodule.ron",
        include_str!("assets/modules/tabs/micro-4c.kmodule.ron"),
    ),
    (
        "modules/tabs/micro-4d.kmodule.ron",
        include_str!("assets/modules/tabs/micro-4d.kmodule.ron"),
    ),
    (
        "modules/tabs/mixer-1d.kmodule.ron",
        include_str!("assets/modules/tabs/mixer-1d.kmodule.ron"),
    ),
    (
        "modules/tabs/mixer-1g.kmodule.ron",
        include_str!("assets/modules/tabs/mixer-1g.kmodule.ron"),
    ),
    (
        "modules/tabs/mixer-label.kmodule.ron",
        include_str!("assets/modules/tabs/mixer-label.kmodule.ron"),
    ),
    (
        "modules/tabs/sizes.kmodule.ron",
        include_str!("assets/modules/tabs/sizes.kmodule.ron"),
    ),
    (
        "modules/tabs/titlebars.kmodule.ron",
        include_str!("assets/modules/tabs/titlebars.kmodule.ron"),
    ),
    (
        "modules/tabs/tokens-anatomy.kmodule.ron",
        include_str!("assets/modules/tabs/tokens-anatomy.kmodule.ron"),
    ),
    (
        "modules/tabs/tokens-notes.kmodule.ron",
        include_str!("assets/modules/tabs/tokens-notes.kmodule.ron"),
    ),
    (
        "modules/tabs/tokens.kmodule.ron",
        include_str!("assets/modules/tabs/tokens.kmodule.ron"),
    ),
    (
        "modules/tabs/tracklist.kmodule.ron",
        include_str!("assets/modules/tabs/tracklist.kmodule.ron"),
    ),
    (
        "modules/tabs/tree.kmodule.ron",
        include_str!("assets/modules/tabs/tree.kmodule.ron"),
    ),
    (
        "modules/tabs/typography.kmodule.ron",
        include_str!("assets/modules/tabs/typography.kmodule.ron"),
    ),
    (
        "modules/tabs/vis-spacer.kmodule.ron",
        include_str!("assets/modules/tabs/vis-spacer.kmodule.ron"),
    ),
    (
        "modules/tabs/vis.kmodule.ron",
        include_str!("assets/modules/tabs/vis.kmodule.ron"),
    ),
    (
        "modules/titlebar.kmodule.ron",
        include_str!("assets/modules/titlebar.kmodule.ron"),
    ),
];

#[derive(Clone, Debug)]
enum Message {
    Close(window::Id),
    Tick,
    Ui(UiEvent),
    /// Move to the next page to photograph, or finish and exit.
    CaptureNext,
    /// The page is on screen; ask the window for its pixels.
    CaptureShoot(Shot),
    /// Write one page to disk.
    CaptureSave(Shot, window::Screenshot),
}

struct Gallery {
    skin: &'static Skin,
    window_id: window::Id,
    reads: MockReads,
    layouts: [CompiledUi; Tab::ALL.len()],
    module_layouts: [CompiledUi; ModuleDemo::ALL.len()],
    capture: Option<Capture>,
}

impl Gallery {
    /// The gallery with no window of iced's: the offscreen capture rasterises
    /// the same documents itself, and never opens one.
    fn mounted() -> Self {
        let resolver = resolver();
        let endpoints = mock::registry();
        Self {
            layouts: Tab::ALL.map(|tab| compiled(tab.entry(), &resolver, &endpoints)),
            module_layouts: ModuleDemo::ALL
                .map(|module| compiled(module.entry(), &resolver, &endpoints)),
            window_id: window::Id::unique(),
            skin: builtin::skin(),
            reads: MockReads::default(),
            capture: None,
        }
    }

    /// Turns to the page a shot names.
    fn select(&mut self, shot: Shot) {
        self.reads.select_tab(shot.tab);
        if let Some(module) = shot.module {
            self.reads.select_module(module);
        }
    }

    fn new() -> (Self, Task<Message>) {
        let resolver = resolver();
        let endpoints = mock::registry();
        let layouts = Tab::ALL.map(|tab| {
            compile(
                tab.entry(),
                &resolver,
                &endpoints,
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "embedded gallery document {} must compile: {error}",
                    tab.entry()
                )
            })
        });
        let module_layouts = ModuleDemo::ALL.map(|module| {
            compile(
                module.entry(),
                &resolver,
                &endpoints,
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "embedded gallery document {} must compile: {error}",
                    module.entry()
                )
            })
        });
        let settings = Settings {
            size: Size::new(Consts::WIDTH, Consts::HEIGHT),
            min_size: Some(Size::new(Consts::WIDTH, Consts::HEIGHT)),
            decorations: false,
            exit_on_close_request: false,
            ..Settings::default()
        };
        let (window_id, open) = window::open(settings);
        let capture = Capture::requested();
        let start = if capture.is_some() {
            Task::done(Message::CaptureNext)
        } else {
            Task::none()
        };
        (
            Self {
                layouts,
                module_layouts,
                window_id,
                skin: builtin::skin(),
                reads: MockReads::default(),
                capture,
            },
            open.discard().chain(start),
        )
    }

    fn compiled(&self) -> &CompiledUi {
        if self.reads.active_tab() == Tab::Modules {
            &self.module_layouts[self.reads.active_module().index()]
        } else {
            &self.layouts[self.reads.active_tab().index()]
        }
    }

    fn select_tab(&mut self, tab: Tab) {
        self.reads.select_tab(tab);
    }

    /// Selects the next page and lets one frame render before the shot.
    fn capture_next(&mut self) -> Task<Message> {
        let Some(capture) = self.capture.as_mut() else {
            return Task::none();
        };
        let Some(shot) = capture.next() else {
            capture.report();
            return iced::exit();
        };
        self.select(shot);
        Task::done(Message::CaptureShoot(shot))
    }

    fn capture_save(&mut self, shot: Shot, image: &window::Screenshot) -> Task<Message> {
        let Some(capture) = self.capture.as_mut() else {
            return Task::none();
        };
        match capture.save(shot, image) {
            Ok(path) => println!("captured {} ({} left)", path.display(), capture.remaining()),
            Err(error) => eprintln!("capture failed: {error}"),
        }
        Task::done(Message::CaptureNext)
    }
}

fn main() -> iced::Result {
    match compare::run() {
        compare::Verdict::Passed => return Ok(()),
        // A gate says so with its exit code; iced's error type has no shape for
        // "the two hosts disagree", and inventing one would say less.
        compare::Verdict::Failed => std::process::exit(1),
        compare::Verdict::NotAsked => {}
    }
    if offscreen::run() {
        return Ok(());
    }
    #[cfg(feature = "masonry-host")]
    if masonry_shots::run() || host::run() {
        return Ok(());
    }
    let daemon = iced::daemon(Gallery::new, update, view)
        .title(|_state: &Gallery, _window| "Kithara UI Gallery".to_owned())
        .theme(|state: &Gallery, _window| theme(state.skin))
        .subscription(subscription)
        .default_font(fonts::SANS);
    fonts::FONT_BYTES
        .iter()
        .fold(daemon, |daemon, bytes| daemon.font(*bytes))
        .run()
}

fn update(state: &mut Gallery, message: Message) -> Task<Message> {
    match message {
        Message::Close(id) if id == state.window_id => iced::exit(),
        Message::Close(id) => window::close(id),
        Message::Tick => {
            state.reads.tick();
            Task::none()
        }
        Message::Ui(UiEvent::Control { path, action }) => {
            if let Ok(tab) = Tab::try_from(path.as_str()) {
                state.select_tab(tab);
            } else {
                state.reads.apply(&path, &action);
            }
            Task::none()
        }
        Message::Ui(UiEvent::LibraryQuery(query)) => {
            state.reads.set_library_query(query);
            Task::none()
        }
        Message::Ui(UiEvent::ToggleModule(module)) => {
            state.reads.toggle_module(module);
            Task::none()
        }
        Message::Ui(UiEvent::Window(command)) => match command {
            WindowCommand::Drag => window::drag(state.window_id),
            WindowCommand::Minimize => window::minimize(state.window_id, true),
            WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
            WindowCommand::Close => iced::exit(),
            _ => Task::none(),
        },
        Message::Ui(_) => Task::none(),
        Message::CaptureNext => state.capture_next(),
        Message::CaptureShoot(shot) => {
            window::screenshot(state.window_id).map(move |image| Message::CaptureSave(shot, image))
        }
        Message::CaptureSave(shot, image) => state.capture_save(shot, &image),
    }
}

fn view(state: &Gallery, _window: window::Id) -> Element<'_, Message> {
    tree::render(
        &state.compiled().root,
        state.compiled(),
        &state.reads,
        state.skin,
    )
    .map(Message::Ui)
}

fn subscription(state: &Gallery) -> Subscription<Message> {
    let close = window::close_requests().map(Message::Close);
    if matches!(state.reads.active_tab(), Tab::Stress | Tab::Vis) {
        Subscription::batch([
            close,
            iced_time::every(Duration::from_millis(Consts::STRESS_TICK_MS)).map(|_| Message::Tick),
        ])
    } else {
        close
    }
}

/// The window every capture is taken at, so the three sets line up without
/// anyone stating the geometry twice.
fn window_size() -> Size {
    Size::new(Consts::WIDTH, Consts::HEIGHT)
}

fn compiled(
    entry: &str,
    resolver: &MemResolver,
    endpoints: &dyn kithara_ui::registry::EndpointRegistry,
) -> CompiledUi {
    compile(
        entry,
        resolver,
        endpoints,
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap_or_else(|error| panic!("embedded gallery document {entry} must compile: {error}"))
}

fn resolver() -> MemResolver {
    let mut resolver = builtin::resolver();
    for (path, text) in ASSETS {
        resolver.insert(path, text);
    }
    resolver
}

fn theme(skin: &Skin) -> Theme {
    let palette = skin.palette;
    Theme::custom(
        "Kithara".to_owned(),
        iced::theme::Palette {
            background: palette.bg.into(),
            text: palette.text.into(),
            primary: palette.accent.into(),
            success: palette.success.into(),
            danger: palette.danger.into(),
            warning: palette.warning.into(),
        },
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::CompiledNode,
        expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
        module::{ButtonStyle, ChromeStyle, IconName, WaveStyle},
        render::{ControlAction, Reads},
    };

    use super::*;

    #[kithara::test]
    fn every_module_demo_compiles_with_full_chrome() {
        let resolver = resolver();
        let endpoints = mock::registry();

        for module in ModuleDemo::ALL {
            let ui = compile(
                module.entry(),
                &resolver,
                &endpoints,
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap();
            let CompiledNode::Split { children, .. } = &ui.root else {
                panic!("expected gallery split");
            };
            let CompiledNode::Split {
                children: gallery_children,
                ..
            } = &children[1].1
            else {
                panic!("expected gallery content");
            };
            let CompiledNode::Split {
                children: module_children,
                ..
            } = &gallery_children[1].1
            else {
                panic!("expected module demo stack");
            };
            let CompiledNode::Module {
                title,
                chip,
                chrome,
                footer,
                ..
            } = &module_children[1].1
            else {
                panic!("expected module demo");
            };

            assert_eq!(*chrome, ChromeStyle::Full, "{}", module.entry());
            assert!(title.is_some(), "{}", module.entry());
            assert!(chip.is_some(), "{}", module.entry());
            assert!(footer.is_some(), "{}", module.entry());
        }
    }

    #[kithara::test]
    fn every_gallery_tab_compiles() {
        let resolver = resolver();
        let endpoints = mock::registry();

        for tab in Tab::ALL {
            compile(
                tab.entry(),
                &resolver,
                &endpoints,
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{} must compile: {error}", tab.entry()));
        }
    }

    #[kithara::test]
    fn the_hosted_meters_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Atoms,
            "meters",
            |path| path.contains("/meters/"),
            &[
                ("atoms/meters/stereo", "stereo-meter"),
                ("atoms/meters/vertical-120", "vertical-vu"),
                ("atoms/meters/vertical-64", "vertical-vu"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_knobs_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Atoms,
            "knobs",
            |path| path.contains("/knobs/"),
            &[
                ("atoms/knobs/size-26", "knob"),
                ("atoms/knobs/size-28", "knob"),
                ("atoms/knobs/size-34", "knob"),
                ("atoms/knobs/size-38", "knob"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_toggles_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Atoms,
            "toggles",
            |path| path.contains("/toggles/"),
            &[
                ("atoms/toggles/checkbox-off", "activation"),
                ("atoms/toggles/checkbox-on", "activation"),
                ("atoms/toggles/toggle-off", "activation"),
                ("atoms/toggles/toggle-on", "activation"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_chips_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Atoms,
            "chips",
            |path| path.contains("/chips/"),
            &[
                ("atoms/chips/active", "activation"),
                ("atoms/chips/inactive", "activation"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_buttons_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Buttons,
            "buttons",
            |path| path.starts_with("buttons/"),
            &[
                ("buttons/cue", "activation"),
                ("buttons/default", "activation"),
                ("buttons/micro", "activation"),
                ("buttons/play", "activation"),
                ("buttons/primary", "activation"),
                ("buttons/sync", "activation"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_faders_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Faders,
            "faders",
            |path| path.starts_with("faders/"),
            &[
                ("faders/default", "fader"),
                ("faders/vertical", "vertical-vu"),
                ("faders/volume", "fader"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_tree_keeps_its_exact_descriptor_inventory() {
        assert_hosted_page_claims(
            Tab::Tree,
            "tree",
            |path| path.starts_with("tree/"),
            &[
                ("tree/browser", "scroll"),
                ("tree/browser/search", "text-input"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_library_keeps_its_exact_descriptor_inventory() {
        assert_hosted_page_claims(
            Tab::Library2,
            "library",
            |path| path.starts_with("library2/"),
            &[
                ("library2/browser", "scroll"),
                ("library2/browser/search", "text-input"),
                ("library2/context", "picker"),
                ("library2/table", "track-list"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_track_list_keeps_its_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Tracklist,
            "track-list",
            |path| path.starts_with("tracklist/"),
            &[
                ("tracklist/column-artist", "activation"),
                ("tracklist/column-bpm", "activation"),
                ("tracklist/column-deck", "activation"),
                ("tracklist/column-energy", "activation"),
                ("tracklist/column-index", "activation"),
                ("tracklist/column-key", "activation"),
                ("tracklist/column-preset", "segmented"),
                ("tracklist/column-time", "activation"),
                ("tracklist/column-title", "activation"),
                ("tracklist/column-transition", "activation"),
                ("tracklist/reset-columns", "activation"),
                ("tracklist/table", "track-list"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_module_tabs_keep_their_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Modules,
            "module tabs",
            |path| path.starts_with("modules-tabs/"),
            &[
                ("modules-tabs/deck", "activation"),
                ("modules-tabs/deck-micro", "activation"),
                ("modules-tabs/global-bar", "activation"),
                ("modules-tabs/layout", "activation"),
                ("modules-tabs/telemetry", "activation"),
            ],
        );
    }

    #[kithara::test]
    fn the_hosted_nav_keeps_its_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Atoms,
            "nav",
            |path| path.starts_with("gallery/"),
            &[
                ("gallery/atoms/item", "activation"),
                ("gallery/buttons/item", "activation"),
                ("gallery/cells/item", "activation"),
                ("gallery/chrome/item", "activation"),
                ("gallery/faders/item", "activation"),
                ("gallery/library2/item", "activation"),
                ("gallery/menu/item", "activation"),
                ("gallery/micro/item", "activation"),
                ("gallery/mixer/item", "activation"),
                ("gallery/modules/item", "activation"),
                ("gallery/sizes/item", "activation"),
                ("gallery/stress/item", "activation"),
                ("gallery/titlebars/item", "activation"),
                ("gallery/tokens/item", "activation"),
                ("gallery/tracklist/item", "activation"),
                ("gallery/tree/item", "activation"),
                ("gallery/typography/item", "activation"),
                ("gallery/vis/item", "activation"),
            ],
        );
    }

    fn engine_descriptor_kinds(spec: &ControlSpec) -> &'static [&'static str] {
        match spec {
            ControlSpec::Button {
                icon: Some(IconName::PlayReverse),
                style,
                ..
            } if *style != ButtonStyle::MicroPrimary => &[],
            ControlSpec::NavItem {
                icon: IconName::PlayReverse,
                ..
            } => &[],
            ControlSpec::Button { .. }
            | ControlSpec::NavItem { .. }
            | ControlSpec::TabLarge { .. }
            | ControlSpec::Toggle
            | ControlSpec::Checkbox
            | ControlSpec::Chip { .. } => &["activation"],
            ControlSpec::ContextBar { .. } => &["picker"],
            ControlSpec::Crossfader { .. } => &["crossfader"],
            ControlSpec::Fader { .. } => &["fader"],
            ControlSpec::Knob { .. } => &["knob"],
            ControlSpec::Segmented { .. } => &["segmented"],
            ControlSpec::TrackList { .. } => &["track-list"],
            ControlSpec::VuStereo => &["stereo-meter"],
            ControlSpec::VuVertical { .. } => &["vertical-vu"],
            ControlSpec::Tree { .. } => &["scroll", "text-input"],
            ControlSpec::Wave {
                style: WaveStyle::Hero,
                ..
            } => &["hero-wave"],
            ControlSpec::Wave { .. } => &["wave"],
            _ => &[],
        }
    }

    fn assert_hosted_page_claims(
        tab: Tab,
        page: &str,
        belongs: impl Fn(&str) -> bool,
        expected: &[(&str, &str)],
    ) {
        let ui = compile(
            tab.entry(),
            &resolver(),
            &mock::registry(),
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the {tab:?} tab must compile: {error}"));
        let mut claims = Vec::new();
        each_control(&ui, &mut |path, spec| {
            if belongs(path) {
                for kind in engine_descriptor_kinds(spec) {
                    let path = if matches!(spec, ControlSpec::Tree { .. }) && *kind == "text-input"
                    {
                        format!("{path}/search")
                    } else {
                        path.to_owned()
                    };
                    claims.push((path, *kind));
                }
            }
        });
        claims.sort_unstable();
        let mut expected = expected
            .iter()
            .map(|(path, kind)| ((*path).to_owned(), *kind))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            claims, expected,
            "the hosted {page} page's engine claims changed; unported controls, passive controls, \
             and containers are intentionally absent"
        );
    }

    fn each_control(ui: &CompiledUi, visit: &mut impl FnMut(&str, &ControlSpec)) {
        fn walk(node: &ExpandedNode, ui: &CompiledUi, visit: &mut impl FnMut(&str, &ControlSpec)) {
            match node {
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. } => {
                    for child in children {
                        walk(child, ui, visit);
                    }
                }
                ExpandedNode::Optional { child, .. } | ExpandedNode::Pressable { child, .. } => {
                    walk(child, ui, visit);
                }
                ExpandedNode::Popover {
                    anchor, content, ..
                } => {
                    walk(anchor, ui, visit);
                    walk(content, ui, visit);
                }
                ExpandedNode::Control { path, spec, .. } => {
                    visit(ui.resolve(*path), spec);
                }
                _ => {}
            }
        }

        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|(_, child)| child));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Module { root, .. } => walk(root, ui, visit),
                _ => {}
            }
        }
    }

    #[kithara::test]
    fn every_nav_item_path_selects_its_tab() {
        let ui = compile(
            Tab::Atoms.entry(),
            &resolver(),
            &mock::registry(),
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut paths = Vec::new();
        collect_nav_item_paths(&ui.root, &ui, &mut paths);

        assert_eq!(paths.len(), Tab::ALL.len());
        let selected: Vec<_> = paths
            .iter()
            .map(|path| Tab::try_from(path.as_str()).unwrap_or_else(|()| panic!("{path}")))
            .collect();
        assert_eq!(selected, Tab::ALL);
    }

    fn collect_nav_item_paths(node: &CompiledNode, ui: &CompiledUi, paths: &mut Vec<String>) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_nav_item_paths(child, ui, paths);
                }
            }
            CompiledNode::Optional { child, .. } => collect_nav_item_paths(child, ui, paths),
            CompiledNode::Module { root, .. } => collect_expanded_nav_paths(root, ui, paths),
            _ => {}
        }
    }

    fn collect_expanded_nav_paths(node: &ExpandedNode, ui: &CompiledUi, paths: &mut Vec<String>) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_expanded_nav_paths(child, ui, paths);
                }
            }
            ExpandedNode::Control {
                path,
                spec: ControlSpec::NavItem { .. },
                ..
            } => paths.push(ui.resolve(*path).to_owned()),
            _ => {}
        }
    }

    #[kithara::test]
    fn module_demo_tabs_activate_their_compiled_control_paths() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let ui = compile(
            Tab::Modules.entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut paths = Vec::new();
        collect_tab_large_paths(&ui.root, &ui, &mut paths);

        assert_eq!(paths.len(), ModuleDemo::ALL.len());
        let mut reads = MockReads::default();
        for (path, module) in paths.iter().zip(ModuleDemo::ALL) {
            reads.apply(path, &ControlAction::Activate);
            assert_eq!(reads.active_module(), module, "{path}");
        }
    }

    #[kithara::test]
    fn menu_tab_carries_the_app_menu_and_one_popover_per_track() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let ui = compile(
            Tab::Menu.entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut found = MenuTab::default();
        collect_menu_tab(&ui.root, &ui, &mut found);

        assert_eq!(
            found.popovers,
            [
                ("app-menu/pop", "ui.menu.open"),
                ("ctx/track-1/menu", "gallery.menu.context@row=1"),
                ("ctx/track-2/menu", "gallery.menu.context@row=2"),
                ("ctx/track-3/menu", "gallery.menu.context@row=3"),
                ("ctx/track-4/menu", "gallery.menu.context@row=4"),
            ]
        );

        let track_one: Vec<_> = found
            .pressables
            .iter()
            .copied()
            .filter(|path| path.starts_with("ctx/track-1"))
            .collect();
        assert_eq!(
            track_one,
            [
                "ctx/track-1/row",
                "ctx/track-1/deck-a",
                "ctx/track-1/deck-b",
                "ctx/track-1/queue",
            ]
        );
        assert!(found.pressables.contains(&"app-menu/burger"));
    }

    #[kithara::test]
    fn the_mock_answers_every_read_the_menu_tab_names() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let ui = compile(
            Tab::Menu.entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut keys = Vec::new();
        collect_menu_reads(&ui.root, &ui, &mut keys);
        assert!(!keys.is_empty());

        let mut reads = MockReads::default();
        reads.apply("app-menu/new-window", &ControlAction::Activate);
        let unanswered: Vec<_> = keys
            .iter()
            .copied()
            .filter(|key| reads.get(key).is_none())
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    #[kithara::test]
    fn tree_query_binding_reaches_the_compiled_control() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let ui = compile(
            Tab::Tree.entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut queries = Vec::new();
        collect_tree_queries(&ui.root, &ui, &mut queries);

        assert_eq!(queries, ["library.query"]);
    }

    #[kithara::test]
    fn context_scope_binding_reaches_the_compiled_control() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let ui = compile(
            Tab::Library2.entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap();
        let mut contexts = Vec::new();
        collect_context_scopes(&ui.root, &ui, &mut contexts);

        assert_eq!(
            contexts,
            [("library2/context", "library.scope", "library.scope", 2)]
        );
    }

    fn collect_tab_large_paths(node: &CompiledNode, ui: &CompiledUi, paths: &mut Vec<String>) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_tab_large_paths(child, ui, paths);
                }
            }
            CompiledNode::Module { root, .. } => collect_expanded_tab_paths(root, ui, paths),
            _ => {}
        }
    }

    fn collect_expanded_tab_paths(node: &ExpandedNode, ui: &CompiledUi, paths: &mut Vec<String>) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_expanded_tab_paths(child, ui, paths);
                }
            }
            ExpandedNode::Control {
                path,
                spec: ControlSpec::TabLarge { .. },
                ..
            } => paths.push(ui.resolve(*path).to_owned()),
            _ => {}
        }
    }

    #[derive(Default)]
    struct MenuTab<'a> {
        popovers: Vec<(&'a str, &'a str)>,
        pressables: Vec<&'a str>,
    }

    fn collect_menu_tab<'a>(node: &'a CompiledNode, ui: &'a CompiledUi, found: &mut MenuTab<'a>) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_menu_tab(child, ui, found);
                }
            }
            CompiledNode::Optional { child, .. } => collect_menu_tab(child, ui, found),
            CompiledNode::Module { root, .. } => collect_menu_tab_module(root, ui, found),
            node => panic!("the menu walker does not know {node:?}"),
        }
    }

    fn collect_menu_tab_module<'a>(
        node: &'a ExpandedNode,
        ui: &'a CompiledUi,
        found: &mut MenuTab<'a>,
    ) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_menu_tab_module(child, ui, found);
                }
            }
            ExpandedNode::Optional { child, .. } => collect_menu_tab_module(child, ui, found),
            ExpandedNode::Popover {
                path,
                open,
                anchor,
                content,
                ..
            } => {
                found
                    .popovers
                    .push((ui.resolve(*path), ui.resolve(open.key)));
                collect_menu_tab_module(anchor, ui, found);
                collect_menu_tab_module(content, ui, found);
            }
            ExpandedNode::Pressable { path, child, .. } => {
                found.pressables.push(ui.resolve(*path));
                collect_menu_tab_module(child, ui, found);
            }
            ExpandedNode::Control { .. } => {}
            node => panic!("the menu walker does not know {node:?}"),
        }
    }

    fn collect_menu_reads<'a>(node: &'a CompiledNode, ui: &'a CompiledUi, keys: &mut Vec<&'a str>) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_menu_reads(child, ui, keys);
                }
            }
            CompiledNode::Optional { block, child } => {
                keys.push(ui.resolve(block.hidden.key));
                collect_menu_reads(child, ui, keys);
            }
            CompiledNode::Module { root, .. } => collect_menu_module_reads(root, ui, keys),
            node => panic!("the menu walker does not know {node:?}"),
        }
    }

    fn collect_menu_module_reads<'a>(
        node: &'a ExpandedNode,
        ui: &'a CompiledUi,
        keys: &mut Vec<&'a str>,
    ) {
        match node {
            ExpandedNode::Row {
                active, children, ..
            } => {
                if let Some(binding) = active {
                    keys.push(ui.resolve(binding.key));
                }
                for child in children {
                    collect_menu_module_reads(child, ui, keys);
                }
            }
            ExpandedNode::Column { children, .. } | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_menu_module_reads(child, ui, keys);
                }
            }
            ExpandedNode::Optional { block, child } => {
                keys.push(ui.resolve(block.hidden.key));
                collect_menu_module_reads(child, ui, keys);
            }
            ExpandedNode::Popover {
                open,
                anchor,
                content,
                ..
            } => {
                keys.push(ui.resolve(open.key));
                collect_menu_module_reads(anchor, ui, keys);
                collect_menu_module_reads(content, ui, keys);
            }
            ExpandedNode::Pressable { child, .. } => collect_menu_module_reads(child, ui, keys),
            ExpandedNode::Control { spec, read, .. } => {
                if let Some(binding) = read {
                    keys.push(ui.resolve(binding.key));
                }
                if let ControlSpec::Text {
                    active: Some(binding),
                    ..
                }
                | ControlSpec::Glyph {
                    active: Some(binding),
                    ..
                } = spec
                {
                    keys.push(ui.resolve(binding.key));
                }
            }
            node => panic!("the menu walker does not know {node:?}"),
        }
    }

    fn collect_tree_queries<'a>(
        node: &'a CompiledNode,
        ui: &'a CompiledUi,
        queries: &mut Vec<&'a str>,
    ) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_tree_queries(child, ui, queries);
                }
            }
            CompiledNode::Module { root, .. } => collect_expanded_tree_queries(root, ui, queries),
            _ => {}
        }
    }

    fn collect_expanded_tree_queries<'a>(
        node: &'a ExpandedNode,
        ui: &'a CompiledUi,
        queries: &mut Vec<&'a str>,
    ) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_expanded_tree_queries(child, ui, queries);
                }
            }
            ExpandedNode::Control {
                spec:
                    ControlSpec::Tree {
                        query:
                            Some(Binding {
                                kind: BindingKind::Model,
                                id,
                                ..
                            }),
                    },
                ..
            } => queries.push(ui.resolve(*id)),
            _ => {}
        }
    }

    fn collect_context_scopes<'a>(
        node: &'a CompiledNode,
        ui: &'a CompiledUi,
        contexts: &mut Vec<(&'a str, &'a str, &'a str, usize)>,
    ) {
        match node {
            CompiledNode::Split { children, .. } => {
                for (_, child) in children {
                    collect_context_scopes(child, ui, contexts);
                }
            }
            CompiledNode::Module { root, .. } => {
                collect_expanded_context_scopes(root, ui, contexts);
            }
            _ => {}
        }
    }

    fn collect_expanded_context_scopes<'a>(
        node: &'a ExpandedNode,
        ui: &'a CompiledUi,
        contexts: &mut Vec<(&'a str, &'a str, &'a str, usize)>,
    ) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    collect_expanded_context_scopes(child, ui, contexts);
                }
            }
            ExpandedNode::Control {
                path,
                spec:
                    ControlSpec::ContextBar {
                        scope_items,
                        scope:
                            Some(Binding {
                                kind: BindingKind::Model,
                                id: scope,
                                ..
                            }),
                    },
                write:
                    Some(Binding {
                        kind: BindingKind::Model,
                        id: write,
                        ..
                    }),
                ..
            } => contexts.push((
                ui.resolve(*path),
                ui.resolve(*scope),
                ui.resolve(*write),
                scope_items.len(),
            )),
            _ => {}
        }
    }
}
