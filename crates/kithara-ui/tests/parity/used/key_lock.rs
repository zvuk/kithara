//! The shipped key lock chip, pressed twice in quick succession on each host.
//!
//! The chip declares a double click besides its press. Nothing but a real
//! mount carries that declaration from the document to the recognizer, so the
//! chip is pressed through raw pointer input and only what it publishes is read.

use kithara_test_utils::kithara;
use kithara_ui::{
    app::{Config, Ui},
    builtin,
    compile::compile,
    draw::Pt,
    interact::{Input, MOUSE, PointerButton, PointerInput, PointerPhase},
    mock::player_registry,
    render::UiEvent,
    source::{MemResolver, UiConfig},
    view,
};

use super::press::{Menu, trigger};
use crate::immediate::Immediate;

/// The window both hosts open the document in.
const WINDOW: (u32, u32) = (240, 60);

/// The glyph inside the chip, a leaf the retained host can be asked to place.
const CHIP: &str = "keys/keylock/chip/icon";

/// The shipped key lock row, inside a module named `module`.
fn documents(module: &str) -> MemResolver {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "menu.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "menu",
            root: Module(instance: "keys", source: "keys.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "keys.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "{module}", chrome: Plain,
                root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                    Include(id: "keylock", source: "modules/deck/key-lock.kmodule.ron",
                        with: {{ "deck": "a" }}),
                ]))"#
        ),
    );
    resolver
}

fn primary(at: Pt, phase: PointerPhase) -> Input<'static> {
    Input::Pointer(PointerInput::new(
        MOUSE,
        Some(PointerButton::Primary),
        phase,
        Some(at),
        1,
    ))
}

/// Presses the chip twice through the retained host, answering with where it
/// pressed and what the chip published.
fn retained(module: &str) -> (Pt, Vec<UiEvent>) {
    let (endpoints, resolver) = (player_registry(), documents(module));
    let mut ui = Ui::new(
        Menu::default(),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        WINDOW,
        1.0,
    )
    .unwrap_or_else(|error| panic!("the key lock row must mount: {error}"));
    let rect = ui
        .rect_of(CHIP)
        .unwrap_or_else(|| panic!("the retained host stands no chip at {CHIP}"));
    let at = Pt {
        x: rect.x + rect.w / 2.0,
        y: rect.y + rect.h / 2.0,
    };
    for _ in 0..2 {
        for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
            ui.input(primary(at, phase));
        }
    }
    (at, ui.app().published.clone())
}

/// Presses the chip twice through the immediate host at `at`.
fn immediate(module: &str, at: Pt) -> Vec<UiEvent> {
    let (endpoints, resolver) = (player_registry(), documents(module));
    let ui = compile(
        "menu.klayout.ron",
        &resolver,
        &endpoints,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the key lock row must compile: {error}"));
    let mut host = Immediate::mount(Menu::default(), &ui, builtin::skin(), WINDOW);
    host.click_at(at);
    host.click_at(at);
    host.app().published.clone()
}

/// Two quick presses on the chip toggle the lock twice and reset the transpose
/// once, after the second press, in an ordinary module and in one whose input
/// the engine owns, as the shipped deck does.
#[kithara::test]
#[case::ordinary("keys")]
#[case::engine_owned("app-deck")]
fn a_quick_double_press_on_the_key_lock_chip_resets_its_transpose_once(#[case] module: &str) {
    let expected = [
        trigger("deck.key.toggle_lock@deck=a"),
        trigger("deck.key.toggle_lock@deck=a"),
        trigger("deck.key.transpose_reset@deck=a"),
    ];
    let (at, retained) = retained(module);

    assert_eq!(retained, expected, "the retained host at {at:?}");
    assert_eq!(
        immediate(module, at),
        expected,
        "the immediate host at {at:?}"
    );
}
