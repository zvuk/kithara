mod mock;
mod shot;

use iced::{Element, Size, Subscription, Task, Theme, time as iced_time, window, window::Settings};
use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::{Skin, UiEvent, fonts, tree},
    source::{MemResolver, UiConfig},
};

use self::mock::{MockReads, ModuleDemo, Tab};

struct Consts;

impl Consts {
    const CAPTURE_TICK_MS: u64 = 100;
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
        "gallery-faders.klayout.ron",
        include_str!("assets/gallery-faders.klayout.ron"),
    ),
    (
        "gallery-modules.klayout.ron",
        include_str!("assets/gallery-modules.klayout.ron"),
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
        "gallery-modules-telemetry.klayout.ron",
        include_str!("assets/gallery-modules-telemetry.klayout.ron"),
    ),
    (
        "gallery-modules-layout.klayout.ron",
        include_str!("assets/gallery-modules-layout.klayout.ron"),
    ),
    (
        "gallery-typography.klayout.ron",
        include_str!("assets/gallery-typography.klayout.ron"),
    ),
    (
        "gallery-cells.klayout.ron",
        include_str!("assets/gallery-cells.klayout.ron"),
    ),
    (
        "gallery-sizes.klayout.ron",
        include_str!("assets/gallery-sizes.klayout.ron"),
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
        "gallery-library2.klayout.ron",
        include_str!("assets/gallery-library2.klayout.ron"),
    ),
    (
        "gallery-stress.klayout.ron",
        include_str!("assets/gallery-stress.klayout.ron"),
    ),
    (
        "modules/nav.kmodule.ron",
        include_str!("assets/modules/nav.kmodule.ron"),
    ),
    (
        "modules/module-tabs.kmodule.ron",
        include_str!("assets/modules/module-tabs.kmodule.ron"),
    ),
    (
        "modules/module-deck.kmodule.ron",
        include_str!("assets/modules/module-deck.kmodule.ron"),
    ),
    (
        "modules/module-deck-micro.kmodule.ron",
        include_str!("assets/modules/module-deck-micro.kmodule.ron"),
    ),
    (
        "modules/module-global-bar.kmodule.ron",
        include_str!("assets/modules/module-global-bar.kmodule.ron"),
    ),
    (
        "modules/module-telemetry.kmodule.ron",
        include_str!("assets/modules/module-telemetry.kmodule.ron"),
    ),
    (
        "modules/module-layout.kmodule.ron",
        include_str!("assets/modules/module-layout.kmodule.ron"),
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
        "modules/tabs/faders.kmodule.ron",
        include_str!("assets/modules/tabs/faders.kmodule.ron"),
    ),
    (
        "modules/tabs/typography.kmodule.ron",
        include_str!("assets/modules/tabs/typography.kmodule.ron"),
    ),
    (
        "modules/tabs/cells.kmodule.ron",
        include_str!("assets/modules/tabs/cells.kmodule.ron"),
    ),
    (
        "modules/tabs/sizes.kmodule.ron",
        include_str!("assets/modules/tabs/sizes.kmodule.ron"),
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
        "modules/tabs/library2.kmodule.ron",
        include_str!("assets/modules/tabs/library2.kmodule.ron"),
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
        "modules/primitives/toggles.kmodule.ron",
        include_str!("assets/modules/primitives/toggles.kmodule.ron"),
    ),
    (
        "modules/primitives/readouts.kmodule.ron",
        include_str!("assets/modules/primitives/readouts.kmodule.ron"),
    ),
    (
        "modules/primitives/chips.kmodule.ron",
        include_str!("assets/modules/primitives/chips.kmodule.ron"),
    ),
];

#[derive(Clone, Debug)]
enum Message {
    Close(window::Id),
    Shot(&'static str, window::Screenshot),
    Tick,
    Ui(UiEvent),
}

struct Gallery {
    layouts: [CompiledUi; Tab::ALL.len()],
    module_layouts: [CompiledUi; ModuleDemo::ALL.len()],
    skin: &'static Skin,
    reads: MockReads,
    shot: Option<shot::ShotPlan>,
    window_id: window::Id,
}

impl Gallery {
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
            exit_on_close_request: false,
            ..Settings::default()
        };
        let (window_id, open) = window::open(settings);
        (
            Self {
                layouts,
                module_layouts,
                skin: builtin::skin(),
                reads: MockReads::default(),
                shot: shot::ShotPlan::read(),
                window_id,
            },
            open.discard(),
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
}

fn main() -> iced::Result {
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
        Message::Shot(name, screenshot) => shot::save(state, name, &screenshot),
        Message::Tick => {
            state.reads.tick();
            shot::drive(state)
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
        Message::Ui(_) => Task::none(),
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
    let stress = state.reads.active_tab() == Tab::Stress;
    if state.shot.is_some() || stress {
        let period = if stress {
            Consts::STRESS_TICK_MS
        } else {
            Consts::CAPTURE_TICK_MS
        };
        Subscription::batch([
            close,
            iced_time::every(Duration::from_millis(period)).map(|_| Message::Tick),
        ])
    } else {
        close
    }
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
            background: palette.bg,
            text: palette.text,
            primary: palette.accent,
            success: palette.success,
            danger: palette.danger,
            warning: palette.warning,
        },
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::CompiledNode,
        expand::{Binding, ControlSpec, ExpandedNode},
        module::ChromeStyle,
        render::ControlAction,
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
                children: module_children,
                ..
            } = &children[1].1
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
            ExpandedNode::Control { .. } => {}
            _ => {}
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
                        query: Some(Binding::Model { id, .. }),
                    },
                ..
            } => queries.push(ui.resolve(*id)),
            ExpandedNode::Control { .. } => {}
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
                collect_expanded_context_scopes(root, ui, contexts)
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
                        scope: Some(Binding::Model { id: scope, .. }),
                    },
                write: Some(Binding::Model { id: write, .. }),
                ..
            } => contexts.push((
                ui.resolve(*path),
                ui.resolve(*scope),
                ui.resolve(*write),
                scope_items.len(),
            )),
            ExpandedNode::Control { .. } => {}
            _ => {}
        }
    }
}
