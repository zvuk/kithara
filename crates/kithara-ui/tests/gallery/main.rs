#![cfg(feature = "capture")]

//! The gallery's own suite: what checks the example rather than what runs it.
//!
//! The modules under test are taken by path rather than copied, so the pages,
//! the demo host and the harnesses examined here are the ones the gallery
//! itself opens with. The example beside this file is therefore only the
//! program, and every assertion about it lives in one place.
#[path = "../../examples/gallery/app.rs"]
mod app;
#[path = "../../examples/gallery/capture.rs"]
mod capture;
mod checks;
#[path = "../../examples/gallery/cli.rs"]
mod cli;
#[path = "../../examples/gallery/custom.rs"]
mod custom;
#[path = "../../examples/gallery/demo/mod.rs"]
mod demo;
#[path = "../../examples/gallery/fixture.rs"]
mod fixture;
#[cfg(feature = "masonry")]
#[path = "../../examples/gallery/host.rs"]
mod host;
#[path = "../../examples/gallery/sections.rs"]
mod sections;

use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    render::{Clock, Skin, UiEvent},
};

use self::{
    app::{Gallery, Message, update},
    capture::{Capture, Shot},
    demo::{DemoReads, reads::FONT_FAMILIES},
    fixture::{consts, resolver},
    sections::Page,
};

mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::CompiledNode,
        draw::Pt,
        expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
        lottie::builtin_artwork,
        module::{ButtonStyle, ChromeStyle, IconName, Motion, Pose, WaveStyle},
        registry::SECONDS,
        render::{ControlAction, ReadValue, Reads},
        source::SourceResolver,
        view::ViewWrite,
    };
    use num_traits::cast::AsPrimitive;

    use super::*;

    /// The screen the gallery draws is the file on disk, so editing it and
    /// opening the gallery again shows the edit rather than what this build
    /// happened to embed.
    #[kithara::test]
    fn the_screen_is_read_from_the_folder_it_ships_in() {
        let entry = sections::entry();
        let on_disk = std::fs::read_to_string(fixture::package_root().join(entry))
            .expect("the gallery ships the screen it names");
        let answered = resolver()
            .load(None, entry)
            .expect("the gallery resolver answers for the screen it names");

        assert_eq!(answered.text, on_disk, "{entry} must be answered from disk");
    }

    fn shot(tab: Page) -> Shot {
        Shot { tab, module: None }
    }

    fn seconds(reads: &dyn Reads, endpoint: &str) -> f64 {
        let Some(ReadValue::Scalar(value)) = reads.get(endpoint) else {
            panic!("{endpoint} answers a scalar")
        };
        value
    }

    /// A page is compiled against a skin, so a skin the gallery turns to is a
    /// set of pages built again rather than repainted. The neon skin gives the
    /// nav taller rows, which is a measurement the compiled page carries.
    #[kithara::test]
    fn turning_to_another_skin_compiles_the_pages_again() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("skins"));
        let dark = gallery.compiled().min;

        drop(update(
            &mut gallery,
            Message::Ui(UiEvent::Control {
                path: "skins/kithara-neon/item".to_owned(),
                action: ControlAction::Activate,
            }),
        ));

        assert_eq!(gallery.skin().id(), "kithara-neon");
        assert_ne!(gallery.compiled().min, dark);
    }

    /// The specimen switch is the runtime half of the face override: pressing a
    /// family lights its item and unfolds that family's block, and folds away
    /// the block of the family it left.
    #[kithara::test]
    fn pressing_a_face_family_unfolds_that_specimen_and_folds_the_others() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("assets"));

        drop(update(
            &mut gallery,
            Message::Ui(UiEvent::Control {
                path: "assets/mono/item".to_owned(),
                action: ControlAction::Activate,
            }),
        ));

        assert_eq!(
            gallery.reads.get("gallery.font.mono"),
            Some(ReadValue::Bool(true))
        );
        assert_eq!(
            gallery.reads.get("gallery.font.mono.hidden"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            gallery.reads.get("gallery.font.display"),
            Some(ReadValue::Bool(false))
        );
        assert_eq!(
            gallery.reads.get("gallery.font.display.hidden"),
            Some(ReadValue::Bool(true))
        );
    }

    #[kithara::test]
    fn a_tick_moves_the_clock_a_page_binds_to() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("motion"));
        gallery.tick();
        assert_ne!(gallery.clock, Clock::default());
    }

    #[kithara::test]
    fn a_tick_moves_the_reading_the_application_hands_over() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("motion"));
        gallery.tick();
        assert_ne!(
            seconds(&gallery.reads, "gallery.motion.clock"),
            seconds(&DemoReads::default(), "gallery.motion.clock")
        );
    }

    #[kithara::test]
    fn a_page_the_capture_turns_to_opens_at_nothing_on_the_clock() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("motion"));
        gallery.tick();
        gallery.select(shot("sprites"));
        assert_eq!(gallery.clock, Clock::default());
    }

    #[kithara::test]
    fn a_page_the_capture_turns_to_opens_with_nothing_behind_it() {
        let mut gallery = Gallery::mounted();
        gallery.select(shot("motion"));
        gallery.tick();
        gallery.select(shot("sprites"));
        assert_eq!(
            seconds(&gallery.reads, "gallery.motion.clock"),
            seconds(&DemoReads::default(), "gallery.motion.clock")
        );
    }

    #[kithara::test]
    fn every_module_demo_compiles_with_full_chrome() {
        for module in sections::modules().iter().copied() {
            let ui = module_page(module);
            let CompiledNode::Split { children, .. } = &ui.root else {
                panic!("expected gallery split");
            };
            let CompiledNode::Split {
                children: gallery_children,
                ..
            } = &children[1].node
            else {
                panic!("expected gallery content");
            };
            let CompiledNode::Split {
                children: module_children,
                ..
            } = &gallery_children[1].node
            else {
                panic!("expected module demo stack");
            };
            let CompiledNode::Module {
                title,
                chip,
                chrome,
                footer,
                ..
            } = &module_children[1].node
            else {
                panic!("expected module demo");
            };

            assert_eq!(*chrome, ChromeStyle::Full, "{module}");
            assert!(title.is_some(), "{module}");
            assert!(chip.is_some(), "{module}");
            assert!(footer.is_some(), "{module}");
        }
    }

    #[kithara::test]
    fn every_gallery_tab_compiles() {
        for tab in sections::pages().iter().copied() {
            drop(page(tab));
        }
    }

    /// The gallery is what proves a control draws the same picture in both
    /// hosts, so a control absent from every page is unproven no matter how
    /// complete the mount registry looks.
    #[kithara::test]
    fn every_control_appears_on_a_gallery_page() {
        let pages = sections::pages()
            .iter()
            .copied()
            .map(page)
            .chain(sections::modules().iter().copied().map(module_page));

        let mut drawn = BTreeSet::new();
        for ui in pages {
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
            "atoms",
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
            "atoms",
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
            "atoms",
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
            "atoms",
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
            "buttons",
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
            "faders",
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
            "tree",
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
            "library2",
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
            "table",
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
            "modules",
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
            "atoms",
            "nav",
            |path| path.starts_with("gallery/"),
            &[
                ("gallery/assets/item", "activation"),
                ("gallery/atoms/item", "activation"),
                ("gallery/buttons/item", "activation"),
                ("gallery/cells/item", "activation"),
                ("gallery/chrome/item", "activation"),
                ("gallery/clock/item", "activation"),
                ("gallery/custom/item", "activation"),
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
                ("gallery/scene/item", "activation"),
                ("gallery/shader/item", "activation"),
                ("gallery/sprites/item", "activation"),
                ("gallery/sizes/item", "activation"),
                ("gallery/skins/item", "activation"),
                ("gallery/stress/item", "activation"),
                ("gallery/titlebars/item", "activation"),
                ("gallery/tokens/item", "activation"),
                ("gallery/table/item", "activation"),
                ("gallery/table-long/item", "activation"),
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
        tab: Page,
        section: &str,
        belongs: impl Fn(&str) -> bool,
        expected: &[(&str, &str)],
    ) {
        let ui = page(tab);
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
            "the hosted {section} page's engine claims changed; unported controls, passive \
             controls, and containers are intentionally absent"
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
                | ExpandedNode::Placed { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Reveal { child, .. }
                | ExpandedNode::Scroll { child, .. } => walk(child, visit),
                ExpandedNode::Popover {
                    anchor, content, ..
                } => {
                    walk(anchor, visit);
                    walk(content, visit);
                }
                ExpandedNode::Adaptive { base, steps, .. } => {
                    walk(base, visit);
                    for (_, branch) in steps {
                        walk(branch, visit);
                    }
                }
                ExpandedNode::Control { spec, read, .. } => visit(spec, read.as_ref()),
                other => panic!("the control census does not walk {other:?}"),
            }
        }

        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|cell| &cell.node));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Adaptive { base, steps, .. } => {
                    stack.push(base);
                    stack.extend(steps.iter().map(|(_, branch)| branch));
                }
                CompiledNode::Module { root, .. } => walk(root, visit),
                other => panic!("the control census does not walk {other:?}"),
            }
        }
    }

    fn each_control(ui: &CompiledUi, visit: &mut impl FnMut(&str, &ControlSpec)) {
        fn walk(node: &ExpandedNode, ui: &CompiledUi, visit: &mut impl FnMut(&str, &ControlSpec)) {
            match node {
                ExpandedNode::Row { children, .. }
                | ExpandedNode::Column { children, .. }
                | ExpandedNode::Slot { children, .. }
                | ExpandedNode::Stage { children, .. } => {
                    for child in children {
                        walk(child, ui, visit);
                    }
                }
                ExpandedNode::Object { child, .. }
                | ExpandedNode::Optional { child, .. }
                | ExpandedNode::Placed { child, .. }
                | ExpandedNode::Pressable { child, .. }
                | ExpandedNode::Reveal { child, .. }
                | ExpandedNode::Scroll { child, .. } => {
                    walk(child, ui, visit);
                }
                ExpandedNode::Popover {
                    anchor, content, ..
                } => {
                    walk(anchor, ui, visit);
                    walk(content, ui, visit);
                }
                ExpandedNode::Adaptive { base, steps, .. } => {
                    walk(base, ui, visit);
                    for (_, branch) in steps {
                        walk(branch, ui, visit);
                    }
                }
                ExpandedNode::Control { path, spec, .. } => {
                    visit(ui.resolve(*path), spec);
                }
                other => panic!("the control census does not walk {other:?}"),
            }
        }

        let mut stack = vec![&ui.root];
        while let Some(node) = stack.pop() {
            match node {
                CompiledNode::Split { children, .. } => {
                    stack.extend(children.iter().map(|cell| &cell.node));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Adaptive { base, steps, .. } => {
                    stack.push(base);
                    stack.extend(steps.iter().map(|(_, branch)| branch));
                }
                CompiledNode::Module { root, .. } => walk(root, ui, visit),
                other => panic!("the control census does not walk {other:?}"),
            }
        }
    }

    /// The nav is what a reader presses to turn the screen, so every one of its
    /// rows must write the screen's own page state, and between them they must
    /// name every page that screen offers.
    #[kithara::test]
    fn every_nav_item_path_turns_the_screen_to_its_page() {
        let ui = page("atoms");
        let mut paths = Vec::new();
        collect_nav_item_paths(&ui.root, &ui, &mut paths);

        let mut turned = turned_to(&ui, sections::PAGE, &paths);
        turned.sort_unstable();

        let mut declared = sections::pages().to_vec();
        declared.sort_unstable();

        assert_eq!(
            turned, declared,
            "the nav offers every page the screen declares, once each"
        );
    }

    /// What each of `paths` writes into `state`, which the document says and
    /// the compiled screen carries.
    fn turned_to<'a>(ui: &'a CompiledUi, state: &str, paths: &[String]) -> Vec<&'a str> {
        paths
            .iter()
            .map(|path| match ui.views().at(path) {
                Some((wrote, ViewWrite::Page(page))) if wrote == state => page,
                other => panic!("{path} must turn {state}, and writes {other:?}"),
            })
            .collect()
    }

    /// The page offering a choice of skins and the skins the crate ships are
    /// two hand-written lists. A skin added to one and not the other is either
    /// a row nothing answers or a skin nobody can reach.
    #[kithara::test]
    fn the_skins_page_offers_every_shipped_skin() {
        let ui = page("skins");
        let mut paths = Vec::new();
        collect_nav_item_paths(&ui.root, &ui, &mut paths);

        let offered: Vec<&str> = paths
            .iter()
            .filter_map(|path| {
                path.strip_prefix("skins/")
                    .and_then(|rest| rest.strip_suffix("/item"))
            })
            .collect();
        let shipped: Vec<&str> = builtin::skins().iter().map(Skin::id).collect();
        assert_eq!(offered, shipped);
    }

    /// The specimen switch and the reading behind it are two hand-written
    /// lists. A family named in one and not the other is either a switch
    /// nothing answers or a face nobody can reach.
    #[kithara::test]
    fn the_assets_page_offers_every_face_family() {
        let ui = page("assets");
        let mut paths = Vec::new();
        collect_nav_item_paths(&ui.root, &ui, &mut paths);

        let offered: Vec<&str> = paths
            .iter()
            .filter_map(|path| {
                path.strip_prefix("assets/")
                    .and_then(|rest| rest.strip_suffix("/item"))
            })
            .collect();
        assert_eq!(offered, FONT_FAMILIES);
    }

    /// The page claims to show the whole icon vocabulary. An icon the toolkit
    /// draws and the page never names is a face a document may write with
    /// nothing to look at first.
    #[kithara::test]
    fn the_assets_page_shows_every_icon_the_toolkit_draws() {
        let ui = page("assets");
        let mut shown = Vec::new();
        each_control(&ui, &mut |path, spec| {
            if let ControlSpec::Glyph { icon, .. } = spec
                && path.starts_with("assets/icon-")
            {
                shown.push(*icon);
            }
        });

        assert_eq!(shown, IconName::ALL);
    }

    fn collect_nav_item_paths(node: &CompiledNode, ui: &CompiledUi, paths: &mut Vec<String>) {
        match node {
            CompiledNode::Split { children, .. } => {
                for cell in children {
                    let child = &cell.node;
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
    fn module_demo_tabs_turn_the_modules_page_to_their_demo() {
        let ui = page("modules");
        let mut paths = Vec::new();
        collect_tab_large_paths(&ui.root, &ui, &mut paths);

        let mut turned = turned_to(&ui, sections::MODULE, &paths);
        turned.sort_unstable();

        let mut declared = sections::modules().to_vec();
        declared.sort_unstable();

        assert_eq!(
            turned, declared,
            "the modules page offers every demo the screen declares, once each"
        );
    }

    #[kithara::test]
    fn menu_tab_carries_the_app_menu_and_one_popover_per_track() {
        let ui = page("menu");
        let mut found = MenuTab::default();
        collect_menu_tab(&ui.root, &ui, &mut found);

        assert_eq!(
            found.popovers,
            [
                ("app-menu/menu/pop", "app-menu/menu"),
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
        assert!(found.pressables.contains(&"app-menu/menu/burger"));
    }

    /// One object the motion page declares, with the track it travels along.
    struct Travel<'a> {
        motion: Option<Motion<&'a str>>,
        phase: Option<&'a str>,
        to: Option<Pose>,
        pose: Pose,
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
                | ExpandedNode::Placed { child, .. }
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
                    stack.extend(children.iter().map(|cell| &cell.node));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Module { root, .. } => walk(root, ui, &mut found),
                _ => {}
            }
        }
        found
    }

    /// The gallery's one screen, standing at the page named. Every page lives
    /// in that screen, so opening one is turning its state rather than
    /// compiling a document of its own.
    fn page(tab: Page) -> CompiledUi {
        standing(Shot { tab, module: None })
    }

    /// The modules page standing at one demo, which is the second state that
    /// screen turns.
    fn module_page(module: Page) -> CompiledUi {
        standing(Shot {
            tab: sections::MODULES,
            module: Some(module),
        })
    }

    fn standing(at: Shot) -> CompiledUi {
        compile(
            sections::entry(),
            &resolver(),
            &demo::registry(),
            builtin::skin_doc(),
            builtin::text_doc(),
            custom::config(),
            &at.standing(),
        )
        .unwrap_or_else(|error| panic!("the {at:?} page must compile: {error}"))
    }

    /// Poses, tracks and the stage that holds them.
    fn objects_page() -> CompiledUi {
        page("objects")
    }

    /// The same journey a track makes, declared as a duration and a curve.
    fn motion_page() -> CompiledUi {
        page("motion")
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
                    stack.extend(children.iter().map(|cell| &cell.node));
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
    fn the_demo_answers_the_phase_every_track_reads() {
        let ui = objects_page();
        let reads = DemoReads::default();

        let unanswered: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| object.phase)
            .filter(|key| !matches!(reads.get(key), Some(ReadValue::Scalar(_))))
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    /// A capture never ticks, so both hosts are photographed at the phase the
    /// demo starts from. At either end of a track the object sits on one of its
    /// two written poses, and the picture would say nothing about the travel
    /// between them.
    #[kithara::test]
    fn every_track_is_off_its_written_pose_when_captured() {
        let ui = objects_page();
        let reads = DemoReads::default();

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

    /// A page that names a picture the worn skin does not carry draws an empty
    /// row, and the capture beside it would agree with itself about nothing at
    /// all.
    #[kithara::test]
    fn every_sprite_names_a_picture_the_skin_carries() {
        let ui = page("sprites");

        let missing: Vec<&str> = sprite_sites(&ui)
            .iter()
            .map(|(sheet, _, _)| *sheet)
            .filter(|sheet| builtin::skin().sheet(sheet).is_none())
            .collect();

        assert_eq!(missing, [""; 0]);
    }

    /// The row exists to show the sheet frame by frame, so its readings have to
    /// land on different frames: one second apart over a pass of eight, with
    /// eight frames cut, is one frame apart each.
    #[kithara::test]
    fn the_sheet_row_reads_one_second_per_frame() {
        let ui = page("sprites");
        let reads = DemoReads::default();

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
    /// declares and this demo does not answer: if the host did not answer it
    /// for itself, that sprite would hold its first frame for ever.
    #[kithara::test]
    fn the_played_sprite_reads_a_clock_the_application_does_not_own() {
        let ui = page("sprites");
        let reads = DemoReads::default();

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
        let ui = page("sprites");

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
        let ui = page("sprites");

        let posed = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .count();

        assert!(posed >= 2, "{posed} object(s) carry a sprite");
    }

    /// Every artwork a page declares: the one it names, the one it switches to,
    /// how long one pass through it takes, and the endpoint it reads its
    /// seconds from.
    fn artwork_sites(ui: &CompiledUi) -> Vec<ArtworkSite<'_>> {
        let mut found = Vec::new();
        each_control_read(ui, &mut |spec, read| {
            if let ControlSpec::Lottie {
                artwork,
                active_artwork,
                ..
            } = spec
            {
                found.push(ArtworkSite {
                    artwork: ui.resolve(*artwork),
                    active: active_artwork.map(|name| ui.resolve(name)),
                    read: read.map(|binding| ui.resolve(binding.key)),
                });
            }
        });
        found
    }

    /// One Lottie site of a page, as the document declared it.
    struct ArtworkSite<'a> {
        artwork: &'a str,
        active: Option<&'a str>,
        read: Option<&'a str>,
    }

    /// A page that names an artwork nothing ships draws an empty box, and the
    /// capture beside it would agree with itself about nothing at all.
    #[kithara::test]
    fn every_artwork_names_one_the_toolkit_ships() {
        let ui = page("lottie");

        let missing: Vec<&str> = artwork_sites(&ui)
            .iter()
            .flat_map(|site| [Some(site.artwork), site.active])
            .flatten()
            .filter(|artwork| builtin_artwork(artwork).is_none())
            .collect();

        assert_eq!(missing, [""; 0]);
    }

    /// The played artwork reads the host's own clock, which no application
    /// declares and this demo does not answer: if the host did not answer it
    /// for itself, that artwork would hold its first frame for ever.
    #[kithara::test]
    fn the_played_artwork_reads_a_clock_the_application_does_not_own() {
        let ui = page("lottie");
        let reads = DemoReads::default();

        let host_clock: Vec<&str> = artwork_sites(&ui)
            .iter()
            .filter_map(|site| site.read)
            .filter(|endpoint| reads.get(endpoint).is_none())
            .collect();

        assert!(
            host_clock.iter().all(|endpoint| *endpoint == SECONDS),
            "{host_clock:?} is read by an artwork and answered by nobody"
        );
    }

    #[kithara::test]
    fn the_page_plays_an_artwork_off_the_host_clock() {
        let ui = page("lottie");

        let played = artwork_sites(&ui)
            .iter()
            .filter(|site| site.read == Some(SECONDS))
            .count();

        assert!(played >= 1, "{played} artwork(s) run off the host's clock");
    }

    /// An artwork is a control like any other, so an object turns one. The claim
    /// is only worth making if the page actually poses one.
    #[kithara::test]
    fn the_page_poses_an_artwork_inside_a_moving_object() {
        let ui = page("lottie");

        let posed = motion_objects(&ui)
            .iter()
            .filter(|object| object.motion.is_some())
            .count();

        assert!(posed >= 2, "{posed} object(s) carry an artwork");
    }

    /// Every placement the scene page declares, in document order: the path it
    /// publishes on, where it reads its point, where a drag publishes it, and
    /// what its magnet names.
    fn placements(ui: &CompiledUi) -> Vec<Placement<'_>> {
        fn walk<'a>(node: &'a ExpandedNode, ui: &'a CompiledUi, found: &mut Vec<Placement<'a>>) {
            match node {
                ExpandedNode::Placed {
                    path,
                    read,
                    write,
                    magnet,
                    child,
                    ..
                } => {
                    found.push(Placement {
                        path: ui.resolve(*path),
                        read: read.as_ref().map(|binding| ui.resolve(binding.key)),
                        write: write.as_ref().map(|binding| ui.resolve(binding.key)),
                        magnet: magnet.as_ref().map(|magnet| {
                            (
                                magnet.to.iter().map(|target| ui.resolve(*target)).collect(),
                                magnet.within,
                            )
                        }),
                    });
                    walk(child, ui, found);
                }
                ExpandedNode::Object { child, .. }
                | ExpandedNode::Optional { child, .. }
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
                    stack.extend(children.iter().map(|cell| &cell.node));
                }
                CompiledNode::Optional { child, .. } => stack.push(child),
                CompiledNode::Module { root, .. } => walk(root, ui, &mut found),
                _ => {}
            }
        }
        found
    }

    /// One placement of the scene page, as the document declared it.
    struct Placement<'a> {
        path: &'a str,
        magnet: Option<(Vec<&'a str>, f32)>,
        read: Option<&'a str>,
        write: Option<&'a str>,
    }

    /// A placement the pointer may carry has to read its point back, or the
    /// drag would publish somewhere the picture never follows.
    #[kithara::test]
    fn every_carried_placement_of_the_scene_reads_the_point_it_publishes() {
        let ui = page("scene");

        let unread: Vec<&str> = placements(&ui)
            .iter()
            .filter(|placement| placement.write.is_some() && placement.read.is_none())
            .map(|placement| placement.path)
            .collect();

        assert_eq!(unread, [""; 0]);
    }

    /// The point a drag publishes is the application's, so the demo has to
    /// answer every placement that reads one.
    #[kithara::test]
    fn the_demo_answers_the_point_every_placement_reads() {
        let ui = page("scene");
        let reads = DemoReads::default();

        let unanswered: Vec<&str> = placements(&ui)
            .iter()
            .filter_map(|placement| placement.read)
            .filter(|endpoint| !matches!(reads.get(endpoint), Some(ReadValue::Point(_))))
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    /// The magnet is the page's whole point: a carried placement names the
    /// others it snaps onto, whatever each of them holds.
    #[kithara::test]
    fn the_scene_magnets_name_placements_of_the_same_stage() {
        let ui = page("scene");
        let placed = placements(&ui);
        let names: Vec<&str> = placed
            .iter()
            .filter_map(|placement| placement.path.rsplit('/').next())
            .collect();

        let magnets: Vec<&str> = placed
            .iter()
            .filter_map(|placement| placement.magnet.as_ref())
            .flat_map(|(to, _)| to.iter().copied())
            .filter(|target| !names.contains(target))
            .collect();

        assert_eq!(magnets, [""; 0]);
    }

    #[kithara::test]
    fn the_scene_carries_more_than_one_placement() {
        let ui = page("scene");

        let carried = placements(&ui)
            .iter()
            .filter(|placement| placement.magnet.is_some())
            .count();

        assert!(carried >= 2, "{carried} placement(s) may be carried");
    }

    /// The press on the artwork switches which drawing stands, so the page has
    /// to name a second artwork and the toolkit has to ship it.
    #[kithara::test]
    fn the_scene_switches_between_two_artworks_the_toolkit_ships() {
        let ui = page("scene");

        let switched: Vec<(&str, &str)> = artwork_sites(&ui)
            .iter()
            .filter_map(|site| Some((site.artwork, site.active?)))
            .collect();

        assert!(!switched.is_empty(), "no artwork on the page switches");
        for (artwork, active) in switched {
            assert_ne!(artwork, active);
            assert!(
                builtin_artwork(artwork).is_some(),
                "{artwork} is not shipped"
            );
            assert!(builtin_artwork(active).is_some(), "{active} is not shipped");
        }
    }

    /// The flag the artwork switches on is the application's, and a press is
    /// what turns it.
    #[kithara::test]
    fn the_press_on_the_scene_artwork_turns_the_flag_it_switches_on() {
        let mut reads = DemoReads::default();

        assert_eq!(
            reads.get("gallery.scene.sparked"),
            Some(ReadValue::Bool(false))
        );
        reads.apply("scene/switch", &ControlAction::Activate);

        assert_eq!(
            reads.get("gallery.scene.sparked"),
            Some(ReadValue::Bool(true))
        );
    }

    /// A drag publishes where it left a placement, and the placement reads it
    /// back from the same endpoint.
    #[kithara::test]
    fn a_published_point_is_where_the_scene_placement_then_stands() {
        let mut reads = DemoReads::default();
        let at = Pt { x: 260.0, y: 150.0 };

        reads.apply("scene/carry-one", &ControlAction::Place(at));

        assert_eq!(reads.get("gallery.scene.one"), Some(ReadValue::Point(at)));
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

    fn scalar(reads: &DemoReads, endpoint: &str) -> f64 {
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
        let path = only_fader_path(&page("lottie"));
        let mut reads = DemoReads::default();
        let before = scalar(&reads, "gallery.lottie.scrub");

        reads.apply(&path, &ControlAction::SetScalar(0.9));

        assert_ne!(scalar(&reads, "gallery.lottie.scrub"), before);
    }

    #[kithara::test]
    fn the_scrub_fader_moves_the_sprite_beside_it() {
        let path = only_fader_path(&page("sprites"));
        let mut reads = DemoReads::default();
        let before = scalar(&reads, "gallery.sprite.scrub");

        reads.apply(&path, &ControlAction::SetScalar(0.9));

        assert_ne!(scalar(&reads, "gallery.sprite.scrub"), before);
    }

    #[kithara::test]
    fn the_demo_answers_the_clock_every_motion_reads() {
        let ui = motion_page();
        let reads = DemoReads::default();

        let unanswered: Vec<&str> = motion_objects(&ui)
            .iter()
            .filter_map(|object| object.motion.as_ref())
            .map(|motion| motion.clock)
            .filter(|key| !matches!(reads.get(key), Some(ReadValue::Scalar(_))))
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    /// A capture never ticks, so every motion is photographed at the one second
    /// the demo starts from. One still on its near pose would draw exactly what
    /// an object with no motion draws, and the page would prove nothing by it.
    /// Arriving is allowed and shown on purpose: that is what `Once` means.
    #[kithara::test]
    fn every_motion_has_left_its_near_pose_when_captured() {
        let ui = motion_page();
        let reads = DemoReads::default();

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
        let reads = DemoReads::default();

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
    fn the_demo_answers_every_read_the_menu_tab_names() {
        let ui = page("menu");
        let mut keys = Vec::new();
        collect_menu_reads(&ui.root, &ui, &mut keys);
        assert!(!keys.is_empty());

        let mut reads = DemoReads::default();
        reads.apply("app-menu/menu/new-window", &ControlAction::Activate);
        let unanswered: Vec<_> = keys
            .iter()
            .copied()
            .filter(|key| reads.get(key).is_none())
            .collect();

        assert_eq!(unanswered, [""; 0]);
    }

    #[kithara::test]
    fn tree_query_binding_reaches_the_compiled_control() {
        let ui = page("tree");
        let mut queries = Vec::new();
        collect_tree_queries(&ui.root, &ui, &mut queries);

        assert_eq!(queries, ["library.query"]);
    }

    #[kithara::test]
    fn context_scope_binding_reaches_the_compiled_control() {
        let ui = page("library2");
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
                for cell in children {
                    let child = &cell.node;
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
                for cell in children {
                    let child = &cell.node;
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
                for cell in children {
                    let child = &cell.node;
                    collect_menu_reads(child, ui, keys);
                }
            }
            CompiledNode::Optional { block, child } => {
                keys.extend(endpoint_key(ui, &block.hidden));
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
                    keys.extend(endpoint_key(ui, binding));
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
                keys.extend(endpoint_key(ui, &block.hidden));
                collect_menu_module_reads(child, ui, keys);
            }
            ExpandedNode::Popover {
                open,
                anchor,
                content,
                ..
            } => {
                keys.extend(endpoint_key(ui, open));
                collect_menu_module_reads(anchor, ui, keys);
                collect_menu_module_reads(content, ui, keys);
            }
            ExpandedNode::Pressable { child, .. } | ExpandedNode::Scroll { child, .. } => {
                collect_menu_module_reads(child, ui, keys);
            }
            ExpandedNode::Control { spec, read, .. } => {
                if let Some(binding) = read {
                    keys.extend(endpoint_key(ui, binding));
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
                    keys.extend(endpoint_key(ui, binding));
                }
            }
            node => panic!("the menu walker does not know {node:?}"),
        }
    }

    /// The endpoint one binding names, or nothing when it names state the page
    /// keeps for itself, which no application is asked to answer.
    /// The endpoint this binding names, or nothing when it names none: state
    /// the screen keeps for itself is answered by the screen rather than by the
    /// application, on either side.
    fn endpoint_key<'a>(ui: &'a CompiledUi, binding: &Binding) -> Option<&'a str> {
        (!matches!(
            binding.kind,
            BindingKind::View { .. } | BindingKind::Page { .. }
        ))
        .then(|| ui.resolve(binding.key))
    }

    fn collect_tree_queries<'a>(
        node: &'a CompiledNode,
        ui: &'a CompiledUi,
        queries: &mut Vec<&'a str>,
    ) {
        match node {
            CompiledNode::Split { children, .. } => {
                for cell in children {
                    let child = &cell.node;
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
                for cell in children {
                    let child = &cell.node;
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

    /// A capture through a window walks every page the gallery has, so the set
    /// it writes is the one the off-screen captures write.
    #[kithara::test]
    fn a_window_capture_walks_every_page() {
        assert_eq!(
            Capture::new(PathBuf::from("nowhere")).remaining(),
            Shot::all().len(),
        );
    }

    /// The gallery opens no smaller than the window it allows being dragged
    /// to. A minimum above the opening size would have the window grow the
    /// moment it appeared, and every still would be taken at a size the
    /// gallery never opens at.
    #[kithara::test]
    fn the_gallery_opens_no_smaller_than_the_window_it_allows() {
        assert!(
            consts::WIDTH >= consts::MIN_WIDTH,
            "the gallery opens at {} wide, below the {} it allows",
            consts::WIDTH,
            consts::MIN_WIDTH,
        );
        assert!(
            consts::HEIGHT >= consts::MIN_HEIGHT,
            "the gallery opens at {} tall, below the {} it allows",
            consts::HEIGHT,
            consts::MIN_HEIGHT,
        );
    }
}
