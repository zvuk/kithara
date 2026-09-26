use kithara_platform::time::Duration;
use kithara_ui::{
    app::{App, Config, Frame, Ui},
    draw::{PoolStats, Pt, Rect, Rgba},
    interact::{Input, MOUSE, PointerInput, PointerPhase, Scroll},
    render::{Reads, Skin, UiEvent},
    view::ViewState,
};
use masonry::vello::Scene;

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
    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        self.inner.reads(with)
    }

    fn update(&mut self, event: UiEvent) {
        self.published.push(event.clone());
        self.inner.update(event);
    }

    delegate::delegate! {
        to self.inner {
            fn document(&self) -> &str;
            fn skin(&self) -> &Skin;
            fn tick(&mut self);
        }
    }
}

/// Drives one mounted document by the path a hand would name, not the
/// coordinates a hand would first have to compute.
pub(crate) struct Scenario<'config, A: App> {
    ui: Ui<'config, Recording<A>>,
}

impl<'config, A: App> Scenario<'config, A> {
    pub(crate) fn background(&self) -> Rgba {
        self.ui.background()
    }

    /// A press immediately followed by a release, at the control's centre.
    pub(crate) fn click(&mut self, path: &str) {
        self.press(path);
        self.release(path);
    }

    /// A whole press at one point of the window, for the gestures that are
    /// aimed at no control: dismissing what stands over the page.
    pub(crate) fn click_at(&mut self, at: Pt) {
        self.ui.input(pointer(at, PointerPhase::Move));
        self.ui.input(pointer(at, PointerPhase::Down));
        self.ui.input(pointer(at, PointerPhase::Up));
    }

    /// Drags from one point in the control's rect to another. `from` and
    /// `to` are fractions of the rect on each axis (0.0..=1.0), so a test
    /// never computes a pixel itself.
    pub(crate) fn drag(&mut self, path: &str, from: Pt, to: Pt, steps: u16) {
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

    /// Mounts the document and settles its first frame, so every control's
    /// rect is real before a test acts on one.
    pub(crate) fn mount(app: A, config: Config<'config>, size: (u32, u32), scale: f64) -> Self {
        let mut ui = Ui::new(Recording::new(app), config, size, scale)
            .unwrap_or_else(|error| panic!("the scenario must mount: {error}"));
        ui.frame(Duration::from_millis(16));
        ui.render()
            .unwrap_or_else(|error| panic!("the scenario must draw its first frame: {error}"));
        Self { ui }
    }

    /// Presses at the control's own centre.
    pub(crate) fn press(&mut self, path: &str) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Move));
        self.ui.input(pointer(at, PointerPhase::Down));
    }

    fn rect(&self, path: &str) -> Rect {
        self.ui
            .rect_of(path)
            .unwrap_or_else(|| panic!("no control mounted at document path {path:?}"))
    }

    /// Releases at the control's own centre.
    pub(crate) fn release(&mut self, path: &str) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Up));
    }

    /// The frame the document draws now, shaders and meters included.
    pub(crate) fn frame(&mut self) -> Frame {
        self.ui
            .render()
            .unwrap_or_else(|error| panic!("the scenario must draw: {error}"))
    }

    /// Draws the current document.
    pub(crate) fn scene(&mut self) -> Scene {
        self.ui
            .scene()
            .unwrap_or_else(|error| panic!("the scenario must draw: {error}"))
    }

    /// A wheel notch over the control's centre.
    pub(crate) fn wheel(&mut self, path: &str, notches: f32) {
        let at = center(self.rect(path));
        self.ui.input(pointer(at, PointerPhase::Move));
        self.ui
            .input(Input::Wheel(Scroll::Lines { x: 0.0, y: notches }));
    }

    delegate::delegate! {
        to self.ui {
            /// Where a control stands, or nothing when nothing stands at that
            /// path.
            pub(crate) fn rect_of(&self, path: &str) -> Option<Rect>;
            /// Reuse counters for the pools this host draws every one of its
            /// documents from.
            pub(crate) fn draw_pool_stats(&self) -> PoolStats;
            /// The state the shown screen keeps for itself.
            pub(crate) fn view(&self) -> &ViewState;
        }
        to self.ui.app() {
            /// The application the scenario is driving.
            #[call(inner)]
            pub(crate) fn app(&self) -> &A;
            /// Every event the document has published since the scenario mounted.
            #[call(published)]
            pub(crate) fn published(&self) -> &[UiEvent];
        }
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
