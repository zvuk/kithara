//! This test-only harness lives inside the crate because resolving a document
//! path needs the retained layout; an integration test would require widening
//! the product API with test-only surface.

use kithara_platform::time::Duration;
use masonry::vello::Scene;

use super::{App, Config, Ui};
use crate::{
    draw::{Pt, Rect, Rgba},
    interact::{Input, MOUSE, PointerInput, PointerPhase, Scroll},
    render::{Reads, UiEvent},
};

/// Wraps an application, recording every event the document published to
/// it, so a test asserts on the consequence of a gesture instead of
/// re-deriving it from the application's own bookkeeping.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(get)]
struct Recording<A> {
    inner: A,
    published: Vec<UiEvent>,
}

impl<A> Recording<A> {
    fn new(inner: A) -> Self {
        Self {
            inner,
            published: Vec::new(),
        }
    }
}

impl<A: App> App for Recording<A> {
    delegate::delegate! {
        to self.inner {
            fn document(&self) -> &str;
            fn tick(&mut self);
        }
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        self.inner.reads(with)
    }

    fn update(&mut self, event: UiEvent) {
        self.published.push(event.clone());
        self.inner.update(event);
    }
}

/// Drives one mounted document by the path a hand would name, not the
/// coordinates a hand would first have to compute.
pub(super) struct Scenario<'config, A: App> {
    ui: Ui<'config, Recording<A>>,
}

impl<'config, A: App> Scenario<'config, A> {
    /// Mounts the document and settles its first frame, so every control's
    /// rect is real before a test acts on one.
    pub(super) fn mount(app: A, config: Config<'config>, size: (u32, u32), scale: f64) -> Self {
        let mut ui = Ui::new(Recording::new(app), config, size, scale)
            .unwrap_or_else(|error| panic!("the scenario must mount: {error}"));
        ui.frame(Duration::from_millis(16));
        ui.render()
            .unwrap_or_else(|error| panic!("the scenario must draw its first frame: {error}"));
        Self { ui }
    }

    delegate::delegate! {
        to self.ui.app() {
            /// The application the scenario is driving.
            #[call(inner)]
            pub(super) fn app(&self) -> &A;
            /// Every event the document has published since the scenario mounted.
            #[call(published)]
            pub(super) fn published(&self) -> &[UiEvent];
        }
    }

    /// Draws the current document.
    pub(super) fn scene(&mut self) -> Scene {
        self.ui
            .scene()
            .unwrap_or_else(|error| panic!("the scenario must draw: {error}"))
    }

    pub(super) const fn background(&self) -> Rgba {
        self.ui.background()
    }

    fn rect(&self, path: &str) -> Rect {
        self.ui
            .rect_of(path)
            .unwrap_or_else(|| panic!("no control mounted at document path {path:?}"))
    }

    /// Presses at the control's own centre.
    pub(super) fn press(&mut self, path: &str) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Move));
        self.ui.input(pointer(at, PointerPhase::Down));
    }

    /// Releases at the control's own centre.
    pub(super) fn release(&mut self, path: &str) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Up));
    }

    /// A press immediately followed by a release, at the control's centre.
    pub(super) fn click(&mut self, path: &str) {
        self.press(path);
        self.release(path);
    }

    /// Drags from one point in the control's rect to another. `from` and
    /// `to` are fractions of the rect on each axis (0.0..=1.0), so a test
    /// never computes a pixel itself.
    pub(super) fn drag(&mut self, path: &str, from: Pt, to: Pt, steps: u16) {
        let rect = self.rect(path);
        let anchor = |fraction: Pt| Pt {
            x: inside(rect.x, rect.w, fraction.x),
            y: inside(rect.y, rect.h, fraction.y),
        };
        let (start, end) = (anchor(from), anchor(to));
        self.ui.input(pointer(start, PointerPhase::Move));
        self.ui.input(pointer(start, PointerPhase::Down));
        for step in 1..=steps {
            let fraction = f32::from(step) / f32::from(steps);
            let at = Pt {
                x: start.x + (end.x - start.x) * fraction,
                y: start.y + (end.y - start.y) * fraction,
            };
            self.ui.input(pointer(at, PointerPhase::Move));
        }
        self.ui.input(pointer(end, PointerPhase::Up));
    }

    /// A wheel notch over the control's centre.
    pub(super) fn wheel(&mut self, path: &str, notches: f32) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Move));
        self.ui
            .input(Input::Wheel(Scroll::Lines { x: 0.0, y: notches }));
    }
}

fn center(rect: Rect) -> Pt {
    Pt {
        x: rect.x + rect.w / 2.0,
        y: rect.y + rect.h / 2.0,
    }
}

fn inside(origin: f32, length: f32, fraction: f32) -> f32 {
    (origin + fraction * length).min((origin + length).next_down())
}

fn pointer(at: Pt, phase: PointerPhase) -> Input<'static> {
    Input::Pointer(PointerInput::new(MOUSE, None, phase, Some(at), 1))
}
