//! The convenience layer: a window of our own for an application that has no
//! host to live in. Everything it does, it does through [`Ui`].

use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant},
};
use masonry::vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene,
    util::{RenderContext, RenderSurface},
    wgpu::{CommandEncoderDescriptor, PresentMode, TextureViewDescriptor},
};
use num_traits::cast::AsPrimitive;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{ResizeDirection, Window, WindowId},
};

use super::{
    embed::Ui,
    neutral::{App, Config, RunError},
};
use crate::{
    draw::Pt,
    interact::{Input, MOUSE, PointerInput, PointerPhase, Scroll},
    render::{WindowCommand, WindowEdge},
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
    pointer: PhysicalPosition<f64>,
    renderer: Renderer,
    surface: RenderSurface<'static>,
    ui: Ui<'config, Application>,
    window: Arc<Window>,
}

/// Multi-click counting. Winit reports presses, not clicks, so the run of
/// presses that count as one gesture is recognised here — once, for every
/// control, rather than in each of them.
#[derive(Default)]
struct Clicks {
    count: u8,
    last: Option<(PhysicalPosition<f64>, Instant)>,
}

#[cfg(test)]
mod clicks {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use winit::dpi::PhysicalPosition;

    use super::Clicks;

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
            WindowEvent::RedrawRequested => live.draw(),
            _ => {}
        }
        if live.obey(event_loop) {
            event_loop.exit();
        }
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
        let renderer = Renderer::new(
            &context.devices[surface.dev_id].device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|error| RunError::Host(format!("vello renderer: {error}")))?;

        let scale = window.scale_factor();
        let ui = Ui::new(app, self.config, (size.width, size.height), scale)?;
        let mut live = Live {
            clicks: Clicks::default(),
            context,
            pointer: PhysicalPosition::new(0.0, 0.0),
            renderer,
            surface,
            ui,
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

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.context
            .resize_surface(&mut self.surface, size.width, size.height);
        self.ui
            .resize((size.width, size.height), self.window.scale_factor());
        self.window.request_redraw();
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
        self.window.request_redraw();
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
        self.window.request_redraw();
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
                WindowCommand::Close => close = true,
            }
        }
        close
    }

    fn draw(&mut self) {
        self.ui.frame(Self::FRAME);
        let Ok(scene) = self
            .ui
            .scene()
            .inspect_err(|error| tracing::error!(%error, "redraw"))
        else {
            return;
        };
        self.present(&scene);
    }

    /// Vello paints into its own `Rgba8Unorm` target; the surface takes whatever
    /// format the platform offers, so the frame is blitted across.
    fn present(&mut self, scene: &Scene) {
        let size = self.window.inner_size();
        let handle = &self.context.devices[self.surface.dev_id];
        if let Err(error) = self.renderer.render_to_texture(
            &handle.device,
            &handle.queue,
            scene,
            &self.surface.target_view,
            &RenderParams {
                base_color: self.ui.background().into(),
                width: size.width,
                height: size.height,
                antialiasing_method: AaConfig::Area,
            },
        ) {
            tracing::error!(%error, "vello render");
            return;
        }
        let Ok(frame) = self.surface.surface.get_current_texture() else {
            return;
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
    }
}
