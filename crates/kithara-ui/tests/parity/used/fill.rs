use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::{CompiledUi, compile},
    draw::Pt,
    error::UiDocError,
    ids::{EndpointId, SourceUri},
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    package::{PackageDoc, load_package},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ReadValue, Reads, Scope, Skin, UiEvent},
    source::{FillDocument, Limits, MemResolver, OverlayResolver, SourceResolver, UiConfig},
    view,
};

use super::press::trigger;
use crate::immediate::Immediate;

const WINDOW: (u32, u32) = (400, 200);

const RACK: &str = r#"(schema: "kithara.module", version: 1, id: "rack", chrome: Plain,
    parameters: ["deck"],
    root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
        Slot(id: "items", from: "items", each: Include(source: "item.kmodule.ron"),
            size: Some((w: Fill, h: Fill)),
            default: [
                Pressable(id: "empty", press: Command(id: "fixture.empty", with: { "deck": "$deck" }),
                    child: Spacer(id: "empty-face", size: Some((w: Fill, h: Fill)))),
            ]),
    ]))"#;

const PICKER: &str = r#"(schema: "kithara.module", version: 1, id: "rack", chrome: Plain,
    parameters: ["deck"],
    root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
        Slot(id: "items", from: "items", select: Model(id: "fixture.page"),
            size: Some((w: Fill, h: Fill)),
            default: [
                Pressable(id: "empty", press: Command(id: "fixture.empty", with: { "deck": "$deck" }),
                    child: Spacer(id: "empty-face", size: Some((w: Fill, h: Fill)))),
            ]),
    ]))"#;

const BOTH: &str = r#"(schema: "kithara.module", version: 1, id: "rack", chrome: Plain,
    parameters: ["deck"],
    root: Slot(id: "items", from: "items", each: Include(source: "item.kmodule.ron"),
        select: Model(id: "fixture.page")))"#;

fn boxed(shows: &str) -> String {
    format!(
        r#"(schema: "kithara.module", version: 1, id: "rack", chrome: Plain,
            parameters: ["deck"],
            root: Slot(id: "items", from: "items", {shows},
                size: Some((w: Fill, h: Fixed(451.0)))))"#
    )
}

fn tall(height: f32) -> String {
    format!(
        r#"(schema: "kithara.module", version: 1, id: "tall", chrome: Plain,
            parameters: ["deck", "key", "source"],
            root: Spacer(id: "tall-face", size: Some((w: Fill, h: Fixed({height:.1})))))"#
    )
}

const ITEM: &str = r#"(schema: "kithara.module", version: 1, id: "item", chrome: Plain,
    parameters: ["deck", "key", "source"],
    root: Optional(id: "frame", hidden: Model(id: "fixture.hidden", with: { "source": "$key" }),
        child: Slot(id: "content", size: Some((w: Fill, h: Fill)))))"#;

const BARE_ITEM: &str = r#"(schema: "kithara.module", version: 1, id: "item", chrome: Plain,
    parameters: ["deck", "key", "source"],
    root: Spacer(id: "frame", size: Some((w: Fill, h: Fill))))"#;

fn filling(endpoint: &str) -> String {
    format!(
        r#"(schema: "kithara.module", version: 1, id: "press-page", chrome: Plain,
            parameters: ["deck", "key", "source"],
            root: Pressable(id: "press",
                press: Command(id: "{endpoint}", with: {{ "deck": "$deck", "source": "$source" }}),
                child: Spacer(id: "press-face", size: Some((w: Fill, h: Fill)))))"#
    )
}

const SOURCED: &str = r#"(schema: "kithara.module", version: 1, id: "sourced-page", chrome: Plain,
    parameters: ["source"],
    root: Pressable(id: "press", press: Command(id: "fixture.sourced", with: { "source": "$source" }),
        child: Spacer(id: "press-face", size: Some((w: Fill, h: Fill)))))"#;

