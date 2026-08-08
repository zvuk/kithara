use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{CursorShape, Hit, Input, Key, Modifiers, Outcome, PointerPhase},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PickerSnapshot {
    pub(crate) open: bool,
    pub(crate) highlighted: Option<usize>,
}

pub(in crate::engine) struct PickerComponent {
    enter_held: bool,
    highlighted: Option<usize>,
    item_count: usize,
    open: bool,
    path: String,
    selected: Option<usize>,
    space_held: bool,
}

impl PickerComponent {
    pub(super) fn new(path: String, item_count: usize, selected: Option<usize>) -> Self {
        let selected = clamp(selected, item_count);
        Self {
            enter_held: false,
            highlighted: selected,
            item_count,
            open: false,
            path,
            selected,
            space_held: false,
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        self.highlighted = clamp(self.highlighted, next.item_count);
        self.item_count = next.item_count;
        self.path = next.path;
        self.selected = next.selected;
        self
    }

    pub(super) const fn snapshot(&self) -> PickerSnapshot {
        PickerSnapshot {
            open: self.open,
            highlighted: self.highlighted,
        }
    }

    fn open(&mut self) {
        self.open = true;
        if self.highlighted.is_none() {
            self.highlighted = self.selected.or_else(|| (self.item_count > 0).then_some(0));
        }
    }

    fn pointer_down(&mut self, hit: &Hit, index: Option<usize>) -> Outcome<EngineEvent> {
        if self.open
            && let Some(index) = index.filter(|index| *index < self.item_count)
            && hit.over()
        {
            self.highlighted = Some(index);
            self.open = false;
            return Outcome::set(EngineEvent::Index(index));
        }
        if index.is_some() {
            return Outcome::IGNORED;
        }
        if hit.over() {
            if self.open {
                self.open = false;
            } else {
                self.open();
            }
            return Outcome::captured();
        }
        if self.open {
            self.open = false;
            return Outcome::captured();
        }
        Outcome::IGNORED
    }

    fn pointer_moved(&mut self, hit: &Hit, index: Option<usize>) -> Outcome<EngineEvent> {
        if !self.open || !hit.over() {
            return Outcome::IGNORED;
        }
        let Some(index) = index.filter(|index| *index < self.item_count) else {
            return Outcome::IGNORED;
        };
        self.highlighted = Some(index);
        Outcome::captured()
    }

    fn key_pressed(&mut self, key: Key<'_>, _modifiers: Modifiers) -> Outcome<EngineEvent> {
        match key {
            Key::ArrowDown => {
                if !self.open {
                    self.open();
                } else if let Some(last) = self.item_count.checked_sub(1) {
                    self.highlighted =
                        Some(self.highlighted.map_or(0, |index| index + 1).min(last));
                }
                Outcome::captured()
            }
            Key::ArrowUp => {
                if !self.open {
                    self.open();
                } else if let Some(last) = self.item_count.checked_sub(1) {
                    self.highlighted = Some(
                        self.highlighted
                            .map_or(last, |index| index.saturating_sub(1)),
                    );
                }
                Outcome::captured()
            }
            Key::Enter if self.enter_held => Outcome::captured(),
            Key::Enter if self.open => {
                self.enter_held = true;
                self.highlighted.map_or_else(Outcome::captured, |index| {
                    self.open = false;
                    Outcome::set(EngineEvent::Index(index))
                })
            }
            Key::Enter => {
                self.enter_held = true;
                self.open();
                Outcome::captured()
            }
            Key::Space if self.space_held => Outcome::captured(),
            Key::Space => {
                self.space_held = true;
                if self.open {
                    self.open = false;
                } else {
                    self.open();
                }
                Outcome::captured()
            }
            Key::Escape => {
                self.open = false;
                Outcome::captured()
            }
            Key::Backspace | Key::Delete => Outcome::captured(),
            Key::ArrowLeft
            | Key::ArrowRight
            | Key::Character(_)
            | Key::End
            | Key::Home
            | Key::Other => Outcome::IGNORED,
        }
    }

    fn key_released(&mut self, key: Key<'_>, modifiers: Modifiers) -> Outcome<EngineEvent> {
        match key {
            Key::Enter => self.enter_held = false,
            Key::Space => self.space_held = false,
            _ if owns_key(key, modifiers) => return Outcome::captured(),
            _ => return Outcome::IGNORED,
        }
        Outcome::captured()
    }
}

impl Component for PickerComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::Picker
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                self.pointer_down(hit, index)
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move => {
                self.pointer_moved(hit, index)
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn handle_key(&mut self, input: Input<'_>) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = match input {
            Input::KeyPressed { key, modifiers, .. } => self.key_pressed(key, modifiers),
            Input::KeyReleased { key, modifiers } => self.key_released(key, modifiers),
            Input::InputMethod(_)
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        if hit.over() {
            CursorShape::Pointer
        } else {
            CursorShape::None
        }
    }

    fn captures_pointer(&self) -> bool {
        false
    }

    fn focusable(&self) -> bool {
        true
    }

    fn blur(&mut self) {
        self.enter_held = false;
        self.space_held = false;
    }
}

fn clamp(index: Option<usize>, item_count: usize) -> Option<usize> {
    index.and_then(|index| item_count.checked_sub(1).map(|last| index.min(last)))
}

const fn owns_key(key: Key<'_>, _modifiers: Modifiers) -> bool {
    matches!(
        key,
        Key::ArrowDown
            | Key::ArrowUp
            | Key::Backspace
            | Key::Delete
            | Key::Enter
            | Key::Escape
            | Key::Space
    )
}
