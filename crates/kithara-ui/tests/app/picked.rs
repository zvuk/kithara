use std::cell::Cell;

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    backends::paint_color,
    builtin,
    draw::Rgba,
    ids::EndpointId,
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ReadValue, Reads, Skin, UiEvent, WriteValue},
    skin::ColorRole,
    source::MemResolver,
};

/// Two rows a press picks between, each dressed by a flag of its own.
fn documents() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "pick.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "pick",
            root: Module(instance: "demo", source: "pick.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "pick.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "pick", chrome: Plain,
            root: Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Pressable(id: "one", press: Command(id: "fixture.one"),
                    child: Row(id: "one-row", size: (w: Fill, h: Fixed(26.0)), gap: 0.0,
                        active: Model(id: "fixture.first"), active_background: BgSelect,
                        children: [
                            Text(id: "one-label", style: Mono, label: "ONE",
                                active_color: Text, active: Model(id: "fixture.first")),
                        ])),
                Pressable(id: "two", press: Command(id: "fixture.two"),
                    child: Row(id: "two-row", size: (w: Fill, h: Fixed(26.0)), gap: 0.0,
                        active: Model(id: "fixture.second"), active_background: BgSelect,
                        children: [
                            Text(id: "two-label", style: Mono, label: "TWO",
                                active_color: Text, active: Model(id: "fixture.second")),
                        ])),
            ]))"#,
    );
    resolver
}

struct Endpoints {
    flag: EndpointDesc,
    press: EndpointDesc,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            flag: EndpointDesc::new(ValueKind::Bool),
            press: EndpointDesc::new(ValueKind::Trigger),
        }
    }
}

impl EndpointRegistry for Endpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        match (category, id.0.as_str()) {
            (EndpointCategory::Model, "fixture.first" | "fixture.second") => Some(&self.flag),
            (EndpointCategory::Command, "fixture.one" | "fixture.two") => Some(&self.press),
            _ => None,
        }
    }
}

/// An application that keeps which row stands picked, and answers each row's
/// flag from it. Nothing else about the document moves.
#[derive(Default)]
struct Picked {
    second: bool,
}

impl Reads for Picked {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "fixture.first" => Some(ReadValue::Bool(!self.second)),
            "fixture.second" => Some(ReadValue::Bool(self.second)),
            _ => None,
        }
    }
}

impl App for Picked {
    fn document(&self) -> &str {
        "pick.klayout.ron"
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, event: UiEvent) {
        if let UiEvent::Write {
            key,
            value: WriteValue::Trigger,
        } = event
        {
            self.second = key == "fixture.two";
        }
    }
}

/// The document mounted in this host, in a window tall enough for both rows.
fn mounted<'a>(endpoints: &'a Endpoints, resolver: &'a MemResolver) -> Ui<'a, Picked> {
    Ui::new(
        Picked::default(),
        Config::builder()
            .endpoints(endpoints)
            .resolver(resolver)
            .text(builtin::text_doc())
            .build(),
        (160, 80),
        1.0,
    )
    .unwrap_or_else(|error| panic!("the pick fixture must mount: {error}"))
}

/// The colour each row's label is written in, first row then second.
fn inks(ui: &Ui<'_, Picked>) -> (Rgba, Rgba) {
    let ink = |path: &str| {
        ui.ink_of(path)
            .unwrap_or_else(|| panic!("the retained host stands no text at {path}"))
    };
    (ink("demo/one-label"), ink("demo/two-label"))
}

/// Presses the second row, through the pointer route a person uses.
fn pick_second(ui: &mut Ui<'_, Picked>) {
    let row = ui
        .rect_of("demo/two-label")
        .expect("the second row must be laid out");
    let at = kithara_ui::draw::Pt {
        x: row.x + row.w / 2.0,
        y: row.y + row.h / 2.0,
    };
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

/// The fixture is worth running: the two faces differ, so a row that never
/// followed its flag would be visible.
#[kithara::test]
fn the_document_dresses_a_picked_row_apart_from_the_rest() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let ui = mounted(&endpoints, &resolver);

    let (picked, rest) = inks(&ui);

    assert_ne!(
        picked, rest,
        "the fixture must dress the picked row apart from the rest"
    );
}

/// The row a press picks is dressed as the picked one.
#[kithara::test]
fn the_row_a_press_picks_takes_the_picked_face() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let mut ui = mounted(&endpoints, &resolver);
    let (picked, _) = inks(&ui);

    pick_second(&mut ui);

    assert_eq!(
        inks(&ui).1,
        picked,
        "the row a press picked must take the face the picked row wears"
    );
}