const KEYED_ITEM: &str = r#"(schema: "kithara.module", version: 1, id: "item", chrome: Plain,
    parameters: ["key"],
    root: Optional(id: "frame", hidden: Model(id: "fixture.hidden", with: { "source": "$key" }),
        child: Slot(id: "content", size: Some((w: Fill, h: Fill)))))"#;

const KEYED: &str = r#"(schema: "kithara.module", version: 1, id: "keyed-page", chrome: Plain,
    parameters: ["key"],
    root: Pressable(id: "press", press: Command(id: "fixture.sourced", with: { "source": "$key" }),
        child: Spacer(id: "press-face", size: Some((w: Fill, h: Fill)))))"#;

const ONE: &str = r#"(schema: "kithara.layout", version: 1, id: "page",
    root: Module(instance: "demo", source: "rack.kmodule.ron", with: { "deck": "a" },
        size: (w: Fill, h: Fill)))"#;

const TWO: &str = r#"(schema: "kithara.layout", version: 1, id: "page",
    root: Split(axis: Horizontal, children: [
        (weight: 1.0, node: Module(instance: "left", source: "rack.kmodule.ron",
            with: { "deck": "a" }, size: (w: Fill, h: Fill))),
        (weight: 1.0, node: Module(instance: "right", source: "rack.kmodule.ron",
            with: { "deck": "b" }, size: (w: Fill, h: Fill))),
    ]))"#;

fn documents(layout: &str, item: &str, fills: &[(&str, &str)]) -> MemResolver {
    documents_in(RACK, layout, item, fills)
}

fn documents_in(rack: &str, layout: &str, item: &str, fills: &[(&str, &str)]) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert("page.klayout.ron", layout);
    resolver.insert("rack.kmodule.ron", rack);
    resolver.insert("item.kmodule.ron", item);
    for (key, endpoint) in fills {
        let origin = SourceUri(format!("{key}.kmodule.ron"));
        let document = FillDocument::parse(&filling(endpoint), origin)
            .unwrap_or_else(|error| panic!("the fill must parse: {error}"));
        resolver.fill("rack/items", key, document);
    }
    resolver
}

#[derive(Default)]
struct Page {
    selected: Option<&'static str>,
    published: Vec<UiEvent>,
}

impl Page {
    fn selecting(selected: Option<&'static str>) -> Self {
        Self {
            selected,
            ..Self::default()
        }
    }
}

impl Reads for Page {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (base, scope) = Scope::split(endpoint);
        match base {
            "fixture.hidden" => {
                Some(ReadValue::Bool(self.selected.is_some_and(|selected| {
                    scope.get("source") != Some(selected)
                })))
            }
            "fixture.page" => self.selected.map(ReadValue::Text),
            _ => None,
        }
    }
}

impl App for Page {
    fn document(&self) -> &str {
        "page.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, event: UiEvent) {
        self.published.push(event);
    }
}

struct Endpoints {
    flag: EndpointDesc,
    page: EndpointDesc,
    press: EndpointDesc,
    sourced: EndpointDesc,
    empty: EndpointDesc,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            flag: EndpointDesc::new(ValueKind::Bool).with_scope("source"),
            page: EndpointDesc::new(ValueKind::Text),
            press: EndpointDesc::new(ValueKind::Trigger)
                .with_scope("deck")
                .with_scope("source"),
            sourced: EndpointDesc::new(ValueKind::Trigger).with_scope("source"),
            empty: EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        }
    }
}

impl EndpointRegistry for Endpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        match (category, id.0.as_str()) {
            (EndpointCategory::Model, "fixture.hidden") => Some(&self.flag),
            (EndpointCategory::Model, "fixture.page") => Some(&self.page),
            (EndpointCategory::Command, "fixture.press") => Some(&self.press),
            (EndpointCategory::Command, "fixture.sourced") => Some(&self.sourced),
            (EndpointCategory::Command, "fixture.empty") => Some(&self.empty),
            _ => None,
        }
    }
}

