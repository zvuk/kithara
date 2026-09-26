use std::{mem, rc::Rc, slice};

use iced::{
    Event, Pixels, Point, Size, Theme,
    advanced::{
        clipboard,
        layout::{Layout, Limits},
        mouse::Cursor,
        renderer::Style,
        widget::Tree,
    },
    event::Status,
    mouse::{Button, Event as MouseEvent},
    theme::Base as _,
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use iced_wgpu::{Engine, Renderer as WgpuRenderer};
use kithara_ui::{
    compile::CompiledUi,
    draw::Rect,
    render::{Clock, UiEvent, custom::CustomKinds, fonts::SANS, tree},
    view,
};

use crate::{
    FrameHost, Step, acquisitions,
    fixture::{CensusReads, Fixture, census_kinds},
    scenarios::Consts,
    skin, theme,
};

/// The immediate host: every frame rebuilds the element tree from the compiled
/// document, lays it out, applies the queued events and draws.
pub(crate) struct Immediate {
    cache: Cache,
    ui: CompiledUi,
    cursor: Cursor,
    kinds: CustomKinds,
    reads: Rc<CensusReads>,
    renderer: iced::Renderer,
    theme: Theme,
    pending: Vec<(Event, Cursor)>,
    scheduled: bool,
    captured: usize,
}

impl Immediate {
    /// The renderer is the one the application draws through. tiny-skia has no
    /// custom-shader primitive: under it the Vis and Shader passes never run
    /// and their frame times are times for work that did not happen.
    pub(crate) fn new(fixture: &Fixture, reads: Rc<CensusReads>, engine: &Engine) -> Self {
        Self {
            reads,
            cache: Cache::default(),
            captured: 0,
            cursor: Cursor::Unavailable,
            kinds: census_kinds(),
            pending: Vec::new(),
            renderer: FallbackRenderer::Primary(WgpuRenderer::new(
                engine.clone(),
                SANS,
                Pixels(14.0),
            )),
            scheduled: false,
            theme: theme(skin()),
            ui: fixture.compiled(),
        }
    }
}

impl FrameHost for Immediate {
    fn frame(&mut self) -> usize {
        let bounds = Size::new(f32::from(Consts::WIDTH), f32::from(Consts::HEIGHT));
        let element = tree::render(
            &self.ui.root,
            &self.ui,
            self.reads.as_ref(),
            &view::EMPTY,
            skin(),
            Clock::default(),
            Some(&self.kinds),
        );
        let mut interface = UserInterface::build(
            element,
            bounds,
            mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages: Vec<UiEvent> = Vec::new();
        self.scheduled = !self.pending.is_empty();
        for (event, at) in mem::take(&mut self.pending) {
            let (state, statuses) = interface.update(
                slice::from_ref(&event),
                at,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.captured += statuses
                .iter()
                .filter(|status| matches!(status, Status::Captured))
                .count();
            self.scheduled |= redraw_asked(&state);
        }
        let base = self.theme.base();
        interface.draw(
            &mut self.renderer,
            &self.theme,
            &Style {
                text_color: base.text_color,
            },
            self.cursor,
        );
        self.cache = interface.into_cache();
        acquisitions(&self.ui)
    }

    fn interact(&mut self, step: Step) {
        match step {
            Step::Move(at) => {
                let position = Point::new(at.x, at.y);
                self.cursor = Cursor::Available(position);
                self.pending.push((
                    Event::Mouse(MouseEvent::CursorMoved { position }),
                    self.cursor,
                ));
            }
            Step::Press => self.pending.push((
                Event::Mouse(MouseEvent::ButtonPressed(Button::Left)),
                self.cursor,
            )),
            Step::Release => self.pending.push((
                Event::Mouse(MouseEvent::ButtonReleased(Button::Left)),
                self.cursor,
            )),
            Step::Data => self.reads.bump(),
        }
    }

    fn receipts(&self) -> usize {
        self.captured
    }

    /// The immediate host has no seam at which it can decline to draw: it
    /// rebuilds the element tree and draws every time it is asked. What it can
    /// report is what iced's runtime would have been told - an event arrived,
    /// or a widget asked for another frame.
    fn scheduled(&self) -> bool {
        self.scheduled
    }
}

/// Whether a widget asked iced's runtime to come back with another frame.
fn redraw_asked(state: &State) -> bool {
    match state {
        State::Outdated
        | State::Updated {
            redraw_request: RedrawRequest::NextFrame | RedrawRequest::At(_),
            ..
        } => true,
        State::Updated {
            redraw_request: RedrawRequest::Wait,
            ..
        } => false,
    }
}

/// The rect the fixture's layout gives the one control in it.
///
/// This is the only laid-out rect an integration test can reach. The retained
/// host resolves a control's rect from its own mounted widget tree, but the
/// call that does it (`Ui::rect_of`) is `#[cfg(test)]` inside the crate, and
/// the index it reads is only filled there too, so both hosts are driven at
/// the geometry measured here and each is made to prove it was hit.
pub(crate) fn laid_out_rect(
    fixture: &Fixture,
    reads: &CensusReads,
    renderer: &iced::Renderer,
) -> Rect {
    let ui = fixture.compiled();
    let mut element = tree::render(
        &ui.root,
        &ui,
        reads,
        &view::EMPTY,
        skin(),
        Clock::default(),
        None,
    );
    let mut tree = Tree::new(element.as_widget());
    let bounds = Size::new(f32::from(Consts::WIDTH), f32::from(Consts::HEIGHT));
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &Limits::new(Size::ZERO, bounds));
    control_rect(&ui, Layout::new(&node))
}

/// The fixture is fixed: a plain module whose root is a Row that fills it and
/// holds the control alone. That shape is two wrappers - the row's own size,
/// then its padding - above the control, and every step asserts, so a change
/// in how a Row is wrapped fails here instead of silently naming another node.
fn control_rect(ui: &CompiledUi, root: Layout<'_>) -> Rect {
    assert!(
        !ui.resize_edges && ui.dragged.is_none(),
        "the frame-perf layout must not wrap its module in a window layer"
    );
    let mut at = root;
    for wrapper in ["the row's own size", "the row's padding", "the control"] {
        at = only_child(at, wrapper);
    }
    let bounds = at.bounds();
    Rect {
        x: bounds.x,
        y: bounds.y,
        w: bounds.width,
        h: bounds.height,
    }
}

fn only_child<'layout>(layout: Layout<'layout>, what: &str) -> Layout<'layout> {
    let mut children = layout.children();
    let Some(first) = children.next() else {
        panic!("{what} must be the one child of the node above it, which has none");
    };
    assert!(
        children.next().is_none(),
        "{what} must be the one child of the node above it, which has more"
    );
    first
}
