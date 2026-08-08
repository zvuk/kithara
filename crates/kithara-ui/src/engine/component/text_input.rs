use std::ops::Range;

use kithara_platform::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{
        CursorShape, Hit, Input, InputMethod, InputMethodRequest, Key, Modifiers, Outcome,
        PointerPhase, PreeditRef, Rect, TextInputLayout,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreeditSnapshot {
    pub(crate) content: String,
    pub(crate) selection: Option<Range<usize>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TextInputSnapshot {
    pub(crate) caret: usize,
    pub(crate) focused: bool,
    pub(crate) preedit: Option<PreeditSnapshot>,
    pub(crate) selection: Option<Range<usize>>,
}

#[derive(Clone, Copy, Default)]
struct Cursor {
    anchor: usize,
    focus: usize,
}

impl Cursor {
    fn clamp(&mut self, layout: &TextInputLayout) {
        self.anchor = layout.clamp(self.anchor);
        self.focus = layout.clamp(self.focus);
    }

    fn collapse(&mut self, index: usize) {
        self.anchor = index;
        self.focus = index;
    }

    fn move_to(&mut self, index: usize, select: bool) {
        if !select {
            self.anchor = index;
        }
        self.focus = index;
    }

    fn left(&mut self, query: &str, select: bool) {
        let index = if select || self.anchor == self.focus {
            previous_grapheme(query, self.focus)
        } else {
            self.anchor.min(self.focus)
        };
        self.move_to(index, select);
    }

    fn right(&mut self, query: &str, select: bool) {
        let index = if select || self.anchor == self.focus {
            next_grapheme(query, self.focus)
        } else {
            self.anchor.max(self.focus)
        };
        self.move_to(index, select);
    }

    fn selection(self) -> Option<Range<usize>> {
        (self.anchor != self.focus)
            .then(|| self.anchor.min(self.focus)..self.anchor.max(self.focus))
    }
}

fn previous_grapheme(query: &str, index: usize) -> usize {
    query
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|candidate| *candidate < index)
        .last()
        .unwrap_or(0)
}

fn next_grapheme(query: &str, index: usize) -> usize {
    query
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(query.len()))
        .find(|candidate| *candidate > index)
        .unwrap_or(query.len())
}

fn grapheme_boundary_at_or_after(query: &str, index: usize) -> usize {
    query
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(query.len()))
        .find(|candidate| *candidate >= index)
        .unwrap_or(query.len())
}

pub(in crate::engine) struct TextInputComponent {
    cursor: Cursor,
    dragging: bool,
    layout: TextInputLayout,
    modifiers: Modifiers,
    path: String,
    preedit: Option<PreeditSnapshot>,
    query: String,
}

impl TextInputComponent {
    pub(super) fn new(path: String, query: String, layout: TextInputLayout) -> Self {
        Self {
            cursor: Cursor::default(),
            dragging: false,
            layout,
            modifiers: Modifiers::default(),
            path,
            preedit: None,
            query,
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        self.layout = next.layout;
        self.path = next.path;
        self.query = next.query;
        self.cursor.clamp(&self.layout);
        self
    }

    pub(super) fn snapshot(&self, focused: bool) -> TextInputSnapshot {
        TextInputSnapshot {
            caret: self.cursor.focus,
            focused,
            preedit: self.preedit.clone(),
            selection: self.cursor.selection(),
        }
    }

    pub(super) fn input_method(&self, area: Rect) -> InputMethodRequest<'_> {
        let caret = self
            .cursor
            .selection()
            .map_or(self.cursor.focus, |selection| selection.start);
        InputMethodRequest {
            caret: self.layout.caret(caret, area),
            preedit: self.preedit.as_ref().map(|preedit| PreeditRef {
                content: &preedit.content,
                selection: preedit.selection.clone(),
            }),
            text_size: self.layout.text_size(),
        }
    }

    fn edit(&mut self, inserted: &str) -> Outcome<EngineEvent> {
        let range = self
            .cursor
            .selection()
            .unwrap_or(self.cursor.focus..self.cursor.focus);
        let mut query = self.query.clone();
        query.replace_range(range.clone(), inserted);
        self.cursor.collapse(grapheme_boundary_at_or_after(
            &query,
            range.start + inserted.len(),
        ));
        self.query.clone_from(&query);
        Outcome::set(EngineEvent::Text(query))
    }

    fn backspace(&mut self) -> Outcome<EngineEvent> {
        if self.cursor.selection().is_none() {
            self.cursor.anchor = previous_grapheme(&self.query, self.cursor.focus);
        }
        self.edit("")
    }

    fn delete(&mut self) -> Outcome<EngineEvent> {
        if self.cursor.selection().is_none() {
            self.cursor.anchor = next_grapheme(&self.query, self.cursor.focus);
        }
        self.edit("")
    }

    fn key_pressed(
        &mut self,
        key: Key<'_>,
        modifiers: Modifiers,
        text: Option<&str>,
    ) -> Outcome<EngineEvent> {
        if let Some(text) = text
            && !text.is_empty()
            && text.chars().all(|character| !character.is_control())
        {
            return self.edit(text);
        }
        match key {
            Key::ArrowLeft => {
                self.cursor.left(&self.query, modifiers.shift());
                Outcome::captured()
            }
            Key::ArrowRight => {
                self.cursor.right(&self.query, modifiers.shift());
                Outcome::captured()
            }
            Key::Home => {
                self.cursor.move_to(0, modifiers.shift());
                Outcome::captured()
            }
            Key::End => {
                self.cursor.move_to(self.query.len(), modifiers.shift());
                Outcome::captured()
            }
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::ArrowDown
            | Key::ArrowUp
            | Key::Character(_)
            | Key::Enter
            | Key::Escape
            | Key::Other
            | Key::Space => Outcome::IGNORED,
        }
    }

    fn input_method_event(&mut self, event: InputMethod<'_>) -> Outcome<EngineEvent> {
        match event {
            InputMethod::Opened => {
                self.preedit = Some(PreeditSnapshot {
                    content: String::new(),
                    selection: None,
                });
                Outcome::captured()
            }
            InputMethod::Preedit { content, selection } => {
                self.preedit = Some(PreeditSnapshot {
                    content: content.to_owned(),
                    selection: selection.map(|(start, end)| start..end),
                });
                Outcome::captured()
            }
            InputMethod::Commit(content) => {
                self.preedit = None;
                self.edit(content)
            }
            InputMethod::Closed => {
                self.preedit = None;
                Outcome::captured()
            }
        }
    }
}

impl Component for TextInputComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::TextInput
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = match input {
            Input::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                Outcome::IGNORED
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down && hit.over() => {
                self.cursor
                    .move_to(self.layout.index_at(*hit), self.modifiers.shift());
                self.dragging = true;
                Outcome::captured()
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Move && self.dragging => {
                self.cursor.move_to(self.layout.index_at(*hit), true);
                Outcome::captured()
            }
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up && self.dragging => {
                self.dragging = false;
                Outcome::captured()
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn handle_key(&mut self, input: Input<'_>) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = match input {
            Input::KeyPressed {
                key,
                modifiers,
                text,
            } => self.key_pressed(key, modifiers, text),
            Input::InputMethod(event) => self.input_method_event(event),
            Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        if hit.over() {
            CursorShape::Text
        } else {
            CursorShape::None
        }
    }

    fn captures_pointer(&self) -> bool {
        self.dragging
    }

    fn cancel_pointer(&mut self) {
        self.dragging = false;
    }

    fn focusable(&self) -> bool {
        true
    }

    fn blur(&mut self) {
        self.dragging = false;
        self.preedit = None;
    }
}
