//! The embeddable layer: a UI that owns no window and no GPU device.
//!
//! A host that already has both — bevy, a plug-in shell, someone else's winit
//! loop - drives this directly: hand it a size, hand it input, take the complete
//! frame, prepare its shader images, draw its Vello scene and then its native
//! effects. [`super::run`] is a
//! thin window of its own built on top of this, for an application that has no
//! host to live in.

use kithara_platform::{sync::Arc, time::Duration};
use masonry::{
    app::{RenderRootOptions, WindowSizePolicy},
    core::{PointerEvent, TextEvent, WindowEvent},
    dpi::{PhysicalPosition, PhysicalSize},
    kurbo::Affine,
    theme::default_property_set,
    ui_events::{
        ScrollDelta,
        pointer::{
            PointerButton as MasonryPointerButton, PointerButtonEvent, PointerButtons, PointerId,
            PointerInfo, PointerScrollEvent, PointerState, PointerType, PointerUpdate,
        },
    },
    vello::Scene,
};
#[cfg(test)]
use num_traits::cast::AsPrimitive;

use super::{
    frame::Frame,
    neutral::{App, Config, RunError},
};
#[cfg(test)]
use crate::draw::Rect;
use crate::{
    compile::{CompiledUi, compile},
    draw::{PoolStats, Rgba},
    interact::{Input, PointerPhase, ScrollAxis, masonry::masonry_text_event},
    render::{
        UiEvent, WindowCommand, document,
        document::{Clock, Ctx},
        masonry::{MasonryHost, MasonryRoot, MasonryState},
    },
    source::UiConfig,
};

/// One mounted document, driven by whoever owns the window.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Ui<'config, Application> {
    /// The application this UI is showing.
    #[field(get, vis = "pub")]
    app: Application,
    /// This host's own reading of time, advanced once per frame so a document
    /// bound to it animates without the application having to keep a timer.
    #[field(get(copy), vis = "pub")]
    clock: Clock,
    commands: Vec<WindowCommand>,
    config: Config<'config>,
    pointer: PhysicalPosition<f64>,
    root: MasonryRoot<UiEvent>,
    scale: f64,
    size: PhysicalSize<u32>,
    state: MasonryState,
    ui: CompiledUi,
}

