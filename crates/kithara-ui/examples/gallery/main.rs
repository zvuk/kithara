mod capture;
mod compare;
mod fixture;
#[cfg(feature = "masonry")]
mod host;
#[cfg(feature = "masonry")]
mod masonry_shots;
mod mock;
mod offscreen;
mod sections;
#[cfg(test)]
mod steady;
#[cfg(test)]
mod walk;

use iced::{Element, Size, Subscription, Task, Theme, time as iced_time, window, window::Settings};
use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::{Clock, Skin, UiEvent, WindowCommand, fonts, tree},
    source::{MemResolver, UiConfig},
};

use self::{
    capture::{Capture, Shot},
    fixture::{Consts, resolver},
    mock::MockReads,
    sections::{ModuleDemo, Tab},
};

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
    /// This host's own reading of time, advanced by the same step the tick
    /// subscription fires at, so a document bound to it moves with the page.
    clock: Clock,
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
            clock: Clock::default(),
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
                builtin::text_doc(),
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
                builtin::text_doc(),
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
                clock: Clock::default(),
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
    #[cfg(feature = "masonry")]
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
            state.clock = state
                .clock
                .advance(Duration::from_millis(Consts::STRESS_TICK_MS));
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
        state.clock,
    )
    .map(Message::Ui)
}

