//! The convenience layer: a window of our own for an application that has no
//! host to live in. Everything it does, it does through [`Ui`].

use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant, WallInstant},
};
use masonry::vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    util::{RenderContext, RenderSurface},
    wgpu::{CommandEncoderDescriptor, PresentMode, SurfaceError, TextureViewDescriptor},
};
use num_traits::cast::AsPrimitive;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{
        ElementState, Ime as WinitIme, KeyEvent as WinitKeyEvent, Modifiers as WinitModifiers,
        MouseButton, MouseScrollDelta, WindowEvent,
    },
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key as WinitKey, NamedKey as WinitNamedKey},
    window::{ResizeDirection, Window, WindowId},
};

use super::{
    embed::Ui,
    frame::Frame,
    neutral::{App, Config, RunError},
    target,
};
use crate::{
    draw::Pt,
    interact::{Input, InputMethod, Key, MOUSE, Modifiers, PointerInput, PointerPhase, Scroll},
    render::{WindowCommand, WindowEdge, shader::ShaderPass, vis::VisPass},
};

/// Opens a window of `size` logical points and runs `app` in it until the
/// window closes.
///
/// # Errors
/// Returns [`RunError`] when the document does not compile, or when the window
/// and its GPU surface cannot be brought up.
pub fn run<Application>(
    app: Application,
    config: Config<'_>,
    size: (u32, u32),
) -> Result<(), RunError>
where
    Application: App,
{
    let event_loop = EventLoop::new().map_err(|error| RunError::Host(error.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut runner = Runner {
        app: Some(app),
        config,
        live: None,
        size,
    };
    event_loop
        .run_app(&mut runner)
        .map_err(|error| RunError::Host(error.to_string()))
}

/// The window and its GPU surface, with the UI mounted in it.
struct Live<'config, Application> {
    clicks: Clicks,
    context: RenderContext,
    idle: IdleClock,
    modifiers: Modifiers,
    pointer: PhysicalPosition<f64>,
    recovery_redraw_latched: bool,
    renderer: Renderer,
    shaders: ShaderPass,
    surface: RenderSurface<'static>,
    ui: Ui<'config, Application>,
    vis: VisPass,
    window: Arc<Window>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceRecovery {
    Retry,
    Reconfigure,
    Stop,
}

struct IdleClock {
    next: WallInstant,
}

impl IdleClock {
    const PERIOD: Duration = Duration::from_millis(500);

    fn new(now: WallInstant) -> Self {
        Self {
            next: now + Self::PERIOD,
        }
    }

    fn wake(&mut self, now: WallInstant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + Self::PERIOD;
        true
    }
}

const fn surface_recovery(error: &SurfaceError) -> SurfaceRecovery {
    match error {
        SurfaceError::Timeout => SurfaceRecovery::Retry,
        SurfaceError::Outdated | SurfaceError::Lost => SurfaceRecovery::Reconfigure,
        SurfaceError::OutOfMemory | SurfaceError::Other => SurfaceRecovery::Stop,
    }
}

/// Multi-click counting. Winit reports presses, not clicks, so the run of
/// presses that count as one gesture is recognised here — once, for every
/// control, rather than in each of them.
#[derive(Default)]
struct Clicks {
    count: u8,
    last: Option<(PhysicalPosition<f64>, Instant)>,
}

impl Clicks {
    /// How long after a press a second one still counts as the same
    /// multi-click, and how far the pointer may drift between them.
    const SLOP: f64 = 5.0;
    const WINDOW: Duration = Duration::from_millis(500);

    fn press(&mut self, at: PhysicalPosition<f64>, now: Instant) -> u8 {
        let repeat = self.last.is_some_and(|(previous, when)| {
            now.duration_since(when) <= Self::WINDOW
                && (previous.x - at.x).abs() <= Self::SLOP
                && (previous.y - at.y).abs() <= Self::SLOP
        });
        self.count = if repeat {
            self.count.saturating_add(1)
        } else {
            1
        };
        self.last = Some((at, now));
        self.count
    }
}

struct Runner<'config, Application> {
    app: Option<Application>,
    config: Config<'config>,
    live: Option<Live<'config, Application>>,
    size: (u32, u32),
}

impl<Application> ApplicationHandler for Runner<'_, Application>
where
    Application: App,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(live) => self.live = Some(live),
            Err(error) => {
                tracing::error!(%error, "window did not start");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
            return;
        }
        let Some(live) = &mut self.live else {
            return;
        };
        match event {
            WindowEvent::Resized(size) => live.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => live.resize(live.window.inner_size()),
            WindowEvent::CursorMoved { position, .. } => {
                live.pointer = position;
                live.point(PointerPhase::Move);
            }
            WindowEvent::CursorLeft { .. } => live.point(PointerPhase::Leave),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => live.point(match state {
                ElementState::Pressed => PointerPhase::Down,
                ElementState::Released => PointerPhase::Up,
            }),
            WindowEvent::MouseWheel { delta, .. } => live.wheel(delta),
            WindowEvent::KeyboardInput { event, .. } => live.key(&event),
            WindowEvent::ModifiersChanged(modifiers) => live.modifiers_changed(modifiers),
            WindowEvent::Ime(event) => live.ime(&event),
            WindowEvent::RedrawRequested => live.draw(),
            _ => {}
        }
        if live.obey(event_loop) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(live) = &mut self.live else {
            return;
        };
        if live.idle.wake(WallInstant::now()) {
            live.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(live.idle.next));
    }
}