impl<'config, Application> Ui<'config, Application>
where
    Application: App,
{
    /// Compiles the application's document and mounts it at the given physical
    /// size and display scale.
    ///
    /// # Errors
    /// Returns [`RunError`] when the document does not compile or does not mount.
    pub fn new(
        app: Application,
        config: Config<'config>,
        size: (u32, u32),
        scale: f64,
    ) -> Result<Self, RunError> {
        let size = PhysicalSize::new(size.0, size.1);
        let state = MasonryState::default();
        let ui = compile_document(&app, &config)?;
        let clock = Clock::default();
        let root = mount(&app, &config, &state, &ui, size, scale, clock)?;
        Ok(Self {
            app,
            clock,
            commands: Vec::new(),
            config,
            pointer: PhysicalPosition::new(0.0, 0.0),
            root,
            scale,
            size,
            state,
            ui,
        })
    }

    /// What to clear the target to before the scene is painted onto it.
    ///
    /// A document paints its own panels and leaves the rest of the rectangle to
    /// whoever owns it. That bare part is the page, and it is the skin's, not
    /// the target's: clearing to anything else shows through wherever a
    /// document does not reach, and the other host does not have that seam.
    pub const fn background(&self) -> Rgba {
        self.config.skin.palette.bg
    }

    /// Current allocation-reuse counters for this mounted document.
    #[must_use]
    pub fn draw_pool_stats(&self) -> PoolStats {
        self.ui.draw_pool_stats()
    }

    /// Resolves one control's document path to the rect its layout gave it, in
    /// the same logical units [`Self::input`] takes a point in.
    ///
    /// Test-only: a scenario harness names a control by its path and acts at
    /// the rect this returns, instead of computing a pixel by hand.
    #[cfg(test)]
    pub(crate) fn rect_of(&self, path: &str) -> Option<Rect> {
        let id = self.state.widget_id(path)?;
        let bounds = self.root.root().get_widget(id)?.ctx().bounding_rect();
        Some(Rect {
            x: bounds.x0.as_(),
            y: bounds.y0.as_(),
            w: (bounds.x1 - bounds.x0).as_(),
            h: (bounds.y1 - bounds.y0).as_(),
        })
    }

    /// Takes what the document asked its window to do since the last call. A
    /// document that draws its own title bar asks to be dragged, minimised,
    /// maximised or closed this way; a host with no window can ignore it.
    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    delegate::delegate! {
        to self.root {
            /// Satisfies the redraw signals covered by a frame the host completed.
            ///
            /// Returns whether another frame should follow. Every unrelated
            /// platform signal remains queued on the retained root.
            pub fn complete_frame(&mut self) -> bool;
            /// Reports whether the next retained frame would change the picture.
            pub fn needs_frame(&self) -> bool;
        }
    }

    /// Tells the UI how big its rectangle is now, in physical pixels.
    pub fn resize(&mut self, size: (u32, u32), scale: f64) {
        let scale_changed = self.scale != scale;
        self.size = PhysicalSize::new(size.0, size.1);
        self.scale = scale;
        if scale_changed
            && let Err(error) = self
                .root
                .handle_window_event(WindowEvent::Rescale(self.scale))
        {
            tracing::error!(%error, "masonry rescale");
        }
        if let Err(error) = self
            .root
            .handle_window_event(WindowEvent::Resize(self.size))
        {
            tracing::error!(%error, "masonry resize");
        }
    }

    /// Feeds one neutral input event in. Whatever the document publishes in
    /// response is applied to the application before this returns.
    pub fn input(&mut self, input: Input<'_>) {
        match input {
            Input::Pointer(pointer) => {
                if let Some(at) = pointer.at {
                    self.pointer = PhysicalPosition::new(
                        f64::from(at.x) * self.scale,
                        f64::from(at.y) * self.scale,
                    );
                }
                if let Some(event) = self.pointer_event(pointer.phase, pointer.clicks) {
                    self.pointer_input(event);
                }
            }
            Input::Wheel(scroll) => {
                let (x, y) = (
                    scroll.delta(ScrollAxis::Horizontal),
                    scroll.delta(ScrollAxis::Vertical),
                );
                let delta = if scroll.is_pixels() {
                    ScrollDelta::PixelDelta(PhysicalPosition::new(x.into(), y.into()))
                } else {
                    ScrollDelta::LineDelta(x, y)
                };
                self.pointer_input(PointerEvent::Scroll(PointerScrollEvent {
                    pointer: pointer_info(),
                    delta,
                    state: pointer_state(self.pointer, false, self.scale, 1),
                }));
            }
            Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::InputMethod(_)
            | Input::ModifiersChanged(_) => {
                if let Some(event) = masonry_text_event(input) {
                    self.text(event);
                }
            }
        }
        self.settle();
    }

    fn text(&mut self, event: TextEvent) {
        if let Err(error) = self.root.handle_text_event(event) {
            tracing::error!(%error, "masonry text");
        }
    }

    fn pointer_input(&mut self, event: PointerEvent) {
        if let Err(error) = self.root.handle_pointer_event(event) {
            tracing::error!(%error, "masonry pointer");
        }
    }

    /// Advances one frame's worth of animation.
    pub fn frame(&mut self, elapsed: Duration) {
        // Before the refresh, so what this frame draws is the time this frame
        // stands at rather than the one before it.
        self.clock = self.clock.advance(elapsed);
        self.app.tick();
        let Self {
            app,
            clock,
            root,
            ui,
            config,
            ..
        } = self;
        let clock = *clock;
        app.reads(|reads| root.refresh(Ctx::new(ui, reads, config.skin_doc, clock)));
        if let Err(error) = self
            .root
            .handle_window_event(WindowEvent::AnimFrame(elapsed))
        {
            tracing::error!(%error, "masonry frame");
        }
        self.settle();
    }

    /// Draws the current document, in the physical pixels the caller sized it
    /// with. The caller prepares [`crate::render::shader::ShaderPass`],
    /// rasterises the Vello scene, then sends native declarations through
    /// [`crate::render::vis::VisPass`] on the same target.
    ///
    /// The document is laid out and painted in logical units. This method scales
    /// the Vello scene; the host gives that same scale and its physical target
    /// size to the native pass.
    ///
    /// # Errors
    /// Returns [`RunError`] when the paint pass fails.
    pub fn render(&mut self) -> Result<Frame, RunError> {
        let (scene, _) = self
            .root
            .redraw()
            .map_err(|error| RunError::Host(error.to_string()))?;
        let shaders = self.root.shader_declarations();
        let vis = self.root.vis_declarations();
        let scene = if (self.scale - 1.0).abs() < f64::EPSILON {
            scene
        } else {
            let mut scaled = Scene::new();
            scaled.append(&scene, Some(Affine::scale(self.scale)));
            scaled
        };
        Ok(Frame::new(scene, shaders, vis))
    }

    /// Draws the current document through the same single paint path as
    /// [`Self::render`] and returns only its Vello scene.
    ///
    /// # Errors
    /// Returns [`RunError`] when the paint pass fails.
    pub fn scene(&mut self) -> Result<Scene, RunError> {
        self.render().map(Into::into)
    }

    fn pointer_event(&self, phase: PointerPhase, clicks: u8) -> Option<PointerEvent> {
        let at = self.pointer;
        let scale = self.scale;
        match phase {
            PointerPhase::Down => Some(PointerEvent::Down(PointerButtonEvent {
                button: Some(MasonryPointerButton::Primary),
                pointer: pointer_info(),
                state: pointer_state(at, true, scale, clicks),
            })),
            PointerPhase::Up => Some(PointerEvent::Up(PointerButtonEvent {
                button: Some(MasonryPointerButton::Primary),
                pointer: pointer_info(),
                state: pointer_state(at, false, scale, clicks),
            })),
            PointerPhase::Move => Some(PointerEvent::Move(PointerUpdate {
                pointer: pointer_info(),
                current: pointer_state(at, false, scale, clicks),
                coalesced: Vec::new(),
                predicted: Vec::new(),
            })),
            // The hand leaving the window ends every hover under it; without
            // this the control it left keeps drawing itself lit.
            PointerPhase::Leave => Some(PointerEvent::Leave(pointer_info())),
            PointerPhase::Cancel
            | PointerPhase::DoubleClick
            | PointerPhase::LongPress
            | PointerPhase::MoveLongPress => None,
        }
    }

    /// Hands the application what the document published, then shows the new
    /// state.
    ///
    /// Showing it re-reads the mounted document's endpoints in place. A rebuild
    /// would replace the whole widget tree, which discards the gesture a control
    /// is in the middle of, the pointer capture that feeds it, and the run of
    /// clicks a double click is made of. The tree is only rebuilt when the
    /// application turns to a different document, which is the one case where
    /// its shape really did change — and only then is the document compiled
    /// again. A hand on a knob publishes an action for every step it moves, and
    /// compiling the page for each of them costs more than drawing it.
    fn settle(&mut self) {
        let actions = self.root.take_actions();
        if actions.is_empty() {
            return;
        }
        let was = self.app.document().to_owned();
        for event in actions {
            if let UiEvent::Window(command) = event {
                self.commands.push(command);
            }
            self.app.update(event);
        }
        if self.app.document() == was {
            let Self {
                app,
                clock,
                root,
                ui,
                config,
                ..
            } = self;
            let clock = *clock;
            app.reads(|reads| root.refresh(Ctx::new(ui, reads, config.skin_doc, clock)));
            return;
        }
        let Ok(ui) = compile_document(&self.app, &self.config)
            .inspect_err(|error| tracing::error!(%error, "document did not compile"))
        else {
            return;
        };
        let Ok(root) = mount(
            &self.app,
            &self.config,
            &self.state,
            &ui,
            self.size,
            self.scale,
            self.clock,
        )
        .inspect_err(|error| tracing::error!(%error, "document did not mount")) else {
            return;
        };
        self.root = root;
        self.ui = ui;
        if let Err(error) = self
            .root
            .handle_window_event(WindowEvent::Resize(self.size))
        {
            tracing::error!(%error, "masonry resize");
        }
    }
}

