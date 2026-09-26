use std::{mem, slice};

use hotpath::measure_block;
use iced::{
    Color, Event, Pixels, Point, Size, Theme,
    advanced::{clipboard, graphics::Viewport, mouse::Cursor, renderer::Style},
    mouse::{Button as MouseButton, Event as MouseEvent, ScrollDelta},
    theme::Base as _,
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use iced_wgpu::{
    Renderer as WgpuRenderer,
    wgpu::{Device, Queue, Texture, TextureViewDescriptor},
};
use kithara_ui::{
    app::App,
    builtin,
    compile::CompiledUi,
    draw::Pt,
    render::{Clock, UiEvent, fonts::SANS, tree},
    view::{self},
};

use crate::{
    Page, PageHost,
    app::PageApp,
    census::{Census, Pool},
    fixture::Consts,
    gpu::{ImmediateGpu, digest, drain, height, readback, width},
    pages::Harness,
    theme,
};

/// The immediate host: every frame rebuilds the element tree from the compiled
/// document, lays it out, applies the queued events, draws and presents.
pub(crate) struct Immediate {
    cache: Cache,
    ui: CompiledUi,
    cursor: Cursor,
    pub(crate) device: Device,
    app: PageApp,
    pointer_at: Pt,
    pub(crate) queue: Queue,
    renderer: iced::Renderer,
    pub(crate) texture: Texture,
    theme: Theme,
    pending: Vec<Event>,
    pub(crate) scheduled: bool,
}

impl Immediate {
    pub(crate) fn new(ui: CompiledUi, page: &Page, app: PageApp, gpu: &ImmediateGpu) -> Self {
        Self {
            app,
            ui,
            cache: Cache::default(),
            cursor: Cursor::Available(Point::new(page.pointer_at.x, page.pointer_at.y)),
            device: gpu.device.clone(),
            pending: Vec::new(),
            queue: gpu.queue.clone(),
            renderer: FallbackRenderer::Primary(WgpuRenderer::new(
                gpu.engine.clone(),
                SANS,
                Pixels(14.0),
            )),
            scheduled: false,
            texture: gpu.texture(),
            theme: theme(builtin::skin()),
            pointer_at: page.pointer_at,
        }
    }

    fn draw(&mut self) -> Census {
        let before = self.ui.draw_pool_stats();
        self.app.tick();
        let bounds = Size::new(Consts::WIDTH, Consts::HEIGHT);
        let element = measure_block!(
            "iced.view",
            self.app.reads(|reads| tree::render(
                &self.ui.root,
                &self.ui,
                reads,
                &view::EMPTY,
                builtin::skin(),
                Clock::default(),
                None
            ))
        );
        let mut interface = measure_block!(
            "iced.build",
            UserInterface::build(
                element,
                bounds,
                mem::take(&mut self.cache),
                &mut self.renderer
            )
        );
        let mut messages: Vec<UiEvent> = Vec::new();
        self.scheduled = !self.pending.is_empty();
        let cursor = self.cursor;
        measure_block!("iced.update", {
            for event in mem::take(&mut self.pending) {
                let (state, _statuses) = interface.update(
                    slice::from_ref(&event),
                    cursor,
                    &mut self.renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                self.scheduled |= redraw_asked(&state);
            }
        });
        let base = self.theme.base();
        measure_block!("iced.draw", {
            interface.draw(
                &mut self.renderer,
                &self.theme,
                &Style {
                    text_color: base.text_color,
                },
                cursor,
            );
        });
        self.cache = interface.into_cache();
        for event in messages {
            self.app.update(event);
        }
        Census {
            scene: None,
            natives: None,
            pool: Pool::delta(&before, &self.ui.draw_pool_stats()),
            scheduled: self.scheduled,
        }
    }

    /// Submits the recorded frame. This is where the shader primitives iced
    /// stored during `draw` are prepared, run and composited: a harness that
    /// stops before this measures a visualiser that never ran.
    fn present(&mut self) {
        let background: Color = builtin::skin().palette.bg.into();
        let texture = self.texture.clone();
        let view = texture.create_view(&TextureViewDescriptor::default());
        let viewport = Viewport::with_physical_size(Size::new(width(), height()), 1.0);
        match &mut self.renderer {
            FallbackRenderer::Primary(wgpu) => {
                wgpu.present(
                    Some(background),
                    Harness::IMMEDIATE_FORMAT,
                    &view,
                    &viewport,
                );
            }
            FallbackRenderer::Secondary(_) => {
                panic!("the immediate page host must be built on the wgpu renderer")
            }
        }
    }
}

impl PageHost for Immediate {
    fn drawn_as(&self) -> &'static str {
        "pixels"
    }

    fn fenced_frame(&mut self) -> Census {
        let census = self.draw();
        measure_block!("iced.encode.gpu.fenced", {
            self.present();
            drain(&self.device);
        });
        census
    }

    fn frame(&mut self) -> Census {
        let census = self.draw();
        measure_block!("iced.encode", self.present());
        census
    }

    fn move_by(&mut self, dx: f32) -> f32 {
        self.pointer_at.x += dx;
        let position = Point::new(self.pointer_at.x, self.pointer_at.y);
        self.cursor = Cursor::Available(position);
        self.pending
            .push(Event::Mouse(MouseEvent::CursorMoved { position }));
        self.pointer_at.x
    }

    /// The pixels themselves: what this host hands the texture comes back the
    /// same bytes every time, so a byte that differs is something drawn
    /// differently.
    fn picture(&mut self) -> u64 {
        digest(&self.pixels())
    }

    fn pixels(&mut self) -> Vec<u8> {
        readback(&self.device, &self.queue, &self.texture)
    }

    fn place_pointer(&mut self) {
        let position = Point::new(self.pointer_at.x, self.pointer_at.y);
        self.cursor = Cursor::Available(position);
        self.pending
            .push(Event::Mouse(MouseEvent::CursorMoved { position }));
    }

    fn press(&mut self) {
        self.pending
            .push(Event::Mouse(MouseEvent::ButtonPressed(MouseButton::Left)));
    }

    fn published(&self) -> usize {
        self.app.published
    }

    fn reading(&self, endpoint: Option<&str>) -> Option<u64> {
        self.app.digest_of(endpoint?)
    }

    fn release(&mut self) {
        self.pending
            .push(Event::Mouse(MouseEvent::ButtonReleased(MouseButton::Left)));
    }

    fn wheel(&mut self, lines: f32) {
        self.pending.push(Event::Mouse(MouseEvent::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y: lines },
        }));
    }
}

/// Whether a widget asked iced's runtime to come back with another frame. This
/// is the closest the immediate host has to a seam at which it could decline to
/// draw, and it does not decline: it rebuilds and draws either way.
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