/// Carries out what the document asked its window to do. With the system frame
/// off, this is the only way the window can be moved, resized, or closed at all.
const fn resize_direction(edge: WindowEdge) -> ResizeDirection {
    match edge {
        WindowEdge::North => ResizeDirection::North,
        WindowEdge::South => ResizeDirection::South,
        WindowEdge::East => ResizeDirection::East,
        WindowEdge::West => ResizeDirection::West,
        WindowEdge::NorthEast => ResizeDirection::NorthEast,
        WindowEdge::NorthWest => ResizeDirection::NorthWest,
        WindowEdge::SouthEast => ResizeDirection::SouthEast,
        WindowEdge::SouthWest => ResizeDirection::SouthWest,
    }
}

impl<'config, Application> Runner<'config, Application>
where
    Application: App,
{
    fn start(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Result<Live<'config, Application>, RunError> {
        let app = self
            .app
            .take()
            .ok_or_else(|| RunError::Host("the application was already taken".to_owned()))?;
        let mut attributes = Window::default_attributes()
            .with_decorations(self.config.decorations)
            .with_title(self.config.title)
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1));
        if let Some((width, height)) = self.config.min_size {
            attributes = attributes.with_min_inner_size(LogicalSize::new(width, height));
        }
        let window = event_loop
            .create_window(attributes)
            .map_err(|error| RunError::Host(error.to_string()))?;
        window.set_ime_allowed(true);
        let window = Arc::new(window);
        let size = window.inner_size();
        let mut context = RenderContext::new();
        let surface = futures_lite::future::block_on(context.create_surface(
            Arc::clone(&window),
            size.width.max(1),
            size.height.max(1),
            PresentMode::AutoVsync,
        ))
        .map_err(|error| RunError::Host(format!("surface: {error}")))?;
        let handle = &context.devices[surface.dev_id];
        let renderer = Renderer::new(
            &handle.device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|error| RunError::Host(format!("vello renderer: {error}")))?;
        let shaders = ShaderPass::new(&handle.device);
        let vis = VisPass::new(&handle.device, target::FORMAT);

        let scale = window.scale_factor();
        let ui = Ui::new(app, self.config, (size.width, size.height), scale)?;
        let mut live = Live {
            clicks: Clicks::default(),
            context,
            idle: IdleClock::new(WallInstant::now()),
            modifiers: Modifiers::default(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            recovery_redraw_latched: false,
            renderer,
            shaders,
            surface,
            ui,
            vis,
            window,
        };
        live.resize(size);
        Ok(live)
    }
}