fn compiled(resolver: &dyn SourceResolver) -> Result<CompiledUi, UiDocError> {
    compile(
        "page.klayout.ron",
        resolver,
        &Endpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
}

fn packaged(mut resolver: MemResolver) -> Result<PackageDoc, UiDocError> {
    resolver.insert(
        "package.kpackage.ron",
        r#"(schema: "kithara.package", version: 1, id: "filled", contract: 1,
            screens: { "page": "page.klayout.ron" })"#,
    );
    load_package(&resolver, "package.kpackage.ron", &Limits::default())
}

fn retained<'a>(
    resolver: &'a dyn SourceResolver,
    endpoints: &'a Endpoints,
    app: Page,
) -> Result<Ui<'a, Page>, String> {
    Ui::new(
        app,
        Config::builder()
            .endpoints(endpoints)
            .resolver(resolver)
            .text(builtin::text_doc())
            .build(),
        WINDOW,
        1.0,
    )
    .map_err(|error| error.to_string())
}

fn clicks(
    resolver: &dyn SourceResolver,
    selected: Option<&'static str>,
    at: &[Pt],
) -> [Vec<UiEvent>; 2] {
    let endpoints = Endpoints::default();
    let mut ui = retained(resolver, &endpoints, Page::selecting(selected))
        .unwrap_or_else(|error| panic!("the page must mount on the retained host: {error}"));
    for point in at {
        for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
            ui.input(Input::Pointer(PointerInput::new(
                MOUSE,
                None,
                phase,
                Some(*point),
                1,
            )));
        }
    }
    let retained = ui.app().published.clone();
    let ui = compiled(resolver).unwrap_or_else(|error| panic!("the page must compile: {error}"));
    let mut host = Immediate::mount(Page::selecting(selected), &ui, builtin::skin(), WINDOW);
    for point in at {
        host.click_at(*point);
    }
    [retained, host.app().published.clone()]
}

fn assert_both(
    resolver: &dyn SourceResolver,
    selected: Option<&'static str>,
    at: &[Pt],
    expected: &[UiEvent],
    what: &str,
) {
    let [retained, immediate] = clicks(resolver, selected, at);
    assert_eq!(retained, expected, "the retained host: {what}");
    assert_eq!(immediate, expected, "the immediate host: {what}");
}

const CENTRE: Pt = Pt { x: 200.0, y: 100.0 };
const LEFT: Pt = Pt { x: 100.0, y: 100.0 };
const RIGHT: Pt = Pt { x: 300.0, y: 100.0 };
const TOP: Pt = Pt { x: 200.0, y: 50.0 };
const BOTTOM: Pt = Pt { x: 200.0, y: 150.0 };

#[kithara::test]
fn each_fill_is_drawn_in_its_template_under_its_key() {
    let resolver = documents(
        ONE,
        ITEM,
        &[("alpha", "fixture.press"), ("beta", "fixture.press")],
    );

    assert_both(
        &resolver,
        Some("alpha"),
        &[CENTRE],
        &[trigger("fixture.press@deck=a,source=alpha")],
        "the selected first fill takes the press",
    );
    assert_both(
        &resolver,
        Some("beta"),
        &[CENTRE],
        &[trigger("fixture.press@deck=a,source=beta")],
        "the selected second fill takes the press",
    );
}

#[kithara::test]
fn fills_are_drawn_in_the_order_given() {
    let forward = documents(
        ONE,
        ITEM,
        &[("alpha", "fixture.press"), ("beta", "fixture.press")],
    );
    let backward = documents(
        ONE,
        ITEM,
        &[("beta", "fixture.press"), ("alpha", "fixture.press")],
    );

    assert_both(
        &forward,
        None,
        &[TOP, BOTTOM],
        &[
            trigger("fixture.press@deck=a,source=alpha"),
            trigger("fixture.press@deck=a,source=beta"),
        ],
        "the fill given first stands first",
    );
    assert_both(
        &backward,
        None,
        &[TOP, BOTTOM],
        &[
            trigger("fixture.press@deck=a,source=beta"),
            trigger("fixture.press@deck=a,source=alpha"),
        ],
        "the fill given first stands first",
    );
}

