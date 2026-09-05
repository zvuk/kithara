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
    core::{CursorIcon, PointerEvent, TextEvent, WindowEvent},
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
#[cfg(any(test, feature = "capture"))]
use num_traits::cast::AsPrimitive;

use super::{
    frame::Frame,
    neutral::{App, Config, RunError},
};
#[cfg(any(test, feature = "capture"))]
use crate::draw::Rect;
use crate::{
    compile::{CompiledUi, compile},
    draw::{PoolStats, Rgba},
    error::UiDocError,
    ids::SourceUri,
    interact::{Input, PointerPhase, ScrollAxis, masonry::masonry_text_event},
    module::ViewSet,
    render::{
        ControlAction, Reads, Skin, UiEvent, WindowCommand,
        custom::CustomKinds,
        document,
        document::{Clock, Ctx},
        masonry::{MasonryHost, MasonryRoot, MasonryState},
    },
    source::UiConfig,
    view::{Screens, ViewState},
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
    /// What every document of this host is compiled against, built once so
    /// that the draw pools inside it outlive a redress. Compiling one per
    /// rebuild would hand each new document an empty pool family and throw the
    /// filled one away with the document it came from.
    doc: UiConfig,
    pointer: PhysicalPosition<f64>,
    root: MasonryRoot<UiEvent>,
    scale: f64,
    size: PhysicalSize<u32>,
    screens: Screens,
    state: MasonryState,
    /// The state the shown screen keeps for itself. It is this host's, not the
    /// application's: nothing is asked for it and nothing declares it.
    view: ViewState,
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
        let mut doc = config.settings.cloned().unwrap_or_default();
        doc.custom_kinds = config.kinds.map(CustomKinds::names).unwrap_or_default();
        let clock = Clock::default();
        let view = ViewState::default();
        let ui = compile_document(&app, &config, &doc, &view)?;
        let root = mount(
            &app,
            &config,
            &state,
            &ui,
            root_options(size, scale),
            clock,
            &view,
        )?;
        let screens = Screens::new(doc.screen_cache, ui);
        Ok(Self {
            app,
            clock,
            commands: Vec::new(),
            config,
            doc,
            pointer: PhysicalPosition::new(0.0, 0.0),
            root,
            scale,
            screens,
            size,
            state,
            view,
        })
    }

    /// The page a target that has nothing behind it is cleared to.
    ///
    /// A document paints its own panels and leaves the rest of the rectangle to
    /// whoever owns it. A window leaves it to the desktop it stands on, which
    /// is what makes a rounded corner a corner at all; a capture has no desktop
    /// behind it, so it stands the document on the skin's own page instead.
    pub fn background(&self) -> Rgba {
        let skin = self.app.skin();
        skin.rgba(skin.layout.page_background)
    }

    /// The state the shown screen keeps for itself.
    ///
    /// An application may read what a document turned for itself without
    /// having declared, answered, or been asked for any of it.
    #[must_use]
    pub const fn view(&self) -> &ViewState {
        &self.view
    }

    /// Stands the screen's own state at one page, and shows that page.
    ///
    /// A page named here comes from the application rather than from a
    /// document, so it is checked against what the shown screen offers before
    /// anything moves: a name no screen offers leaves the screen where it is.
    ///
    /// # Errors
    /// Returns [`UiDocError::UnknownPage`] when the shown screen turns no such
    /// state, or turns it between pages that do not include `page`, and
    /// whatever compiling the page it does offer fails with.
    pub fn stand(&mut self, state: &str, page: &str) -> Result<(), RunError> {
        let views = self.screens.shown().views();
        if !views
            .pages()
            .get(state)
            .is_some_and(|at| at.offered.contains(page))
        {
            return Err(UiDocError::UnknownPage {
                origin: SourceUri(self.app.document().to_owned()),
                id: state.to_owned(),
                page: page.to_owned(),
                path: String::new(),
            }
            .into());
        }
        self.view.stand(state, page);
        self.turn().map(|_| ())
    }

    /// Turns one flag the screen keeps for itself, and shows what that changed.
    ///
    /// A flag turned here comes from the application rather than from a press,
    /// which is how a harness opens the surface a page is about before
    /// photographing it. The name is checked against what the shown screen
    /// names, so a state no document wrote leaves the screen where it is.
    ///
    /// # Errors
    /// Returns [`UiDocError::UnknownState`] when the shown screen names no such
    /// state, and whatever compiling the screen it does show fails with.
    pub fn set(&mut self, state: &str, set: ViewSet) -> Result<(), RunError> {
        if !self.screens.shown().views().named().contains(state) {
            return Err(UiDocError::UnknownState {
                origin: SourceUri(self.app.document().to_owned()),
                id: state.to_owned(),
            }
            .into());
        }
        if !self.view.set(state, set) {
            return Ok(());
        }
        if self.turn()? {
            return Ok(());
        }
        // The flag turned no page, so the screen standing is the one already
        // compiled. It is mounted again all the same, because what a flag
        // lights - a group's background, a glyph's colour - is settled where
        // the tree is built, which is the same reason standing at a page
        // mounts one.
        self.mount_shown()
    }

    /// Current allocation-reuse counters for this mounted document.
    #[must_use]
    pub fn draw_pool_stats(&self) -> PoolStats {
        self.screens.shown().draw_pool_stats()
    }

    /// Resolves one control's document path to the rect its layout gave it, in
    /// the same logical units [`Self::input`] takes a point in.
    ///
    /// A harness names a control by its path and acts at the rect this
    /// returns instead of computing a pixel by hand: a scenario clicks it,
    /// and a capture photographs it.
    #[cfg(any(test, feature = "capture"))]
    pub fn rect_of(&self, path: &str) -> Option<Rect> {
        let id = self.state.widget_id(path)?;
        let bounds = self.root.root().get_widget(id)?.ctx().bounding_rect();
        Some(Rect {
            x: bounds.x0.as_(),
            y: bounds.y0.as_(),
            w: (bounds.x1 - bounds.x0).as_(),
            h: (bounds.y1 - bounds.y0).as_(),
        })
    }

    /// The colour the control at `path` writes its text in right now.
    ///
    /// What a flag lights is a value the document reads rather than a shape it
    /// stands in, so a harness reads it back the same way it reads a rect: by
    /// the path the document gave the control.
    #[cfg(any(test, feature = "capture"))]
    pub fn ink_of(&self, path: &str) -> Option<Rgba> {
        self.root.ink_of(self.state.widget_id(path)?)
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
            /// Takes the cursor the document last asked its window to show.
            ///
            /// A host that owns a window applies it; a host without one can
            /// drop it, but takes it either way so the queue stays bounded.
            pub fn take_cursor(&mut self) -> Option<CursorIcon>;
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
            screens,
            config,
            view,
            ..
        } = self;
        let clock = *clock;
        app.reads(|reads| {
            root.refresh(frame_ctx(
                screens.shown(),
                reads,
                view,
                app.skin(),
                config,
                clock,
            ));
        });
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
}