impl<Application> Live<'_, Application>
where
    Application: App,
{
    /// One logical frame at sixty a second, which is what the animation pass is
    /// told when the window asks for a redraw.
    const FRAME: Duration = Duration::from_millis(16);

    fn request_redraw(&mut self) {
        self.clear_recovery_redraw();
        self.window.request_redraw();
    }

    fn request_recovery_redraw(&mut self) -> bool {
        if self.recovery_redraw_latched {
            return false;
        }
        self.recovery_redraw_latched = true;
        self.window.request_redraw();
        true
    }

    fn clear_recovery_redraw(&mut self) {
        self.recovery_redraw_latched = false;
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        if self.surface.config.width != size.width || self.surface.config.height != size.height {
            self.reconfigure_surface(size);
        } else {
            self.replace_target(size);
        }
        self.ui
            .resize((size.width, size.height), self.window.scale_factor());
        self.request_redraw();
    }

    fn reconfigure_surface(&mut self, size: PhysicalSize<u32>) {
        self.context
            .resize_surface(&mut self.surface, size.width, size.height);
        self.replace_target(size);
    }

    fn replace_target(&mut self, size: PhysicalSize<u32>) {
        let handle = &self.context.devices[self.surface.dev_id];
        target::replace(&mut self.surface, &handle.device, size.width, size.height);
    }

    fn recover_surface(&mut self, error: &SurfaceError) {
        match surface_recovery(error) {
            SurfaceRecovery::Retry => {
                let redraw_requested = self.request_recovery_redraw();
                tracing::warn!(%error, redraw_requested, "surface frame timed out");
            }
            SurfaceRecovery::Reconfigure => {
                let size = self.window.inner_size();
                if size.width == 0 || size.height == 0 {
                    tracing::warn!(%error, "surface recovery waits for a nonzero resize");
                    return;
                }
                self.reconfigure_surface(size);
                let redraw_requested = self.request_recovery_redraw();
                tracing::warn!(%error, redraw_requested, "surface frame reconfigured");
            }
            SurfaceRecovery::Stop => tracing::error!(%error, "surface frame failed"),
        }
    }

    /// Turns the window's physical pointer position into the logical point the
    /// neutral input contract speaks in.
    fn point(&mut self, phase: PointerPhase) {
        let scale = self.window.scale_factor();
        let at = Pt {
            x: (self.pointer.x / scale).as_(),
            y: (self.pointer.y / scale).as_(),
        };
        let clicks = if phase == PointerPhase::Down {
            self.clicks.press(self.pointer, Instant::now())
        } else {
            self.clicks.count.max(1)
        };
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            phase,
            Some(at),
            clicks,
        )));
        self.request_redraw();
    }

    /// One wheel notch, in whichever units the platform reports it.
    fn wheel(&mut self, delta: MouseScrollDelta) {
        self.ui.input(Input::Wheel(match delta {
            MouseScrollDelta::LineDelta(x, y) => Scroll::Lines { x, y },
            MouseScrollDelta::PixelDelta(at) => Scroll::Pixels {
                x: at.x.as_(),
                y: at.y.as_(),
            },
        }));
        self.request_redraw();
    }

    fn key(&mut self, event: &WinitKeyEvent) {
        let key = portable_key(&event.logical_key);
        let modifiers = self.modifiers;
        self.ui.input(match event.state {
            ElementState::Pressed => Input::KeyPressed {
                key,
                modifiers,
                text: event.text.as_deref(),
            },
            ElementState::Released => Input::KeyReleased { key, modifiers },
        });
        self.request_redraw();
    }

    fn modifiers_changed(&mut self, modifiers: WinitModifiers) {
        let state = modifiers.state();
        self.modifiers = Modifiers::new(
            state.alt_key(),
            state.control_key(),
            state.super_key(),
            state.shift_key(),
        );
        self.ui.input(Input::ModifiersChanged(self.modifiers));
        self.request_redraw();
    }

    fn ime(&mut self, event: &WinitIme) {
        self.ui.input(Input::InputMethod(portable_ime(event)));
        self.request_redraw();
    }

    /// Carries out what the document asked its window to do, and says whether
    /// it asked to be closed.
    fn obey(&mut self, _event_loop: &ActiveEventLoop) -> bool {
        let mut close = false;
        for command in self.ui.take_window_commands() {
            match command {
                WindowCommand::Drag => drop(self.window.drag_window()),
                WindowCommand::Resize(edge) => {
                    drop(self.window.drag_resize_window(resize_direction(edge)));
                }
                WindowCommand::Minimize => self.window.set_minimized(true),
                WindowCommand::ToggleMaximize => {
                    self.window.set_maximized(!self.window.is_maximized());
                }
                WindowCommand::ToggleFullScreen => {
                    let full = self.window.fullscreen().is_some();
                    self.window.set_fullscreen(
                        (!full).then(|| winit::window::Fullscreen::Borderless(None)),
                    );
                }
                WindowCommand::Close => close = true,
            }
        }
        close
    }

    fn draw(&mut self) {
        self.ui.frame(Self::FRAME);
        if !self.ui.needs_frame() {
            return;
        }
        let Ok(frame) = self
            .ui
            .render()
            .inspect_err(|error| tracing::error!(%error, "redraw"))
        else {
            return;
        };
        if self.present(&frame) && self.ui.complete_frame() {
            self.request_redraw();
        }
    }

    /// Vello paints into its own `Rgba8Unorm` target; the surface takes whatever
    /// format the platform offers, so the frame is blitted across.
    fn present(&mut self, frame: &Frame) -> bool {
        let size = self.window.inner_size();
        let handle = &self.context.devices[self.surface.dev_id];
        self.shaders.render(
            &handle.device,
            &handle.queue,
            &mut self.renderer,
            frame.shaders(),
        );
        if let Err(error) = self.renderer.render_to_texture(
            &handle.device,
            &handle.queue,
            frame.scene(),
            &self.surface.target_view,
            &RenderParams {
                base_color: self.ui.background().into(),
                width: size.width,
                height: size.height,
                antialiasing_method: AaConfig::Area,
            },
        ) {
            tracing::error!(%error, "vello render");
            return false;
        }
        self.vis.render(
            &handle.device,
            &handle.queue,
            &self.surface.target_view,
            frame.vis(),
            self.window.scale_factor(),
            [size.width, size.height],
        );
        let frame = match self.surface.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(error) => {
                self.recover_surface(&error);
                return false;
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = handle
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("kithara-ui blit"),
            });
        self.surface.blitter.copy(
            &handle.device,
            &mut encoder,
            &self.surface.target_view,
            &view,
        );
        handle.queue.submit([encoder.finish()]);
        frame.present();
        self.clear_recovery_redraw();
        true
    }
}