#[kithara::test]
fn a_fill_is_laid_out_under_the_slot_and_its_key() {
    let resolver = documents(ONE, ITEM, &[("alpha", "fixture.press")]);
    let endpoints = Endpoints::default();
    let ui = retained(&resolver, &endpoints, Page::selecting(Some("alpha")))
        .unwrap_or_else(|error| panic!("the page must mount on the retained host: {error}"));

    assert!(ui.rect_of("demo/items/alpha/press-face").is_some());
}

#[kithara::test]
fn a_collection_with_no_fills_draws_the_default() {
    let resolver = documents(ONE, ITEM, &[]);

    assert_both(
        &resolver,
        None,
        &[CENTRE],
        &[trigger("fixture.empty@deck=a")],
        "the default stands while nothing fills the slot",
    );
}

#[kithara::test]
fn a_module_included_twice_draws_its_fill_per_instance() {
    let resolver = documents(TWO, ITEM, &[("alpha", "fixture.press")]);

    assert_both(
        &resolver,
        Some("alpha"),
        &[LEFT, RIGHT],
        &[
            trigger("fixture.press@deck=a,source=alpha"),
            trigger("fixture.press@deck=b,source=alpha"),
        ],
        "each instance draws the fill with its own deck",
    );
}

#[kithara::test]
fn a_template_without_content_fails_loading() {
    let resolver = documents(ONE, BARE_ITEM, &[("alpha", "fixture.press")]);
    let endpoints = Endpoints::default();

    let retained = retained(&resolver, &endpoints, Page::default()).err();
    let immediate = compiled(&resolver).err();

    for (host, error) in [
        ("retained", retained),
        ("immediate", immediate.map(|error| error.to_string())),
    ] {
        let error = error.unwrap_or_else(|| panic!("the {host} host must refuse the template"));
        assert!(
            error.contains("item.kmodule.ron") && error.contains("content"),
            "the {host} host names the template: {error}"
        );
    }
    assert!(matches!(
        compiled(&resolver),
        Err(UiDocError::TemplateWithoutContent { origin }) if origin.0 == "item.kmodule.ron"
    ));
}

#[kithara::test]
fn an_unknown_endpoint_in_a_fill_fails_loading() {
    let resolver = documents(ONE, ITEM, &[("alpha", "fixture.nowhere")]);
    let endpoints = Endpoints::default();

    let retained = retained(&resolver, &endpoints, Page::default()).err();
    let immediate = compiled(&resolver).err().map(|error| error.to_string());

    for (host, error) in [("retained", retained), ("immediate", immediate)] {
        let error = error.unwrap_or_else(|| panic!("the {host} host must refuse the fill"));
        assert!(
            error.contains("fixture.nowhere"),
            "the {host} host names the endpoint: {error}"
        );
    }
}

#[kithara::test]
fn a_selection_draws_the_fill_whose_key_it_reads() {
    let resolver = documents_in(
        PICKER,
        ONE,
        ITEM,
        &[("alpha", "fixture.press"), ("beta", "fixture.press")],
    );

    assert_both(
        &resolver,
        Some("alpha"),
        &[CENTRE],
        &[trigger("fixture.press@deck=a,source=alpha")],
        "the fill keyed alpha stands alone",
    );
    assert_both(
        &resolver,
        Some("beta"),
        &[CENTRE],
        &[trigger("fixture.press@deck=a,source=beta")],
        "the fill keyed beta stands alone",
    );

    let endpoints = Endpoints::default();
    let ui = retained(&resolver, &endpoints, Page::selecting(Some("beta")))
        .unwrap_or_else(|error| panic!("the page must mount on the retained host: {error}"));
    assert!(ui.rect_of("demo/items/beta/press-face").is_some());
}

