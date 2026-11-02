use kithara_platform::time::Instant;

use crate::{
    atoms::text_input::{TextInputPaint, text_input_layout},
    draw::{DrawListBuilder, Rect},
    engine::{Descriptor, Engine, EngineEvent, Target},
    expand::Binding,
    interact::{
        CursorShape, Hit, Hover, Input, Key, Outcome, PointerPhase,
        recognizers::{StepEvent, Stepper},
    },
    render::{ControlAction, ReadValue, Skin, document::Ctx},
};

/// What a surface that declares an entry opens its field with.
#[derive(Clone, Copy)]
pub(crate) struct Entry<'a> {
    pub(crate) value: &'a str,
    pub(crate) skin: &'a Skin,
}

/// The text the document names for a surface's field to open holding.
pub(crate) fn entry_value(ctx: Ctx<'_, '_>, binding: &Binding) -> String {
    match ctx.read(binding) {
        Some(ReadValue::Text(text)) => text.to_owned(),
        _ => String::new(),
    }
}

struct NumberField {
    engine: Engine,
    draft: String,
}

impl NumberField {
    /// The name of the one text input the private engine drives.
    const FIELD: &str = "entry";

    /// Opens focused with the whole value selected, so typing replaces it.
    fn open(entry: Entry<'_>) -> Self {
        let mut field = Self {
            engine: Engine::default(),
            draft: entry.value.to_owned(),
        };
        field.reconcile(entry.skin);
        field.engine.open_text_input(Self::FIELD);
        field
    }

    fn edit(&mut self, input: Input<'_>, hit: &Hit, now: Instant, skin: &Skin) {
        let emission = self
            .engine
            .handle(input, &[Target::new(Self::FIELD, *hit)], now);
        if let Some(EngineEvent::Text(draft)) = emission.and_then(|edit| edit.outcome.value()) {
            self.draft = draft;
            self.reconcile(skin);
        }
    }

    fn number(&self) -> Option<f64> {
        self.draft
            .trim()
            .parse()
            .ok()
            .filter(|number: &f64| number.is_finite())
    }

    fn reconcile(&mut self, skin: &Skin) {
        self.engine.reconcile([Descriptor::text_input(
            Self::FIELD.to_owned(),
            self.draft.clone(),
            text_input_layout(&self.draft, skin),
        )]);
    }
}

/// The gesture state of one stepping surface: the stepper, and the number
/// field a double-click opens where the document declares an entry.
#[derive(Default)]
pub(crate) struct SteppingSurface {
    field: Option<NumberField>,
    stepper: Stepper,
}

impl SteppingSurface {
    /// Closes the field unchanged, answering whether one stood open.
    pub(crate) fn close(&mut self) -> bool {
        self.field.take().is_some()
    }

    pub(crate) fn cursor(&self, hit: &Hit) -> CursorShape {
        let shape = if self.editing() {
            CursorShape::Text
        } else {
            CursorShape::ResizeV
        };
        Hover::new(shape).cursor(self.stepper.dragging(), hit)
    }

    #[cfg(feature = "masonry")]
    pub(crate) const fn dragging(&self) -> bool {
        self.stepper.dragging()
    }

    pub(crate) const fn editing(&self) -> bool {
        self.field.is_some()
    }

    pub(crate) fn on_input(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        now: Instant,
        entry: Option<Entry<'_>>,
    ) -> Outcome<ControlAction> {
        let (Some(field), Some(declared)) = (&mut self.field, entry) else {
            return self.step(input, hit, now, entry);
        };
        match input {
            Input::KeyPressed {
                key: Key::Enter, ..
            } => {
                let Some(number) = field.number() else {
                    return Outcome::captured();
                };
                self.field = None;
                Outcome::set(ControlAction::SetScalar(number))
            }
            Input::KeyPressed {
                key: Key::Escape, ..
            } => {
                self.field = None;
                Outcome::captured()
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down && !hit.over() => {
                self.field = None;
                Outcome::IGNORED
            }
            Input::Pointer(_) | Input::Wheel(_) => {
                field.edit(input, hit, now, declared.skin);
                if hit.over() {
                    Outcome::captured()
                } else {
                    Outcome::IGNORED
                }
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_) => {
                field.edit(input, hit, now, declared.skin);
                Outcome::captured()
            }
        }
    }

    fn step(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        now: Instant,
        entry: Option<Entry<'_>>,
    ) -> Outcome<ControlAction> {
        let outcome = self.stepper.on_input(input, hit, now);
        if let (Some(StepEvent::Activate), Some(entry)) = (outcome.value(), entry) {
            self.field = Some(NumberField::open(entry));
            return Outcome::captured();
        }
        outcome.map(|event| match event {
            StepEvent::By(steps) => ControlAction::StepScalar(steps),
            StepEvent::Activate => ControlAction::Activate,
        })
    }

    /// Draws the open field over `bounds`.
    pub(crate) fn paint(&self, list: &mut DrawListBuilder, bounds: Rect, skin: &Skin) {
        let Some(field) = &self.field else {
            return;
        };
        if let Some(snapshot) = field.engine.text_input_snapshot(NumberField::FIELD) {
            TextInputPaint::new(&field.draft, skin)
                .without_placeholder()
                .paint(list, &snapshot, bounds);
        }
    }
}