/// And the row it leaves goes back to the face of a row that stands unpicked.
#[kithara::test]
fn the_row_a_press_leaves_takes_the_unpicked_face() {
    let (endpoints, resolver) = (Endpoints::default(), documents());
    let mut ui = mounted(&endpoints, &resolver);
    let (_, rest) = inks(&ui);

    pick_second(&mut ui);

    assert_eq!(
        inks(&ui).0,
        rest,
        "the row a press left must take the face an unpicked row wears"
    );
}

#[kithara::test]
fn a_bound_colour_repaints_static_row_glyph_and_text_in_place_without_text_reads() {
    struct KeyEndpoints {
        key: EndpointDesc,
        locked: EndpointDesc,
    }

    impl Default for KeyEndpoints {
        fn default() -> Self {
            Self {
                key: EndpointDesc::new(ValueKind::Color),
                locked: EndpointDesc::new(ValueKind::Bool),
            }
        }
    }

    impl EndpointRegistry for KeyEndpoints {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (EndpointCategory::Telemetry, "fixture.key") => Some(&self.key),
                (EndpointCategory::Telemetry, "fixture.locked") => Some(&self.locked),
                _ => None,
            }
        }
    }

    struct KeyApp<'s> {
        key: &'s Cell<Option<ColorRole>>,
        locked: &'s Cell<bool>,
    }

    impl Reads for KeyApp<'_> {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "fixture.key" => self.key.get().map(ReadValue::Color),
                "fixture.locked" => Some(ReadValue::Bool(self.locked.get())),
                _ => None,
            }
        }
    }

    impl App for KeyApp<'_> {
        fn document(&self) -> &str {
            "key.klayout.ron"
        }

        fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
            with(self)
        }

        fn skin(&self) -> &Skin {
            builtin::skin()
        }

        fn update(&mut self, _event: UiEvent) {}
    }

    let mut resolver = MemResolver::default();
    resolver.insert(
        "key.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "key",
            root: Module(instance: "demo", source: "key.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "key.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "key", chrome: Plain,
            root: Column(
                size: (w: Fill, h: Fill),
                children: [
                    Row(
                        id: "lit-row",
                        size: (w: Fill, h: Fixed(16.0)),
                        active: Telemetry(id: "fixture.locked"),
                        active_background: FromRead(read: Telemetry(id: "fixture.key"), base: Accent),
                        children: [Spacer(id: "fill", size: Some((w: Fill, h: Fill)))],
                    ),
                    Glyph(
                        id: "solo-icon",
                        icon: Music2,
                        style: MenuSmall,
                        color: FromRead(read: Telemetry(id: "fixture.key"), base: Warning),
                    ),
                    Text(
                        id: "plain",
                        label: "PLAIN",
                        style: PivotSmall,
                        color: FromRead(read: Telemetry(id: "fixture.key"), base: Danger),
                    ),
                    Text(
                        id: "inherit",
                        label: "INHERIT",
                        style: PivotSmall,
                        color: FromRead(read: Telemetry(id: "fixture.key"), base: Success),
                        active: Telemetry(id: "fixture.locked"),
                    ),
                ],
            ))"#,
    );
    let endpoints = KeyEndpoints::default();
    let skin = builtin::skin();
    let word = |role: ColorRole| {
        paint_color(skin.palette[role])
            .premultiply()
            .to_rgba8()
            .to_u32()
    };
    let locked = Cell::new(true);
    let key = Cell::new(Some(ColorRole::Key4m));
    let mut ui = Ui::new(
        KeyApp {
            key: &key,
            locked: &locked,
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .build(),
        (160, 80),
        1.0,
    )
    .expect("FromRead combined fixture must mount");

    assert_eq!(
        ui.ink_of("demo/plain"),
        Some(skin.palette[ColorRole::Key4m])
    );
    assert_eq!(
        ui.ink_of("demo/inherit"),
        Some(skin.palette[ColorRole::Key4m])
    );

    key.set(Some(ColorRole::Key11d));
    ui.frame(Duration::from_millis(16));
    assert_eq!(
        ui.ink_of("demo/plain"),
        Some(skin.palette[ColorRole::Key11d])
    );
    assert_eq!(
        ui.ink_of("demo/inherit"),
        Some(skin.palette[ColorRole::Key11d])
    );

    key.set(None);
    ui.frame(Duration::from_millis(16));
    assert_eq!(
        ui.ink_of("demo/plain"),
        Some(skin.palette[ColorRole::Danger])
    );
    assert_eq!(
        ui.ink_of("demo/inherit"),
        Some(skin.palette[ColorRole::Success])
    );

    let row_doc = r#"(schema: "kithara.module", version: 1, id: "key", chrome: Plain,
        root: Row(
            id: "lit-row",
            size: (w: Fill, h: Fixed(24.0)),
            active: Telemetry(id: "fixture.locked"),
            active_background: FromRead(read: Telemetry(id: "fixture.key"), base: Accent),
            children: [Spacer(id: "fill", size: Some((w: Fill, h: Fill)))],
        ))"#;
    let row_locked = Cell::new(true);
    let row_key = Cell::new(Some(ColorRole::Key4m));
    let mut row_resolver = MemResolver::default();
    row_resolver.insert(
        "key.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "key",
            root: Module(instance: "demo", source: "key.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    row_resolver.insert("key.kmodule.ron", row_doc);
    let mut row_ui = Ui::new(
        KeyApp {
            key: &row_key,
            locked: &row_locked,
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&row_resolver)
            .text(builtin::text_doc())
            .build(),
        (160, 40),
        1.0,
    )
    .expect("row-only FromRead fixture must mount");
    assert!(
        row_ui
            .scene()
            .expect("retained scene")
            .encoding()
            .draw_data
            .contains(&word(ColorRole::Key4m))
    );
    row_key.set(Some(ColorRole::Key11d));
    row_ui.frame(Duration::from_millis(16));
    let row_words = row_ui
        .scene()
        .expect("retained scene")
        .encoding()
        .draw_data
        .clone();
    assert!(
        row_words.contains(&word(ColorRole::Key11d)),
        "a static row must request a repaint when only its FromRead read changes"
    );
    assert!(!row_words.contains(&word(ColorRole::Key4m)));
    row_locked.set(false);
    row_ui.frame(Duration::from_millis(16));
    row_key.set(Some(ColorRole::Key4m));
    row_ui.frame(Duration::from_millis(16));
    assert!(
        !row_ui
            .scene()
            .expect("retained scene")
            .encoding()
            .draw_data
            .contains(&word(ColorRole::Key4m)),
        "an unlit row must not paint its active FromRead colour"
    );

    let text_doc = r#"(schema: "kithara.module", version: 1, id: "key", chrome: Plain,
        root: Text(
            id: "solo-text",
            label: "KEY",
            style: PivotSmall,
            color: FromRead(read: Telemetry(id: "fixture.key"), base: Danger),
        ))"#;
    let text_locked = Cell::new(false);
    let text_key = Cell::new(Some(ColorRole::Key4m));
    let mut text_resolver = MemResolver::default();
    text_resolver.insert(
        "key.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "key",
            root: Module(instance: "demo", source: "key.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    text_resolver.insert("key.kmodule.ron", text_doc);
    let mut text_ui = Ui::new(
        KeyApp {
            key: &text_key,
            locked: &text_locked,
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&text_resolver)
            .text(builtin::text_doc())
            .build(),
        (80, 40),
        1.0,
    )
    .expect("text-only FromRead fixture must mount");
    text_key.set(Some(ColorRole::Key11d));
    text_ui.frame(Duration::from_millis(16));
    assert!(
        text_ui
            .scene()
            .expect("retained scene")
            .encoding()
            .draw_data
            .contains(&word(ColorRole::Key11d)),
        "a static text leaf must repaint its scene when its FromRead read changes"
    );

    let glyph_doc = r#"(schema: "kithara.module", version: 1, id: "key", chrome: Plain,
        root: Glyph(
            id: "solo-icon",
            icon: Music2,
            style: MenuSmall,
            color: FromRead(read: Telemetry(id: "fixture.key"), base: Warning),
        ))"#;
    let glyph_locked = Cell::new(false);
    let glyph_key = Cell::new(Some(ColorRole::Key4m));
    let mut glyph_resolver = MemResolver::default();
    glyph_resolver.insert(
        "key.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "key",
            root: Module(instance: "demo", source: "key.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    glyph_resolver.insert("key.kmodule.ron", glyph_doc);
    let mut glyph_ui = Ui::new(
        KeyApp {
            key: &glyph_key,
            locked: &glyph_locked,
        },
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&glyph_resolver)
            .text(builtin::text_doc())
            .build(),
        (60, 40),
        1.0,
    )
    .expect("glyph-only FromRead fixture must mount");
    assert!(
        glyph_ui
            .scene()
            .expect("retained scene")
            .encoding()
            .draw_data
            .contains(&word(ColorRole::Key4m))
    );
    glyph_key.set(Some(ColorRole::Key11d));
    glyph_ui.frame(Duration::from_millis(16));
    assert!(
        glyph_ui
            .scene()
            .expect("retained scene")
            .encoding()
            .draw_data
            .contains(&word(ColorRole::Key11d)),
        "an unflagged glyph must repaint when its FromRead read changes"
    );
}