#[kithara::test]
fn a_selection_of_no_fill_draws_the_default() {
    let resolver = documents_in(
        PICKER,
        ONE,
        ITEM,
        &[("alpha", "fixture.press"), ("beta", "fixture.press")],
    );

    for selected in [Some("gamma"), None] {
        assert_both(
            &resolver,
            selected,
            &[CENTRE],
            &[trigger("fixture.empty@deck=a")],
            "the default stands while no fill has the key read",
        );
    }
}

#[kithara::test]
fn a_selection_needs_the_room_of_its_largest_fill() {
    let room = |shows: &str, heights: &[f32]| {
        let mut resolver = MemResolver::default();
        resolver.insert("page.klayout.ron", ONE);
        resolver.insert("rack.kmodule.ron", &boxed(shows));
        resolver.insert("item.kmodule.ron", ITEM);
        for (index, height) in heights.iter().enumerate() {
            let key = format!("fill{index}");
            let origin = SourceUri(format!("{key}.kmodule.ron"));
            let document = FillDocument::parse(&tall(*height), origin)
                .unwrap_or_else(|error| panic!("the fill must parse: {error}"));
            resolver.fill("rack/items", &key, document);
        }
        resolver
    };
    let endpoints = Endpoints::default();
    let select = r#"select: Model(id: "fixture.page")"#;
    let each = r#"each: Include(source: "item.kmodule.ron")"#;

    let fitting = room(select, &[300.0, 300.0]);
    assert!(
        compiled(&fitting).is_ok(),
        "the immediate host loads fills that each fit"
    );
    assert!(
        retained(&fitting, &endpoints, Page::default()).is_ok(),
        "the retained host loads fills that each fit"
    );

    let oversized = room(select, &[300.0, 500.0]);
    assert!(matches!(
        compiled(&oversized),
        Err(UiDocError::DeclaredRoom { .. })
    ));
    assert!(retained(&oversized, &endpoints, Page::default()).is_err());

    let listed = room(each, &[300.0, 300.0]);
    assert!(matches!(
        compiled(&listed),
        Err(UiDocError::DeclaredRoom { .. })
    ));
    assert!(retained(&listed, &endpoints, Page::default()).is_err());
}

#[kithara::test]
fn a_slot_with_both_each_and_select_fails_loading() {
    let resolver = documents_in(BOTH, ONE, ITEM, &[("alpha", "fixture.press")]);
    let endpoints = Endpoints::default();

    let retained = retained(&resolver, &endpoints, Page::default()).err();
    let immediate = compiled(&resolver).err().map(|error| error.to_string());

    for (host, error) in [("retained", retained), ("immediate", immediate)] {
        let error = error.unwrap_or_else(|| panic!("the {host} host must refuse the slot"));
        assert!(
            error.contains("rack.kmodule.ron") && error.contains("items"),
            "the {host} host names the slot: {error}"
        );
    }
    assert!(matches!(
        compiled(&resolver),
        Err(UiDocError::CollectionShape { origin, slot })
            if origin.0 == "rack.kmodule.ron" && slot == "items"
    ));
}

#[kithara::test]
fn a_fill_named_by_path_is_read_through_the_package() {
    let path = "pages/shared.kmodule.ron";
    let mut lower = documents_in(PICKER, ONE, ITEM, &[]);
    lower.insert(path, &filling("fixture.press"));
    lower.fill("rack/items", "alpha", FillDocument::Path(path.to_owned()));

    assert_both(
        &lower,
        Some("alpha"),
        &[CENTRE],
        &[trigger("fixture.press@deck=a,source=alpha")],
        "the package's own document is the fill",
    );

    let mut upper = MemResolver::default();
    upper.insert(
        path,
        r#"(schema: "kithara.module", version: 1, id: "press-page", chrome: Plain,
            parameters: ["deck", "key", "source"],
            root: Pressable(id: "press", press: Command(id: "fixture.empty", with: { "deck": "$deck" }),
                child: Spacer(id: "press-face", size: Some((w: Fill, h: Fill)))))"#,
    );
    let overlaid = OverlayResolver::new(upper, lower);

    assert_both(
        &overlaid,
        Some("alpha"),
        &[CENTRE],
        &[trigger("fixture.empty@deck=a")],
        "an overlay replaces the document the fill names",
    );
}