fn portable_key(key: &WinitKey) -> Key<'_> {
    match key {
        WinitKey::Named(WinitNamedKey::ArrowDown) => Key::ArrowDown,
        WinitKey::Named(WinitNamedKey::ArrowLeft) => Key::ArrowLeft,
        WinitKey::Named(WinitNamedKey::ArrowRight) => Key::ArrowRight,
        WinitKey::Named(WinitNamedKey::ArrowUp) => Key::ArrowUp,
        WinitKey::Named(WinitNamedKey::Backspace) => Key::Backspace,
        WinitKey::Named(WinitNamedKey::Delete) => Key::Delete,
        WinitKey::Named(WinitNamedKey::End) => Key::End,
        WinitKey::Named(WinitNamedKey::Enter) => Key::Enter,
        WinitKey::Named(WinitNamedKey::Escape) => Key::Escape,
        WinitKey::Named(WinitNamedKey::Home) => Key::Home,
        WinitKey::Named(_) | WinitKey::Unidentified(_) | WinitKey::Dead(_) => Key::Other,
        WinitKey::Character(text) if text.as_str() == " " => Key::Space,
        WinitKey::Character(text) => Key::Character(text.as_str()),
    }
}

fn portable_ime(event: &WinitIme) -> InputMethod<'_> {
    match event {
        WinitIme::Enabled => InputMethod::Opened,
        WinitIme::Preedit(content, selection) => InputMethod::Preedit {
            content,
            selection: *selection,
        },
        WinitIme::Commit(content) => InputMethod::Commit(content),
        WinitIme::Disabled => InputMethod::Closed,
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use masonry::vello::wgpu::SurfaceError;
    use winit::dpi::PhysicalPosition;

    use super::{Clicks, IdleClock, SurfaceRecovery, WallInstant, surface_recovery};

    #[kithara::test]
    fn idle_clock_wakes_once_per_period() {
        let start = WallInstant::now();
        let mut clock = IdleClock::new(start);

        assert!(!clock.wake(start));
        assert!(!clock.wake(start + IdleClock::PERIOD / 2));
        assert!(clock.wake(start + IdleClock::PERIOD));
        assert!(!clock.wake(start + IdleClock::PERIOD));
        assert!(clock.wake(start + IdleClock::PERIOD * 2));
    }

    /// A double click is two presses close in time and place. Nothing below the
    /// window sees presses, so if this does not count them no control can.
    #[kithara::test]
    fn two_quick_presses_in_the_same_place_are_one_double_click() {
        let mut clicks = Clicks::default();
        let start = Instant::now();
        let at = PhysicalPosition::new(100.0, 100.0);

        assert_eq!(clicks.press(at, start), 1);
        assert_eq!(clicks.press(at, start + Duration::from_millis(120)), 2);
        assert_eq!(clicks.press(at, start + Duration::from_millis(240)), 3);
    }

    /// A run ends when the hand pauses or moves away, otherwise every later
    /// press in a session would read as a deeper multi-click.
    #[kithara::test]
    fn a_pause_or_a_move_starts_the_count_again() {
        let mut clicks = Clicks::default();
        let start = Instant::now();
        let at = PhysicalPosition::new(100.0, 100.0);

        assert_eq!(clicks.press(at, start), 1);
        assert_eq!(
            clicks.press(at, start + Duration::from_millis(900)),
            1,
            "a press long after the last one starts a new run"
        );
        assert_eq!(
            clicks.press(
                PhysicalPosition::new(140.0, 100.0),
                start + Duration::from_millis(950)
            ),
            1,
            "a press away from the last one starts a new run"
        );
    }

    #[kithara::test]
    fn retries_only_recoverable_surface_errors() {
        assert_eq!(
            surface_recovery(&SurfaceError::Timeout),
            SurfaceRecovery::Retry
        );
        assert_eq!(
            surface_recovery(&SurfaceError::Outdated),
            SurfaceRecovery::Reconfigure
        );
        assert_eq!(
            surface_recovery(&SurfaceError::Lost),
            SurfaceRecovery::Reconfigure
        );
        assert_eq!(
            surface_recovery(&SurfaceError::OutOfMemory),
            SurfaceRecovery::Stop
        );
        assert_eq!(
            surface_recovery(&SurfaceError::Other),
            SurfaceRecovery::Stop
        );
    }
}