/// Time runs on the pages that say they move, which the document answers for
/// itself. Naming the pages here instead is a second account of the same fact,
/// and it drifts: a page that gained something moving kept its picture frozen
/// until an unrelated event redrew it, and one that lost it went on waking the
/// host every tick for nothing.
///
/// A capture never ticks: the offscreen host photographs one frame of a freshly
/// mounted page, so a clock running here would put the two hosts at different
/// moments and the comparison would measure the difference between them.
fn subscription(state: &Gallery) -> Subscription<Message> {
    let close = window::close_requests().map(Message::Close);
    if state.capture.is_none() && state.compiled().animates {
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
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_or_else(|error| panic!("embedded gallery document {entry} must compile: {error}"))
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
    use std::collections::BTreeSet;

    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::CompiledNode,
        expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
        lottie::builtin_artwork,
        module::{ButtonStyle, ChromeStyle, IconName, Motion, Pose, WaveStyle},
        registry::SECONDS,
        render::{ControlAction, ReadValue, Reads, builtin_sheet},
    };
    use num_traits::cast::AsPrimitive;

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
                builtin::text_doc(),
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
                builtin::text_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{} must compile: {error}", tab.entry()));
        }
    }

    /// The gallery is what proves a control draws the same picture in both
    /// hosts, so a control absent from every page is unproven no matter how
    /// complete the mount registry looks.
    #[kithara::test]
    fn every_control_appears_on_a_gallery_page() {
        let resolver = resolver();
        let endpoints = mock::registry();
        let entries = Tab::ALL
            .iter()
            .map(|tab| tab.entry())
            .chain(ModuleDemo::ALL.iter().map(|demo| demo.entry()));

        let mut drawn = BTreeSet::new();
        for entry in entries {
            let ui = compile(
                entry,
                &resolver,
                &endpoints,
                builtin::skin_doc(),
                builtin::text_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{entry} must compile: {error}"));
            each_control(&ui, &mut |_, spec| {
                drawn.insert(spec.kind());
            });
        }

        let absent: Vec<&str> = ControlSpec::KINDS
            .iter()
            .copied()
            .filter(|kind| !drawn.contains(kind))
            .collect();
        assert!(
            absent.is_empty(),
            "no gallery page names {absent:?}, so nothing compares them across the two hosts"
        );
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
    fn the_hosted_table_keeps_its_descriptor_backed_controls() {
        assert_hosted_page_claims(
            Tab::Table,
            "track-list",
            |path| path.starts_with("table/"),
            &[
                ("table/column-artist", "activation"),
                ("table/column-bpm", "activation"),
                ("table/column-deck", "activation"),
                ("table/column-energy", "activation"),
                ("table/column-index", "activation"),
                ("table/column-key", "activation"),
                ("table/column-preset", "segmented"),
                ("table/column-time", "activation"),
                ("table/column-title", "activation"),
                ("table/column-transition", "activation"),
                ("table/reset-columns", "activation"),
                ("table/table", "track-list"),
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
                ("gallery/clock/item", "activation"),
                ("gallery/faders/item", "activation"),
                ("gallery/library2/item", "activation"),
                ("gallery/lottie/item", "activation"),
                ("gallery/menu/item", "activation"),
                ("gallery/micro/item", "activation"),
                ("gallery/mixer/item", "activation"),
                ("gallery/modules/item", "activation"),
                ("gallery/motion/item", "activation"),
                ("gallery/objects/item", "activation"),
                ("gallery/pivot/item", "activation"),
                ("gallery/shader/item", "activation"),
                ("gallery/sprites/item", "activation"),
                ("gallery/sizes/item", "activation"),
                ("gallery/stress/item", "activation"),
                ("gallery/titlebars/item", "activation"),
                ("gallery/tokens/item", "activation"),
                ("gallery/table/item", "activation"),
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
            ControlSpec::Table { .. } => &["track-list"],
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
            builtin::text_doc(),
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

    /// Every control on a page, with the binding it reads from.
    fn each_control_read(ui: &CompiledUi, visit: &mut impl FnMut(&ControlSpec, Option<&Binding>)) {
        fn walk(node: &ExpandedNode, visit: &mut impl FnMut(&ControlSpec, Option<&Binding>)) {
            match node {
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. }
                | ExpandedNode::Stage { children, .. } => {
                    for child in children {
                        walk(child, visit);
                    }
                }
                ExpandedNode::Object { child, .. }
                | ExpandedNode::Optional { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Scroll { child, .. } => walk(child, visit),
                ExpandedNode::Popover {
                    anchor, content, ..
                } => {
                    walk(anchor, visit);
                    walk(content, visit);
                }
                ExpandedNode::Control { spec, read, .. } => visit(spec, read.as_ref()),
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
                CompiledNode::Module { root, .. } => walk(root, visit),
                _ => {}
            }
        }
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
                ExpandedNode::Object { child, .. }
                | ExpandedNode::Optional { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Scroll { child, .. } => {
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
            builtin::text_doc(),
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
            ExpandedNode::Scroll { child, .. } => collect_expanded_nav_paths(child, ui, paths),
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
            builtin::text_doc(),
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
            builtin::text_doc(),
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

    /// One object the motion page declares, with the track it travels along.
    struct Travel<'a> {
        pose: Pose,
        to: Option<Pose>,
        phase: Option<&'a str>,
        motion: Option<Motion<&'a str>>,
    }

    fn motion_objects(ui: &CompiledUi) -> Vec<Travel<'_>> {
        fn walk<'a>(node: &'a ExpandedNode, ui: &'a CompiledUi, found: &mut Vec<Travel<'a>>) {
            match node {
                ExpandedNode::Object {
                    pose,
                    to,
                    phase,
                    motion,
                    child,
                } => {
                    found.push(Travel {
                        pose: *pose,
                        to: *to,
                        phase: phase.as_ref().map(|binding| ui.resolve(binding.key)),
                        motion: motion
                            .as_ref()
                            .map(|motion| motion.with_clock(ui.resolve(motion.clock.key))),
                    });
                    walk(child, ui, found);
                }
                ExpandedNode::Optional { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Scroll { child, .. } => walk(child, ui, found),
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. }
                | ExpandedNode::Stage { children, .. } => {
                    for child in children {
                        walk(child, ui, found);
                    }
                }
                _ => {}
            }
        }

        let mut found = Vec::new();
        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|(_, child)| child));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Module { root, .. } => walk(root, ui, &mut found),
                _ => {}
            }
        }
        found
    }

    fn page(tab: Tab) -> CompiledUi {
        compile(
            tab.entry(),
            &resolver(),
            &mock::registry(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the {tab:?} page must compile: {error}"))
    }

    /// Poses, tracks and the stage that holds them.
    fn objects_page() -> CompiledUi {
        page(Tab::Objects)
    }

    /// The same journey a track makes, declared as a duration and a curve.
    fn motion_page() -> CompiledUi {
        page(Tab::Motion)
    }

    /// Every stage the page declares, as the number of children sharing its box.
    fn motion_stages(ui: &CompiledUi) -> Vec<usize> {
        fn walk(node: &ExpandedNode, found: &mut Vec<usize>) {
            match node {
                ExpandedNode::Stage { children, .. } => {
                    found.push(children.len());
                    for child in children {
                        walk(child, found);
                    }
                }
                ExpandedNode::Object { child, .. }
                | ExpandedNode::Optional { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Scroll { child, .. } => walk(child, found),
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. } => {
                    for child in children {
                        walk(child, found);
                    }
                }
                _ => {}
            }
        }

        let mut found = Vec::new();
        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|(_, child)| child));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Module { root, .. } => walk(root, &mut found),
                _ => {}
            }
        }
        found
    }

    /// A stage holding one child says nothing: one child fills its own box in
    /// any container. Overlap is the whole claim, so the page has to make it.
    #[kithara::test]
    fn the_objects_page_puts_several_children_in_one_box() {
        let ui = objects_page();

        let sharing = motion_stages(&ui);

        assert_eq!(sharing, vec![3]);
    }

    /// The page exists to show a control being moved, so a version of it with
    /// nothing that travels would capture cleanly and prove nothing.
    #[kithara::test]
    fn the_objects_page_declares_objects_that_travel() {
        let ui = objects_page();

        let travelling = motion_objects(&ui)
            .iter()
            .filter(|object| object.to.is_some())
            .count();

        assert!(travelling >= 4, "{travelling} object(s) travel");
    }

    #[kithara::test]
    fn the_mock_answers_the_phase_every_track_reads() {
        let ui = objects_page();
        let reads = MockReads::default();

        let unanswered: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| object.phase)
            .filter(|key| !matches!(reads.get(key), Some(ReadValue::Scalar(_))))
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    /// A capture never ticks, so both hosts are photographed at the phase the
    /// mock starts from. At either end of a track the object sits on one of its
    /// two written poses, and the picture would say nothing about the travel
    /// between them.
    #[kithara::test]
    fn every_track_is_off_its_written_pose_when_captured() {
        let ui = objects_page();
        let reads = MockReads::default();

        let still: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| Some((object, object.to.as_ref()?, object.phase?)))
            .filter_map(|(object, to, key)| {
                let ReadValue::Scalar(phase) = reads.get(key)? else {
                    return None;
                };
                (object.pose.between(to, phase.as_()) == object.pose).then_some(key)
            })
            .collect();

        assert_eq!(still, [""; 0]);
    }

    /// A motion is the other half of the page: an object whose document knows
    /// how long it takes and which way it turns, rather than being told where
    /// it is. Without one the page shows only the half that was already there.
    #[kithara::test]
    fn the_motion_page_declares_objects_that_move_off_a_clock() {
        let ui = motion_page();

        let running = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .count();

        assert!(running >= 4, "{running} object(s) run off a clock");
    }

    /// Clockwise and anticlockwise are one field with a sign, not two kinds of
    /// motion, and the page has to carry both for that to be worth saying.
    #[kithara::test]
    fn the_motion_page_turns_one_object_each_way() {
        let ui = motion_page();

        let turns: Vec<f32> = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .filter_map(|object| Some(object.to.as_ref()?.rotation))
            .filter(|rotation| *rotation != 0.0)
            .collect();

        assert!(
            turns.iter().any(|rotation| *rotation > 0.0)
                && turns.iter().any(|rotation| *rotation < 0.0),
            "turns are {turns:?}"
        );
    }

    /// Every sprite the sprite page declares: the sheet it names, how long one
    /// pass through it takes, and the endpoint it reads its seconds from.
    fn sprite_sites(ui: &CompiledUi) -> Vec<(&str, f32, Option<&str>)> {
        let mut found = Vec::new();
        each_control_read(ui, &mut |spec, read| {
            if let ControlSpec::Sprite { sheet, seconds } = spec {
                found.push((
                    ui.resolve(*sheet),
                    *seconds,
                    read.map(|binding| ui.resolve(binding.key)),
                ));
            }
        });
        found
    }

    /// A page that names a sheet nothing ships draws an empty row, and the
    /// capture beside it would agree with itself about nothing at all.
    #[kithara::test]
    fn every_sprite_names_a_sheet_the_toolkit_ships() {
        let ui = page(Tab::Sprites);

        let missing: Vec<&str> = sprite_sites(&ui)
            .iter()
            .map(|(sheet, _, _)| *sheet)
            .filter(|sheet| builtin_sheet(sheet).is_none())
            .collect();

        assert_eq!(missing, [""; 0]);
    }

    /// The row exists to show the sheet frame by frame, so its readings have to
    /// land on different frames: one second apart over a pass of eight, with
    /// eight frames cut, is one frame apart each.
    #[kithara::test]
    fn the_sheet_row_reads_one_second_per_frame() {
        let ui = page(Tab::Sprites);
        let reads = MockReads::default();

        let mut seconds: Vec<f64> = sprite_sites(&ui)
            .iter()
            .filter(|(_, pass, _)| *pass == 8.0)
            .filter_map(|(_, _, read)| match reads.get((*read)?)? {
                ReadValue::Scalar(seconds) => Some(seconds),
                _ => None,
            })
            .collect();
        seconds.sort_unstable_by(f64::total_cmp);
        seconds.dedup();

        assert_eq!(seconds, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    /// The played sprite reads the host's own clock, which no application
    /// declares and this mock does not answer: if the host did not answer it
    /// for itself, that sprite would hold its first frame for ever.
    #[kithara::test]
    fn the_played_sprite_reads_a_clock_the_application_does_not_own() {
        let ui = page(Tab::Sprites);
        let reads = MockReads::default();

        let host_clock: Vec<&str> = sprite_sites(&ui)
            .iter()
            .filter_map(|(_, _, read)| *read)
            .filter(|endpoint| reads.get(endpoint).is_none())
            .collect();

        assert!(
            host_clock.iter().all(|endpoint| *endpoint == SECONDS),
            "{host_clock:?} is read by a sprite and answered by nobody"
        );
    }

    #[kithara::test]
    fn the_page_plays_a_sprite_off_the_host_clock() {
        let ui = page(Tab::Sprites);

        let played = sprite_sites(&ui)
            .iter()
            .filter(|(_, _, read)| *read == Some(SECONDS))
            .count();

        assert!(played >= 1, "{played} sprite(s) run off the host's clock");
    }

    /// A sprite is a control like any other, so an object turns one. The claim
    /// is only worth making if the page actually poses one.
    #[kithara::test]
    fn the_page_poses_a_sprite_inside_a_moving_object() {
        let ui = page(Tab::Sprites);

        let posed = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .count();

        assert!(posed >= 2, "{posed} object(s) carry a sprite");
    }

    /// Every artwork the artwork page declares: the one it names, how long one
    /// pass through it takes, and the endpoint it reads its seconds from.
    fn artwork_sites(ui: &CompiledUi) -> Vec<(&str, f32, Option<&str>)> {
        let mut found = Vec::new();
        each_control_read(ui, &mut |spec, read| {
            if let ControlSpec::Lottie { artwork, seconds } = spec {
                found.push((
                    ui.resolve(*artwork),
                    *seconds,
                    read.map(|binding| ui.resolve(binding.key)),
                ));
            }
        });
        found
    }

    /// A page that names an artwork nothing ships draws an empty box, and the
    /// capture beside it would agree with itself about nothing at all.
    #[kithara::test]
    fn every_artwork_names_one_the_toolkit_ships() {
        let ui = page(Tab::Lottie);

        let missing: Vec<&str> = artwork_sites(&ui)
            .iter()
            .map(|(artwork, _, _)| *artwork)
            .filter(|artwork| builtin_artwork(artwork).is_none())
            .collect();

        assert_eq!(missing, [""; 0]);
    }

    /// The played artwork reads the host's own clock, which no application
    /// declares and this mock does not answer: if the host did not answer it
    /// for itself, that artwork would hold its first frame for ever.
    #[kithara::test]
    fn the_played_artwork_reads_a_clock_the_application_does_not_own() {
        let ui = page(Tab::Lottie);
        let reads = MockReads::default();

        let host_clock: Vec<&str> = artwork_sites(&ui)
            .iter()
            .filter_map(|(_, _, read)| *read)
            .filter(|endpoint| reads.get(endpoint).is_none())
            .collect();

        assert!(
            host_clock.iter().all(|endpoint| *endpoint == SECONDS),
            "{host_clock:?} is read by an artwork and answered by nobody"
        );
    }

    #[kithara::test]
    fn the_page_plays_an_artwork_off_the_host_clock() {
        let ui = page(Tab::Lottie);

        let played = artwork_sites(&ui)
            .iter()
            .filter(|(_, _, read)| *read == Some(SECONDS))
            .count();

        assert!(played >= 1, "{played} artwork(s) run off the host's clock");
    }

    /// An artwork is a control like any other, so an object turns one. The claim
    /// is only worth making if the page actually poses one.
    #[kithara::test]
    fn the_page_poses_an_artwork_inside_a_moving_object() {
        let ui = page(Tab::Lottie);

        let posed = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .count();

        assert!(posed >= 2, "{posed} object(s) carry an artwork");
    }

    /// The one fader a page carries, under the path the document gives it.
    fn only_fader_path(ui: &CompiledUi) -> String {
        let mut found = Vec::new();
        each_control(ui, &mut |path, spec| {
            if matches!(spec, ControlSpec::Fader { .. }) {
                found.push(path.to_owned());
            }
        });
        let [path] = <[String; 1]>::try_from(found)
            .unwrap_or_else(|found| panic!("the page must carry one fader, not {}", found.len()));
        path
    }

    fn scalar(reads: &MockReads, endpoint: &str) -> f64 {
        match reads.get(endpoint) {
            Some(ReadValue::Scalar(value)) => value,
            other => panic!("{endpoint} reads {other:?}"),
        }
    }

    /// A document builds a control's path from the module instance it is
    /// mounted under, so an application listening under another name hears
    /// nothing and the fader is a control the page only claims to have.
    #[kithara::test]
    fn the_scrub_fader_moves_the_artwork_beside_it() {
        let path = only_fader_path(&page(Tab::Lottie));
        let mut reads = MockReads::default();
        let before = scalar(&reads, "gallery.lottie.scrub");

        reads.apply(&path, &ControlAction::SetScalar(0.9));

        assert_ne!(scalar(&reads, "gallery.lottie.scrub"), before);
    }

    #[kithara::test]
    fn the_scrub_fader_moves_the_sprite_beside_it() {
        let path = only_fader_path(&page(Tab::Sprites));
        let mut reads = MockReads::default();
        let before = scalar(&reads, "gallery.sprite.scrub");

        reads.apply(&path, &ControlAction::SetScalar(0.9));

        assert_ne!(scalar(&reads, "gallery.sprite.scrub"), before);
    }

    #[kithara::test]
    fn the_mock_answers_the_clock_every_motion_reads() {
        let ui = motion_page();
        let reads = MockReads::default();

        let unanswered: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| object.motion.as_ref())
            .map(|motion| motion.clock)
            .filter(|key| !matches!(reads.get(key), Some(ReadValue::Scalar(_))))
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    /// A capture never ticks, so every motion is photographed at the one second
    /// the mock starts from. One still on its near pose would draw exactly what
    /// an object with no motion draws, and the page would prove nothing by it.
    /// Arriving is allowed and shown on purpose: that is what `Once` means.
    #[kithara::test]
    fn every_motion_has_left_its_near_pose_when_captured() {
        let ui = motion_page();
        let reads = MockReads::default();

        let unmoved: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| Some((object, object.to.as_ref()?, object.motion.as_ref()?)))
            .filter_map(|(object, to, motion)| {
                let ReadValue::Scalar(seconds) = reads.get(motion.clock)? else {
                    return None;
                };
                let here = object.pose.between(to, motion.phase_at(seconds.as_()));
                (here == object.pose).then_some(motion.clock)
            })
            .collect();

        assert_eq!(unmoved, [""; 0]);
    }

    /// The three repeats exist to be told apart, and they only are because the
    /// page runs them short enough that one and a half seconds lands each in a
    /// different place. Equal durations would draw one picture three times.
    #[kithara::test]
    fn the_three_repeats_stand_in_three_different_places_when_captured() {
        let ui = motion_page();
        let reads = MockReads::default();

        let mut places: Vec<f32> = motion_objects(&ui)
            .iter()
            .filter_map(|object| Some((object, object.to.as_ref()?, object.motion.as_ref()?)))
            .filter(|(_, _, motion)| motion.duration < 2.0)
            .filter_map(|(object, to, motion)| {
                let ReadValue::Scalar(seconds) = reads.get(motion.clock)? else {
                    return None;
                };
                Some(
                    object
                        .pose
                        .between(to, motion.phase_at(seconds.as_()))
                        .position
                        .0,
                )
            })
            .collect();
        places.sort_unstable_by(f32::total_cmp);
        places.dedup();

        assert_eq!(places.len(), 3, "the repeats stand at {places:?}");
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
            builtin::text_doc(),
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
            builtin::text_doc(),
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
            builtin::text_doc(),
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
            ExpandedNode::Optional { child, .. } | ExpandedNode::Scroll { child, .. } => {
                collect_menu_tab_module(child, ui, found);
            }
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
            ExpandedNode::Pressable { child, .. } | ExpandedNode::Scroll { child, .. } => {
                collect_menu_module_reads(child, ui, keys);
            }
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
