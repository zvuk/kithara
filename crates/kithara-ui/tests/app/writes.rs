//! A gesture on a compiled document reaches the host as the write the document
//! declares for it: the scoped endpoint key and a value typed by the endpoint.

use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config},
    builtin,
    compile::{CompiledUi, compile},
    draw::Pt,
    error::UiDocError,
    interact::Key,
    mock::MapEndpoints,
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{ReadValue, Reads, Scope, Skin, UiEvent, WriteValue},
    source::{MemResolver, UiConfig},
    view,
};

use crate::{scenario::Scenario, ui::skin};

const DOCUMENT: &str = "writes.klayout.ron";

fn endpoints() -> MapEndpoints {
    let mut registry = MapEndpoints::default();
    for (category, id, kind) in [
        (
            EndpointCategory::Command,
            "fixture.play",
            ValueKind::Trigger,
        ),
        (EndpointCategory::Model, "fixture.gain", ValueKind::Scalar),
        (
            EndpointCategory::Parameter,
            "fixture.gain",
            ValueKind::Scalar,
        ),
        (
            EndpointCategory::Parameter,
            "fixture.rate",
            ValueKind::Scalar,
        ),
        (EndpointCategory::Model, "fixture.rate", ValueKind::Text),
        (EndpointCategory::Model, "fixture.mode", ValueKind::Scalar),
        (
            EndpointCategory::Parameter,
            "fixture.mode",
            ValueKind::Index,
        ),
    ] {
        registry.insert(category, id, EndpointDesc::new(kind).with_scope("deck"));
    }
    registry
}

fn deck(play: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        DOCUMENT,
        r#"(schema: "kithara.layout", version: 1, id: "writes",
            root: Module(instance: "deck-b", source: "deck.kmodule.ron", with: { "deck": "b" },
                size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "deck.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "gallery-knobs", chrome: Plain,
                parameters: ["deck"],
                root: Row(size: (w: Fill, h: Fill), gap: 8.0, pad: 8.0, children: [
                    Chip(id: "{play}", size: Some((w: Fixed(60.0), h: Fixed(30.0))), label: "PLAY",
                        write: Command(id: "fixture.play", with: {{ "deck": "$deck" }})),
                    Chip(id: "panel", size: Some((w: Fixed(60.0), h: Fixed(30.0))), label: "PANEL",
                        read: View(id: "panel"), write: View(id: "panel")),
                    Knob(id: "gain", size: (w: Fixed(38.0), h: Fixed(49.0)),
                        read: Model(id: "fixture.gain", with: {{ "deck": "$deck" }}),
                        write: Parameter(id: "fixture.gain", with: {{ "deck": "$deck" }})),
                    Segmented(id: "mode", items: ["A", "B", "C", "D"], size: (w: Fixed(120.0), h: Fixed(26.0)),
                        read: Model(id: "fixture.mode", with: {{ "deck": "$deck" }}),
                        write: Parameter(id: "fixture.mode", with: {{ "deck": "$deck" }})),
                    Row(id: "tempo", size: (w: Fixed(80.0), h: Fill),
                        write: Parameter(id: "fixture.rate", with: {{ "deck": "$deck" }}),
                        reset: Command(id: "fixture.play", with: {{ "deck": "$deck" }}), children: [
                        Text(id: "tempo-label", style: MicroLabel, label: "TEMPO"),
                    ]),
                    Row(id: "typed", size: (w: Fixed(80.0), h: Fill),
                        write: Parameter(id: "fixture.rate", with: {{ "deck": "$deck" }}),
                        entry: Model(id: "fixture.rate", with: {{ "deck": "$deck" }}), children: [
                        Text(id: "typed-label", style: MicroLabel, label: "TEMPO"),
                    ]),
                ]))"#
        ),
    );
    resolver
}

struct Deck;

impl Reads for Deck {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = Scope::split(endpoint).0;
        match id {
            "fixture.gain" => Some(ReadValue::Scalar(0.5)),
            "fixture.mode" => Some(ReadValue::Scalar(0.0)),
            "fixture.rate" => Some(ReadValue::Text("124.00")),
            _ => None,
        }
    }
}

impl App for Deck {
    fn document(&self) -> &str {
        DOCUMENT
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        skin()
    }

    fn update(&mut self, _event: UiEvent) {}
}