fn compile_document<Application>(
    app: &Application,
    config: &Config<'_>,
) -> Result<CompiledUi, RunError>
where
    Application: App,
{
    Ok(compile(
        app.document(),
        config.resolver,
        config.endpoints,
        config.skin_doc,
        config.text,
        &UiConfig::default(),
    )?)
}

fn mount<Application>(
    app: &Application,
    config: &Config<'_>,
    state: &MasonryState,
    ui: &CompiledUi,
    size: PhysicalSize<u32>,
    scale: f64,
    clock: Clock,
) -> Result<MasonryRoot<UiEvent>, RunError>
where
    Application: App,
{
    #[cfg(test)]
    state.clear_paths();
    let node = app.reads(|reads| {
        let ctx = Ctx::new(ui, reads, config.skin_doc, clock);
        let host = MasonryHost::new(ctx, config.skin).with_state(state.clone());
        document::render(&ui.root, ctx, host)
    });
    MasonryRoot::new(
        node,
        RenderRootOptions {
            default_properties: Arc::new(default_property_set()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size,
            scale_factor: scale,
            test_font: None,
        },
    )
    .map(|root| root.with_animates(ui.animates))
    .map_err(|error| RunError::Host(error.to_string()))
}

fn pointer_info() -> PointerInfo {
    PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    }
}

fn pointer_state(at: PhysicalPosition<f64>, pressed: bool, scale: f64, clicks: u8) -> PointerState {
    let mut buttons = PointerButtons::new();
    if pressed {
        buttons.insert(MasonryPointerButton::Primary);
    }
    PointerState {
        position: at,
        buttons,
        count: clicks.max(1),
        scale_factor: scale,
        ..PointerState::default()
    }
}
