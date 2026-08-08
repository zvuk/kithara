//! The embeddable layer: a UI that owns no window and no GPU device.
//!
//! A host that already has both — bevy, a plug-in shell, someone else's winit
//! loop — drives this directly: hand it a size, hand it input, take the scene,
//! draw the scene wherever you like. [`super::run`] is a thin window of its own
//! built on top of this, for an application that has no host to live in.

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

use super::neutral::{App, Config, RunError};
use crate::{
    compile::{CompiledUi, compile},
    draw::Rgba,
    interact::{Input, PointerPhase, ScrollAxis},
    render::{
        UiEvent, WindowCommand, document,
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
        let root = mount(&app, &config, &state, &ui, size, scale)?;
        Ok(Self {
            app,
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

    /// Takes what the document asked its window to do since the last call. A
    /// document that draws its own title bar asks to be dragged, minimised,
    /// maximised or closed this way; a host with no window can ignore it.
    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Tells the UI how big its rectangle is now, in physical pixels.
    pub fn resize(&mut self, size: (u32, u32), scale: f64) {
        self.size = PhysicalSize::new(size.0, size.1);
        self.scale = scale;
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
            | Input::ModifiersChanged(_) => {}
        }
        self.settle();
    }

    /// One typed keyboard or input-method event, in the toolkit's own words.
    ///
    /// Kept apart from [`Self::input`] because the neutral vocabulary borrows
    /// the text it carries, and the toolkit's own event owns it.
    pub fn text(&mut self, event: TextEvent) {
        if let Err(error) = self.root.handle_text_event(event) {
            tracing::error!(%error, "masonry text");
        }
        self.settle();
    }

    fn pointer_input(&mut self, event: PointerEvent) {
        if let Err(error) = self.root.handle_pointer_event(event) {
            tracing::error!(%error, "masonry pointer");
        }
    }

    /// Advances one frame's worth of animation.
    pub fn frame(&mut self, elapsed: Duration) {
        self.app.tick();
        if let Err(error) = self
            .root
            .handle_window_event(WindowEvent::AnimFrame(elapsed))
        {
            tracing::error!(%error, "masonry frame");
        }
        self.settle();
    }

    /// Draws the current document, in the physical pixels the caller sized it
    /// with. The caller rasterises this with its own device and queue, into
    /// whatever target it likes.
    ///
    /// The document is laid out and painted in logical units; the display scale
    /// is applied here so that a host only ever deals in the size it gave.
    ///
    /// # Errors
    /// Returns [`RunError`] when the paint pass fails.
    pub fn scene(&mut self) -> Result<Scene, RunError> {
        let (scene, _) = self
            .root
            .redraw()
            .map_err(|error| RunError::Host(error.to_string()))?;
        if (self.scale - 1.0).abs() < f64::EPSILON {
            return Ok(scene);
        }
        let mut scaled = Scene::new();
        scaled.append(&scene, Some(Affine::scale(self.scale)));
        Ok(scaled)
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
            let Self { app, root, ui, .. } = self;
            app.reads(|reads| root.refresh(ui, reads));
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
) -> Result<MasonryRoot<UiEvent>, RunError>
where
    Application: App,
{
    let node = app.reads(|reads| {
        let host = MasonryHost::new(ui, reads, config.skin).with_state(state.clone());
        document::render(&ui.root, ui, reads, config.skin_doc, host)
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