fn compiled(resolver: &MemResolver, endpoints: &MapEndpoints) -> Result<CompiledUi, UiDocError> {
    compile(
        DOCUMENT,
        resolver,
        endpoints,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
}

fn mount(play: &str) -> (MapEndpoints, MemResolver) {
    (endpoints(), deck(play))
}

fn delivered(play: &str, gesture: impl FnOnce(&mut Scenario<'_, Deck>)) -> Option<UiEvent> {
    let (endpoints, resolver) = mount(play);
    let mut scenario = Scenario::mount(
        Deck,
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (480, 80),
        1.0,
    );
    gesture(&mut scenario);
    scenario
        .published()
        .iter()
        .rev()
        .find(|event| matches!(event, UiEvent::Write { .. }))
        .cloned()
}

fn write(key: &str, value: WriteValue) -> Option<UiEvent> {
    Some(UiEvent::Write {
        key: key.to_owned(),
        value,
    })
}

#[kithara::test]
fn a_press_delivers_the_scoped_trigger_it_declares() {
    let host = delivered("play", |scenario| scenario.click("deck-b/play"));

    assert_eq!(host, write("fixture.play@deck=b", WriteValue::Trigger));
}

#[kithara::test]
fn a_fader_delivers_the_scalar_it_moved_to() {
    let host = delivered("play", |scenario| {
        scenario.drag(
            "deck-b/gain",
            Pt { x: 0.5, y: 0.8 },
            Pt { x: 0.5, y: 0.2 },
            4,
        );
    });

    assert!(
        matches!(
            &host,
            Some(UiEvent::Write { key, value: WriteValue::Scalar(value) })
                if key == "fixture.gain@deck=b" && *value > 0.5
        ),
        "a knob dragged up must deliver the scalar it moved to, got {host:?}"
    );
}

#[kithara::test]
fn a_stepper_delivers_its_steps() {
    let host = delivered("play", |scenario| {
        scenario.wheel("deck-b/tempo-label", -1.0)
    });

    assert_eq!(host, write("fixture.rate@deck=b", WriteValue::Step(1.0)));
}

#[kithara::test]
fn a_declared_surface_writes_the_number_its_field_holds_on_enter() {
    let typed = delivered("play", |scenario| {
        scenario.double_click("deck-b/typed-label");
        scenario.type_text("128.5");
        scenario.key(Key::Enter);
    });
    let held = delivered("play", |scenario| {
        scenario.double_click("deck-b/typed-label");
        scenario.key(Key::Enter);
    });

    assert_eq!(
        typed,
        write("fixture.rate@deck=b", WriteValue::Scalar(128.5))
    );
    assert_eq!(
        held,
        write("fixture.rate@deck=b", WriteValue::Scalar(124.0))
    );
}

#[kithara::test]
fn a_declared_surface_writes_nothing_until_a_number_is_entered() {
    let refused_then_escaped = delivered("play", |scenario| {
        scenario.double_click("deck-b/typed-label");
        scenario.type_text("fast");
        scenario.key(Key::Enter);
        scenario.key(Key::Escape);
        scenario.key(Key::Enter);
    });
    let refused_then_corrected = delivered("play", |scenario| {
        scenario.double_click("deck-b/typed-label");
        scenario.type_text("fast");
        scenario.key(Key::Enter);
        for _ in "fast".chars() {
            scenario.key(Key::Backspace);
        }
        scenario.type_text("90");
        scenario.key(Key::Enter);
    });
    let unfocused = delivered("play", |scenario| {
        scenario.double_click("deck-b/typed-label");
        scenario.click("deck-b/gain");
        scenario.type_text("128");
        scenario.key(Key::Enter);
    });

    assert_eq!(refused_then_escaped, None);
    assert_eq!(unfocused, None);
    assert_eq!(
        refused_then_corrected,
        write("fixture.rate@deck=b", WriteValue::Scalar(90.0)),
        "a refused input leaves the field open"
    );
}

#[kithara::test]
fn an_undeclared_surface_still_activates_on_a_double_click() {
    let host = delivered("play", |scenario| {
        scenario.double_click("deck-b/tempo-label");
        scenario.type_text("128");
        scenario.key(Key::Enter);
    });

    assert_eq!(host, write("fixture.play@deck=b", WriteValue::Trigger));
}

#[kithara::test]
fn a_selection_delivers_the_index_it_picked() {
    let host = delivered("play", |scenario| scenario.click("deck-b/mode"));

    assert!(
        matches!(
            &host,
            Some(UiEvent::Write { key, value: WriteValue::Index(_) }) if key == "fixture.mode@deck=b"
        ),
        "a segmented press must deliver the index it picked, got {host:?}"
    );
}

#[kithara::test]
fn renaming_a_control_keeps_the_write_it_delivers() {
    let before = delivered("play", |scenario| scenario.click("deck-b/play"));
    let after = delivered("start", |scenario| scenario.click("deck-b/start"));

    assert!(before.is_some(), "the press must deliver a write");
    assert_eq!(before, after);
}

#[kithara::test]
fn a_view_flag_press_turns_the_flag_and_delivers_nothing_to_the_host() {
    let (endpoints, resolver) = mount("play");
    let mut scenario = Scenario::mount(
        Deck,
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (480, 80),
        1.0,
    );

    scenario.click("deck-b/panel");

    assert!(scenario.view().flag("deck-b/panel"));
    assert_eq!(scenario.published(), []);
}

#[kithara::test]
fn an_entry_on_a_surface_that_writes_nothing_fails_to_compile() {
    let endpoints = endpoints();
    let mut resolver = deck("play");
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "gallery-knobs", chrome: Plain,
            parameters: ["deck"],
            root: Row(id: "typed", size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0,
                entry: Model(id: "fixture.rate", with: { "deck": "$deck" }), children: []))"#,
    );

    let Err(error) = compiled(&resolver, &endpoints) else {
        panic!("a field with nowhere to write cannot stand")
    };

    assert!(
        matches!(error, UiDocError::UnwritableEntry { .. }),
        "the refusal must name the entry, got {error}"
    );
}

#[kithara::test]
fn a_fader_bound_to_a_trigger_fails_to_compile_naming_the_fader() {
    let endpoints = endpoints();
    let mut resolver = deck("play");
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "gallery-knobs", chrome: Plain,
            parameters: ["deck"],
            root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Fader(id: "level", size: (w: Fixed(30.0), h: Fixed(60.0)),
                    write: Command(id: "fixture.play", with: { "deck": "$deck" })),
            ]))"#,
    );

    let Err(error) = compiled(&resolver, &endpoints) else {
        panic!("a fader cannot produce a trigger")
    };

    assert!(
        matches!(&error, UiDocError::BindingType { path, .. } if path.ends_with("level")),
        "the refusal must name the fader, got {error}"
    );
}