#[kithara::test]
fn a_fill_receives_only_the_parameters_it_declares() {
    for rack in [RACK, PICKER] {
        let mut resolver = documents_in(rack, ONE, ITEM, &[]);
        let origin = SourceUri("sourced.kmodule.ron".to_owned());
        let document = FillDocument::parse(SOURCED, origin)
            .unwrap_or_else(|error| panic!("the fill must parse: {error}"));
        resolver.fill("rack/items", "alpha", document);

        assert_both(
            &resolver,
            Some("alpha"),
            &[CENTRE],
            &[trigger("fixture.sourced@source=alpha")],
            "a fill declaring only `source` stands in an instance with `deck`",
        );
    }
}

#[kithara::test]
fn a_template_receives_only_the_parameters_it_declares() {
    let mut resolver = documents(ONE, KEYED_ITEM, &[]);
    let origin = SourceUri("keyed.kmodule.ron".to_owned());
    let document = FillDocument::parse(KEYED, origin)
        .unwrap_or_else(|error| panic!("the fill must parse: {error}"));
    resolver.fill("rack/items", "alpha", document);

    assert_both(
        &resolver,
        Some("alpha"),
        &[CENTRE],
        &[trigger("fixture.sourced@source=alpha")],
        "a template declaring only `key` stands in an instance with `deck`",
    );
}

#[kithara::test]
fn a_fill_key_taken_twice_or_not_a_path_segment_fails_loading() {
    let cases: [(&[&str], &str); 4] = [
        (&["alpha", "alpha"], "alpha"),
        (&["a/b"], "a/b"),
        (&[""], ""),
        (&["$alpha"], "$alpha"),
    ];
    let endpoints = Endpoints::default();
    for (keys, refused) in cases {
        let fills: Vec<(&str, &str)> = keys.iter().map(|key| (*key, "fixture.press")).collect();
        let resolver = documents(ONE, ITEM, &fills);

        let immediate = compiled(&resolver)
            .err()
            .unwrap_or_else(|| panic!("the immediate host must refuse the key {refused:?}"));
        let retained = retained(&resolver, &endpoints, Page::default())
            .err()
            .unwrap_or_else(|| panic!("the retained host must refuse the key {refused:?}"));
        assert_eq!(
            retained,
            immediate.to_string(),
            "both hosts refuse the key {refused:?} alike"
        );
        assert!(
            matches!(
                immediate,
                UiDocError::FillKey { ref address, ref key, .. }
                    if address == "rack/items" && key == refused
            ),
            "the immediate host refuses the key {refused:?}"
        );
        assert!(
            matches!(
                packaged(resolver),
                Err(UiDocError::FillKey { address, key, .. })
                    if address == "rack/items" && key == refused
            ),
            "loading the package refuses the key {refused:?}"
        );
    }
}

#[kithara::test]
fn a_fill_key_that_cannot_stand_as_a_scope_value_fails_loading() {
    let endpoints = Endpoints::default();
    for refused in ["a,b", "a=b"] {
        let resolver = documents(ONE, ITEM, &[(refused, "fixture.press")]);

        assert!(
            matches!(
                compiled(&resolver),
                Err(UiDocError::ScopeValue { value, .. }) if value == refused
            ),
            "the immediate host refuses the scope value {refused:?}"
        );
        assert!(
            retained(&resolver, &endpoints, Page::default()).is_err(),
            "the retained host refuses the scope value {refused:?}"
        );
    }
}