/// How the screen this host shows follows the application and the state the
/// document keeps for itself.
impl<Application> Ui<'_, Application>
where
    Application: App,
{
    /// Hands the application what the document published, then shows the new
    /// state.
    ///
    /// Showing it re-reads the mounted document's endpoints in place. A rebuild
    /// would replace the whole widget tree, which discards the gesture a control
    /// is in the middle of, the pointer capture that feeds it, and the run of
    /// clicks a double click is made of. The tree is only rebuilt when the
    /// application turns to another document or another skin, the two cases
    /// where its shape really did change — and only then is the document
    /// compiled again. A hand on a knob publishes an action for every step it moves, and
    /// compiling the page for each of them costs more than drawing it.
    fn settle(&mut self) {
        let actions = self.root.take_actions();
        if actions.is_empty() {
            return;
        }
        let was_document = self.app.document().to_owned();
        let was_skin = self.app.skin().id().to_owned();
        for event in actions {
            // A press that turns the screen's own state is answered here, by
            // the host that owns it. The application is told all the same: what
            // the document turns for itself is not hidden from it.
            if let UiEvent::Control { path, action } = &event
                && matches!(action, ControlAction::Activate)
                && let Some((state, write)) = self.screens.shown().views().at(path)
            {
                self.view.apply(state, write);
            }
            if let UiEvent::Window(command) = event {
                self.commands.push(command);
            }
            self.app.update(event);
        }
        if self.app.document() == was_document && self.app.skin().id() == was_skin {
            match self.turn() {
                // The shape on screen is the one the pages already standing
                // compile to, so the mounted tree is read again in place.
                Ok(false) => {
                    let Self {
                        app,
                        clock,
                        root,
                        screens,
                        config,
                        view,
                        ..
                    } = self;
                    let clock = *clock;
                    app.reads(|reads| {
                        root.refresh(frame_ctx(
                            screens.shown(),
                            reads,
                            view,
                            app.skin(),
                            config,
                            clock,
                        ));
                    });
                }
                Ok(true) => {}
                Err(error) => {
                    tracing::error!(%error, "page did not follow the state that turns it");
                }
            }
            return;
        }
        if let Err(error) = self.remount() {
            tracing::error!(%error, "document did not follow its application");
        }
    }

    /// Shows the page the screen's own state now stands at, and answers whether
    /// the shape on screen changed.
    ///
    /// A page is a document of its own, so turning to one is a tree built
    /// again rather than a tree read again. What was shown is kept: turning
    /// back to it costs no compile at all.
    fn turn(&mut self) -> Result<bool, RunError> {
        let Self {
            app,
            config,
            doc,
            screens,
            view,
            ..
        } = self;
        if !screens.show(view, || compile_document(app, config, doc, view))? {
            return Ok(false);
        }
        self.mount_shown().map(|()| true)
    }

    /// Compiles the application's document again and mounts what it produced,
    /// in place of what this host is showing.
    ///
    /// The tree a host mounts is retained: its shape is settled where it is
    /// built, so a document that now compiles to another shape - because the
    /// application moved on, or because another skin measures it differently -
    /// reaches the screen only by being built again.
    fn remount(&mut self) -> Result<(), RunError> {
        let ui = compile_document(&self.app, &self.config, &self.doc, &self.view)?;
        self.screens.reset(ui);
        self.mount_shown()
    }

    /// Mounts the screen the cache is showing, in place of the tree standing.
    fn mount_shown(&mut self) -> Result<(), RunError> {
        // A state belongs to the document that named it. What the screen now
        // shown does not name is gone rather than kept to answer for a state
        // this document does not have.
        self.view.retain(self.screens.shown().views().named());
        self.app.turned(&self.view);
        self.root = mount(
            &self.app,
            &self.config,
            &self.state,
            self.screens.shown(),
            root_options(self.size, self.scale),
            self.clock,
            &self.view,
        )?;
        self.root
            .handle_window_event(WindowEvent::Resize(self.size))
            .map(|_| ())
            .map_err(|error| RunError::Host(error.to_string()))
    }
}

