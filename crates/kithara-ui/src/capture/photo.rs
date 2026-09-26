use std::borrow::Cow;

use futures_lite::future::block_on;
use iced::{
    Element, Pixels, Size, Theme,
    advanced::{
        clipboard,
        graphics::{Shell, Viewport, text::font_system},
        mouse::Cursor,
        renderer::Style,
    },
    theme::Base as _,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{UserInterface, user_interface::Cache};
use iced_wgpu::{
    Engine, Renderer as WgpuRenderer,
    wgpu::{
        Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
        TextureFormat,
    },
};
use kithara_ui_capture::Geometry;
use num_traits::cast::AsPrimitive;

use crate::render::fonts::{FONT_BYTES, SANS};

/// Photographs a page through iced with no window and no display.
///
/// wgpu and not the software rasteriser, because a window draws through wgpu:
/// a set taken through a different engine answers a different question, and
/// the one an offscreen set is here to answer is what the window shows.
pub struct Photographer {
    renderer: iced::Renderer,
}

impl Photographer {
    /// A renderer with the toolkit's own faces registered, drawing into a
    /// texture rather than into a surface.
    ///
    /// # Errors
    /// Fails when the machine offers no wgpu adapter or device, which is a
    /// machine without a graphics device rather than a defect.
    pub fn new() -> Result<Self, String> {
        let mut fonts = font_system()
            .write()
            .map_err(|error| format!("iced font system lock: {error}"))?;
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        let instance = Instance::new(&InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..InstanceDescriptor::default()
        });
        let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .map_err(|error| format!("no wgpu adapter: {error}"))?;
        let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
            .map_err(|error| format!("no wgpu device: {error}"))?;
        let engine = Engine::new(
            &adapter,
            device,
            queue,
            TextureFormat::Rgba8UnormSrgb,
            None,
            Shell::headless(),
        );
        Ok(Self {
            renderer: FallbackRenderer::Primary(WgpuRenderer::new(engine, SANS, Pixels(14.0))),
        })
    }

    /// One page, laid out and rasterised at the frame's geometry.
    ///
    /// The runtime's own interface does the drawing rather than a hand-rolled
    /// layout-then-draw. Two things this needs come only from there: `update`
    /// is what builds the overlay every window layer paints from, and `draw`
    /// is what resets the renderer between pages.
    ///
    /// # Errors
    /// Fails when the renderer fell back to the software rasteriser, which
    /// would answer a different question than the window does.
    pub fn shoot<Message>(
        &mut self,
        element: Element<'_, Message, Theme, iced::Renderer>,
        theme: &Theme,
        frame: Geometry,
    ) -> Result<Vec<u8>, String> {
        let scale = AsPrimitive::<f32>::as_(frame.scale);
        let logical = Size::new(
            AsPrimitive::<f32>::as_(frame.width) / scale,
            AsPrimitive::<f32>::as_(frame.height) / scale,
        );
        let mut ui = UserInterface::build(element, logical, Cache::default(), &mut self.renderer);
        drop(ui.update(
            &[],
            Cursor::Unavailable,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        ));
        let base = theme.base();
        ui.draw(
            &mut self.renderer,
            theme,
            &Style {
                text_color: base.text_color,
            },
            Cursor::Unavailable,
        );
        let FallbackRenderer::Primary(renderer) = &mut self.renderer else {
            return Err("the offscreen capture must rasterise through wgpu".to_owned());
        };
        Ok(renderer.screenshot(
            &Viewport::with_physical_size(Size::new(frame.width, frame.height), scale),
            base.background_color,
        ))
    }
}
