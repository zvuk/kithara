use std::collections::BTreeMap;

use kithara_platform::{sync::Arc, time::Duration};
use masonry::{
    app::{RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::{CursorIcon, Handled, PointerEvent, TextEvent, WindowEvent},
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
use num_traits::cast::AsPrimitive;

use super::{
    frame::Frame,
    neutral::{App, Config, RunError},
};
use crate::{
    compile::{CompiledUi, compile},
    draw::{PoolStats, Rect, Rgba},
    error::UiDocError,
    ids::SourceUri,
    interact::{
        Input, PointerButton, PointerInput, PointerPhase, ScrollAxis, masonry::masonry_text_event,
    },
    masonry::retained::{MasonryHost, MasonryRoot, MasonryState},
    module::ViewSet,
    render::{
        Published, Reads, Skin, UiEvent, WindowCommand,
        custom::CustomKinds,
        document::{self, Clock, Ctx},
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
    config: Config<'config>,
    root: MasonryRoot<Published>,
    state: MasonryState,
    pointer: PhysicalPosition<f64>,
    buttons: BTreeMap<u64, PointerButtons>,
    size: PhysicalSize<u32>,
    screens: Screens,
    /// What every document of this host is compiled against, built once so
    /// that the draw pools inside it outlive a redress. Compiling one per
    /// rebuild would hand each new document an empty pool family and throw the
    /// filled one away with the document it came from.
    doc: UiConfig,
    commands: Vec<WindowCommand>,
    /// The state the shown screen keeps for itself. It is this host's, not the
    /// application's: nothing is asked for it and nothing declares it.
    view: ViewState,
    scale: f64,
    /// The document and the skin the standing tree was built from. The
    /// application turns to others whenever it likes — in answer to a press,
    /// on a tick, or between frames from outside — and the host follows by
    /// comparing against these.
    built_from: (String, String),
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
        let built_from = (app.document().to_owned(), app.skin().id().to_owned());
        Ok(Self {
            built_from,
            app,
            clock,
            config,
            doc,
            root,
            scale,
            screens,
            size,
            state,
            view,
            commands: Vec::new(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            buttons: BTreeMap::new(),
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

    /// Current allocation-reuse counters for this mounted document.
    #[must_use]
    pub fn draw_pool_stats(&self) -> PoolStats {
        self.screens.shown().draw_pool_stats()
    }

    /// Advances one frame's worth of animation.
    ///
    /// Advances the clock before ticking the app, so what this frame draws reflects the time this
    /// frame stands at rather than the previous one.
    pub fn frame(&mut self, elapsed: Duration) {
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

    /// The colour the control at `path` writes its text in right now.
    ///
    /// What a flag lights is a value the document reads rather than a shape it
    /// stands in, so a harness reads it back the same way it reads a rect: by
    /// the path the document gave the control.
    #[cfg(feature = "capture")]
    pub fn ink_of(&self, path: &str) -> Option<Rgba> {
        self.root.ink_of(self.visible_widget_id(path)?)
    }

    /// Feeds one neutral input event in. Whatever the document publishes in
    /// response is applied to the application before this returns.
    pub fn input(&mut self, input: Input<'_>) -> Handled {
        let handled = match input {
            Input::Pointer(pointer) => {
                if let Some(at) = pointer.at {
                    self.pointer = PhysicalPosition::new(
                        f64::from(at.x) * self.scale,
                        f64::from(at.y) * self.scale,
                    );
                }
                self.pointer_event(pointer)
                    .map_or(Handled::No, |event| self.pointer_input(event))
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
                    delta,
                    pointer: pointer_info(PointerId::PRIMARY),
                    state: pointer_state(self.pointer, PointerButtons::new(), self.scale, 1),
                }))
            }
            Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::InputMethod(_)
            | Input::ModifiersChanged(_) => {
                masonry_text_event(input).map_or(Handled::No, |event| self.text(event))
            }
        };
        self.settle();
        handled
    }

    /// The pointer leaving the window ends every hover beneath it; without this the control it left
    /// would keep drawing itself lit.
    fn pointer_event(&mut self, input: PointerInput) -> Option<PointerEvent> {
        let at = self.pointer;
        let scale = self.scale;
        let pointer = pointer_info(PointerId::new(input.id.0.checked_add(1)?)?);
        let button = pointer_button(input.button);
        let mut buttons = self.buttons.get(&input.id.0).copied().unwrap_or_default();
        match input.phase {
            PointerPhase::Down => {
                if let Some(button) = button {
                    buttons.insert(button);
                }
                self.buttons.insert(input.id.0, buttons);
            }
            PointerPhase::Up | PointerPhase::DoubleClick => {
                if let Some(button) = button {
                    buttons.remove(button);
                }
                if buttons.is_empty() {
                    self.buttons.remove(&input.id.0);
                } else {
                    self.buttons.insert(input.id.0, buttons);
                }
            }
            PointerPhase::Cancel => {
                self.buttons.remove(&input.id.0);
            }
            _ => {}
        }
        match input.phase {
            PointerPhase::Down => Some(PointerEvent::Down(PointerButtonEvent {
                button,
                pointer,
                state: pointer_state(at, buttons, scale, input.clicks),
            })),
            PointerPhase::Up | PointerPhase::DoubleClick => {
                Some(PointerEvent::Up(PointerButtonEvent {
                    button,
                    pointer,
                    state: pointer_state(at, buttons, scale, input.clicks),
                }))
            }
            PointerPhase::Move | PointerPhase::MoveLongPress => {
                Some(PointerEvent::Move(PointerUpdate {
                    pointer,
                    current: pointer_state(at, buttons, scale, input.clicks),
                    coalesced: Vec::new(),
                    predicted: Vec::new(),
                }))
            }
            PointerPhase::Leave => Some(PointerEvent::Leave(pointer)),
            PointerPhase::Cancel => Some(PointerEvent::Cancel(pointer)),
            PointerPhase::LongPress => None,
        }
    }

    fn pointer_input(&mut self, event: PointerEvent) -> Handled {
        self.root
            .handle_pointer_event(event)
            .unwrap_or_else(|error| {
                tracing::error!(%error, "masonry pointer");
                Handled::Yes
            })
    }

    /// Resolves one control's document path to the rect its layout gave it, in
    /// the same logical units [`Self::input`] takes a point in.
    ///
    /// A harness names a control by its path and acts at the rect this
    /// returns instead of computing a pixel by hand: a scenario clicks it,
    /// and a capture photographs it. A control a hidden block stashed takes
    /// an empty box.
    pub fn rect_of(&self, path: &str) -> Option<Rect> {
        let id = self.visible_widget_id(path)?;
        let widget = self.root.root().get_widget(id)?;
        let bounds = widget.ctx().bounding_rect();
        let corner = Rect {
            x: bounds.x0.as_(),
            y: bounds.y0.as_(),
            w: 0.0,
            h: 0.0,
        };
        if widget.ctx().is_stashed() {
            return Some(corner);
        }
        Some(Rect {
            w: (bounds.x1 - bounds.x0).as_(),
            h: (bounds.y1 - bounds.y0).as_(),
            ..corner
        })
    }

    /// The widget a path names on screen right now.
    ///
    /// The branches of an adaptive block share the paths of the controls they
    /// both hold, and only the branch the room reaches stands; the others are
    /// stashed. A path whose every widget is stashed names the last one
    /// mounted, as it did before.
    fn visible_widget_id(&self, path: &str) -> Option<masonry::core::WidgetId> {
        self.state
            .widget_ids(path)
            .into_iter()
            .rev()
            .find(|id| {
                self.root
                    .root()
                    .get_widget(*id)
                    .is_some_and(|widget| !widget.ctx().is_stashed())
            })
            .or_else(|| self.state.widget_id(path))
    }

    /// Draws the current document, in the physical pixels the caller sized it
    /// with. The caller prepares [`crate::masonry::shader::ShaderPass`], binds
    /// every [`Frame::external_images`] declaration to its producer's texture,
    /// rasterises the Vello scene, then sends native declarations through
    /// [`crate::masonry::vis::VisPass`] on the same target.
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
        let external_images = self.root.external_images();
        let scene = if (self.scale - 1.0).abs() < f64::EPSILON {
            scene
        } else {
            let mut scaled = Scene::new();
            scaled.append(&scene, Some(Affine::scale(self.scale)));
            scaled
        };
        Ok(Frame::new(scene, external_images, shaders, vis))
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

    /// Draws the current document through the same single paint path as
    /// [`Self::render`] and returns only its Vello scene.
    ///
    /// # Errors
    /// Returns [`RunError`] when the paint pass fails.
    pub fn scene(&mut self) -> Result<Scene, RunError> {
        self.render().map(Into::into)
    }

    /// Takes what the document asked its window to do since the last call. A
    /// document that draws its own title bar asks to be dragged, minimised,
    /// maximised or closed this way; a host with no window can ignore it.
    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    fn text(&mut self, event: TextEvent) -> Handled {
        self.root.handle_text_event(event).unwrap_or_else(|error| {
            tracing::error!(%error, "masonry text");
            Handled::Yes
        })
    }

    delegate::delegate! {
        to self.root {
            /// Satisfies the redraw signals covered by a frame the host completed.
            ///
            /// Returns whether another frame should follow. Every unrelated
            /// platform signal remains queued on the retained root.
            pub fn complete_frame(&mut self) -> bool;
            /// Whether the widget holding the keyboard takes text input, so a
            /// paste the window reads belongs to it.
            pub fn editing_text(&self) -> bool;
            /// Takes requests for IME, clipboard and other external host actions.
            pub fn take_platform_signals(&mut self) -> Vec<RenderRootSignal>;
            /// Reports whether the next retained frame would change the picture.
            pub fn needs_frame(&self) -> bool;
            /// Takes the cursor the document last asked its window to show.
            ///
            /// A host that owns a window applies it; a host without one can
            /// drop it, but takes it either way so the queue stays bounded.
            pub fn take_cursor(&mut self) -> Option<CursorIcon>;
        }
    }
}

/// How the screen this host shows follows the application and the state the
/// document keeps for itself.
impl<Application> Ui<'_, Application>
where
    Application: App,
{
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
        self.mount_shown()
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

    /// The state the shown screen keeps for itself.
    ///
    /// An application may read what a document turned for itself without
    /// having declared, answered, or been asked for any of it.
    #[must_use]
    pub const fn view(&self) -> &ViewState {
        &self.view
    }

    /// Mounts the screen the cache is showing, in place of the tree standing.
    ///
    /// Drops state the newly shown screen does not name, rather than keeping it to answer for a
    /// state this document does not have.
    fn mount_shown(&mut self) -> Result<(), RunError> {
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

    /// Compiles the application's document again and mounts what it produced,
    /// in place of what this host is showing.
    ///
    /// The tree a host mounts is retained: its shape is settled where it is
    /// built, so a document that now compiles to another shape - because the
    /// application moved on, or because another skin measures it differently -
    /// reaches the screen only by being built again.
    fn remount(&mut self) -> Result<(), RunError> {
        // Taken before compiling, so a document that fails reports once per turn
        // rather than once per frame.
        self.built_from = (
            self.app.document().to_owned(),
            self.app.skin().id().to_owned(),
        );
        let ui = compile_document(&self.app, &self.config, &self.doc, &self.view)?;
        self.screens.reset(ui);
        self.mount_shown()
    }

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
    ///
    /// The application may have turned without the document publishing
    /// anything, so the turn is looked for on every call, not only after an
    /// action.
    fn settle(&mut self) {
        let actions = self.root.take_actions();
        let acted = !actions.is_empty();
        for published in actions {
            let Self {
                app, screens, view, ..
            } = self;
            let Some(event) =
                app.reads(|reads| screens.shown().views().settle(published, reads, view))
            else {
                continue;
            };
            if let UiEvent::Window(command) = event {
                self.commands.push(command);
            }
            self.app.update(event);
        }
        if self.app.document() == self.built_from.0 && self.app.skin().id() == self.built_from.1 {
            if !acted {
                return;
            }
            match self.turn() {
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
) -> Result<MasonryRoot<Published>, RunError>
where
    Application: App,
{
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
        size,
        scale_factor,
        default_properties: Arc::new(default_property_set()),
        use_system_fonts: false,
        size_policy: WindowSizePolicy::User,
        test_font: None,
    }
}

fn pointer_info(id: PointerId) -> PointerInfo {
    PointerInfo {
        pointer_id: Some(id),
        persistent_device_id: None,
        pointer_type: if id.is_primary_pointer() {
            PointerType::Mouse
        } else {
            PointerType::Unknown
        },
    }
}

fn pointer_button(button: Option<PointerButton>) -> Option<MasonryPointerButton> {
    match button {
        None | Some(PointerButton::Primary) => Some(MasonryPointerButton::Primary),
        Some(PointerButton::Secondary) => Some(MasonryPointerButton::Secondary),
        Some(PointerButton::Auxiliary) => Some(MasonryPointerButton::Auxiliary),
        Some(PointerButton::Back) => Some(MasonryPointerButton::X1),
        Some(PointerButton::Forward) => Some(MasonryPointerButton::X2),
        Some(PointerButton::Other(_)) => None,
    }
}

fn pointer_state(
    at: PhysicalPosition<f64>,
    buttons: PointerButtons,
    scale: f64,
    clicks: u8,
) -> PointerState {
    PointerState {
        position: at,
        buttons,
        count: clicks.max(1),
        scale_factor: scale,
        ..PointerState::default()
    }
}