fn compile_document<Application>(
    app: &Application,
    config: &Config<'_>,
    doc: &UiConfig,
    view: &ViewState,
) -> Result<CompiledUi, RunError>
where
    Application: App,
{
    Ok(compile(
        app.document(),
        config.resolver,
        config.endpoints,
        app.skin().document(),
        config.text,
        doc,
        view,
    )?)
}

/// The frame context both this host's passes read, carrying whatever the
/// application registered.
fn frame_ctx<'a, 'r>(
    ui: &'a CompiledUi,
    reads: &'r dyn Reads,
    view: &'r ViewState,
    skin: &'a Skin,
    config: &Config<'a>,
    clock: Clock,
) -> Ctx<'a, 'r> {
    let ctx = Ctx::new(ui, reads, view, skin.document(), clock);
    config.kinds.map_or(ctx, |kinds| ctx.with_kinds(kinds))
}

fn mount<Application>(
    app: &Application,
    config: &Config<'_>,
    state: &MasonryState,
    ui: &CompiledUi,
    options: RenderRootOptions,
    clock: Clock,
    view: &ViewState,
) -> Result<MasonryRoot<UiEvent>, RunError>
where
    Application: App,
{
    #[cfg(any(test, feature = "capture"))]
    state.clear_paths();
    let skin = app.skin();
    let node = app.reads(|reads| {
        let ctx = frame_ctx(ui, reads, view, skin, config, clock);
        let host = MasonryHost::new(ctx, skin).with_state(state.clone());
        document::render(&ui.root, ctx, host)
    });
    MasonryRoot::new(node, options)
        .map(|root| root.with_animates(ui.animates))
        .map_err(|error| RunError::Host(error.to_string()))
}

fn root_options(size: PhysicalSize<u32>, scale_factor: f64) -> RenderRootOptions {
    RenderRootOptions {
        default_properties: Arc::new(default_property_set()),
        use_system_fonts: false,
        size_policy: WindowSizePolicy::User,
        size,
        scale_factor,
        test_font: None,
    }
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
