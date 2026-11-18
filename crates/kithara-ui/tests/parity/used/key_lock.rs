//! The shipped key lock chip, pressed twice in quick succession on each host.
//!
//! The chip declares a double click besides its press. Nothing but a real
//! mount carries that declaration from the document to the recognizer, so the
//! chip is pressed through raw pointer input and only what it publishes is read.

use std::cell::Cell;

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    backends::paint_color,
    builtin,
    compile::compile,
    draw::{Pt, Rect},
    interact::{Input, MOUSE, PointerButton, PointerInput, PointerPhase},
    mock::player_registry,
    render::{ReadValue, Reads, Skin, UiEvent},
    skin::ColorRole,
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

#[kithara::test]
fn the_shipped_key_lock_chip_shows_key_colours_and_charges_no_gap_for_empty_runs() {
    struct ChipState<'s> {
        locked: &'s Cell<bool>,
        key: &'s Cell<&'static str>,
        transpose: &'s Cell<&'static str>,
    }

    impl Reads for ChipState<'_> {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "deck.key.locked@deck=a" => Some(ReadValue::Bool(self.locked.get())),
                "deck.key.sounding@deck=a" => Some(ReadValue::Text(self.key.get())),
                "deck.key.color@deck=a" => match self.key.get() {
                    "4m" => Some(ReadValue::Color(ColorRole::Key4m)),
                    "11m" => Some(ReadValue::Color(ColorRole::Key11m)),
                    _ => None,
                },
                "deck.key.sounding_hidden@deck=a" => {
                    Some(ReadValue::Bool(self.key.get().is_empty()))
                }
                "deck.key.transpose_hidden@deck=a" => {
                    Some(ReadValue::Bool(self.transpose.get().is_empty()))
                }
                "deck.key.transpose@deck=a" => Some(ReadValue::Text(self.transpose.get())),
                _ => None,
            }
        }
    }

    impl App for ChipState<'_> {
        fn document(&self) -> &str {
            "menu.klayout.ron"
        }

        fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
            with(self)
        }

        fn skin(&self) -> &Skin {
            builtin::skin()
        }

        fn update(&mut self, _event: UiEvent) {}
    }

    let (endpoints, resolver) = (player_registry(), documents("keys"));
    let ui_doc = compile(
        "menu.klayout.ron",
        &resolver,
        &endpoints,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .expect("key-lock module must compile");
    let locked = Cell::new(true);
    let key = Cell::new("");
    let transpose = Cell::new("");
    let mut ui = Ui::new(
        ChipState {
            locked: &locked,
            key: &key,
            transpose: &transpose,
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        WINDOW,
        1.0,
    )
    .expect("key-lock module must mount");
    let skin = builtin::skin();
    let word = |role: ColorRole| {
        paint_color(skin.palette[role])
            .premultiply()
            .to_rgba8()
            .to_u32()
    };

    let left_rect = ui
        .rect_of("keys/keylock/down-icon")
        .expect("retained left chevron rect");
    let right_rect = ui
        .rect_of("keys/keylock/up-icon")
        .expect("retained right chevron rect");
    let chip_width = right_rect.x - (left_rect.x + left_rect.w);
    assert_eq!(chip_width, 56.0, "the chip reserves its full width");
    let mut imm = Immediate::mount(
        ChipState {
            locked: &locked,
            key: &key,
            transpose: &transpose,
        },
        &ui_doc,
        skin,
        WINDOW,
    );
    let imm_quads = imm.quads();
    let gold_bg = iced::Background::Color(skin.palette[ColorRole::Accent].into());
    assert!(
        imm_quads
            .iter()
            .any(|(_, quad, bg)| *bg == gold_bg && Rect::from(quad.bounds).w == chip_width),
        "immediate keyless locked chip must fill the reserved width in gold: {imm_quads:#?}"
    );

    key.set("4m");
    ui.frame(Duration::from_millis(16));
    let keyed_width = ui
        .rect_of("keys/keylock/up-icon")
        .expect("retained right chevron rect after key")
        .x
        - (left_rect.x + left_rect.w);
    assert_eq!(keyed_width, chip_width);
    assert_eq!(
        ui.ink_of("keys/keylock/chip/key"),
        Some(skin.palette[ColorRole::Bg])
    );
    let locked_words = ui
        .scene()
        .expect("retained scene")
        .encoding()
        .draw_data
        .clone();
    assert!(locked_words.contains(&word(ColorRole::Key4m)));
    let mut imm_keyed = Immediate::mount(
        ChipState {
            locked: &locked,
            key: &key,
            transpose: &transpose,
        },
        &ui_doc,
        skin,
        WINDOW,
    );
    let key4m_bg = iced::Background::Color(skin.palette[ColorRole::Key4m].into());
    let imm_keyed_quads = imm_keyed.quads();
    assert!(
        imm_keyed_quads.iter().any(|(_, quad, bg)| {
            let width = Rect::from(quad.bounds).w;
            *bg == key4m_bg && (width - keyed_width).abs() <= 1.0
        }),
        "immediate locked chip must keep ~{keyed_width}px in Key4m: {imm_keyed_quads:#?}"
    );

    locked.set(false);
    key.set("11m");
    transpose.set("+1");
    ui.frame(Duration::from_millis(16));
    assert_eq!(
        ui.ink_of("keys/keylock/chip/key"),
        Some(skin.palette[ColorRole::Key11m])
    );
    assert_eq!(
        ui.ink_of("keys/keylock/chip/readout"),
        Some(skin.palette[ColorRole::Accent])
    );
}