#[kithara::test]
fn a_literal_scope_value_that_cannot_stand_in_a_scoped_key_fails_loading() {
    let rack = RACK.replace(r#""deck": "$deck""#, r#""deck": "a,b""#);
    let resolver = documents_in(&rack, ONE, ITEM, &[]);

    assert!(
        matches!(
            compiled(&resolver),
            Err(UiDocError::ScopeValue { value, .. }) if value == "a,b"
        ),
        "the immediate host refuses the literal scope value"
    );
    assert!(
        retained(&resolver, &Endpoints::default(), Page::default()).is_err(),
        "the retained host refuses the literal scope value"
    );
}

#[kithara::test]
fn a_parsed_fill_is_named_by_the_origin_it_was_parsed_with() {
    let resolver = documents(ONE, ITEM, &[("alpha", "fixture.nowhere")]);
    let endpoints = Endpoints::default();

    let retained = retained(&resolver, &endpoints, Page::default()).err();
    let immediate = compiled(&resolver).err().map(|error| error.to_string());

    for (host, error) in [("retained", retained), ("immediate", immediate)] {
        let error = error.unwrap_or_else(|| panic!("the {host} host must refuse the fill"));
        assert!(
            error.starts_with("alpha.kmodule.ron:"),
            "the {host} host names the fill's origin: {error}"
        );
    }
}

#[kithara::test]
fn a_parsed_fill_owns_its_origin_alone() {
    let origin = SourceUri("alpha.kmodule.ron".to_owned());
    let parsed = |text: &str| {
        FillDocument::parse(text, origin.clone())
            .unwrap_or_else(|error| panic!("the fill must parse: {error}"))
    };
    let mut twice = documents(ONE, ITEM, &[]);
    twice.fill("rack/items", "alpha", parsed(&filling("fixture.press")));
    twice.fill("rack/items", "beta", parsed(SOURCED));
    let mut held_before = documents(ONE, ITEM, &[]);
    held_before.insert(&origin.0, SOURCED);
    held_before.fill("rack/items", "alpha", parsed(&filling("fixture.press")));
    let mut held_after = documents(ONE, ITEM, &[]);
    held_after.fill("rack/items", "alpha", parsed(&filling("fixture.press")));
    held_after.insert(&origin.0, SOURCED);
    let mut shared = documents(ONE, ITEM, &[]);
    shared.fill("rack/items", "alpha", parsed(&filling("fixture.press")));
    shared.fill("rack/items", "beta", parsed(&filling("fixture.press")));
    let endpoints = Endpoints::default();

    assert!(
        compiled(&shared).is_ok(),
        "the immediate host takes one document at one origin under two keys"
    );
    assert!(
        retained(&shared, &endpoints, Page::default()).is_ok(),
        "the retained host takes one document at one origin under two keys"
    );
    assert!(
        packaged(shared).is_ok(),
        "loading the package takes one document at one origin under two keys"
    );
    for (case, resolver) in [
        ("two documents", twice),
        ("a document held before", held_before),
        ("a document held after", held_after),
    ] {
        let immediate = compiled(&resolver)
            .err()
            .unwrap_or_else(|| panic!("the immediate host must refuse {case} at the origin"));
        let retained = retained(&resolver, &endpoints, Page::default())
            .err()
            .unwrap_or_else(|| panic!("the retained host must refuse {case} at the origin"));
        assert_eq!(
            retained,
            immediate.to_string(),
            "both hosts refuse {case} at the origin alike"
        );
        assert!(
            matches!(
                immediate,
                UiDocError::FillOrigin { origin: ref at, ref address, .. }
                    if *at == origin && address == "rack/items"
            ),
            "the immediate host refuses {case} at the origin"
        );
        let error = packaged(resolver)
            .err()
            .unwrap_or_else(|| panic!("loading the package must refuse {case} at the origin"));
        let message = error.to_string();
        assert!(
            matches!(
                error,
                UiDocError::FillOrigin { origin: at, address, .. }
                    if at == origin && address == "rack/items"
            ),
            "loading the package refuses {case} at the origin"
        );
        assert!(
            message.contains("alpha.kmodule.ron") && message.contains("\"rack/items\""),
            "loading the package names the origin and the address of {case}: {message}"
        );
    }
}
