//! Photographs the gallery through iced with no window and no display.
//!
//! Enabled by `KITHARA_GALLERY_CAPTURE_OFFSCREEN=<dir>`. It writes the same
//! pages, the same `frame.txt` and the same images as the window capture, so any
//! two of the three sets — window, offscreen, masonry — compare on equal terms.
//!
//! Parity with the masonry capture is the point: that one has always been
//! headless, and while this one needed a display the two could only be compared
//! on a developer's machine. The window capture stays, and its job becomes
//! checking that this path draws what a window draws.

use std::{
    borrow::Cow,
    env,
    fs::create_dir_all,
    path::{Path, PathBuf},
};

use futures_lite::future::block_on;
use iced::{
    Pixels, Size,
    advanced::{
        clipboard,
        graphics::{Shell, Viewport, text::font_system},
        mouse::Cursor,
        renderer::Style,
    },
    theme::Base as _,
    window,
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
use kithara_ui::render::fonts::{FONT_BYTES, SANS};
use num_traits::cast::AsPrimitive;

use super::{
    capture::{Frame, Shot, read_frame, write_frame, write_png},
    theme, view,
};

/// The scale the offscreen set is photographed at when the directory does not
/// already say. One, so a page's pixels are its points and a difference between
/// two sets is a difference in what was drawn rather than in how it was sampled.
const SCALE: f32 = 1.0;

/// Runs the capture when asked. Returns `false` when the environment variable
/// is absent, so the caller falls through to the window.
pub(super) fn run() -> bool {
    let Some(dir) = env::var_os("KITHARA_GALLERY_CAPTURE_OFFSCREEN").map(PathBuf::from) else {
        return false;
    };
    match capture(&dir) {
        Ok(count) => println!("{count} page(s) written to {}", dir.display()),
        Err(error) => eprintln!("offscreen capture failed: {error}"),
    }
    true
}

fn capture(dir: &PathBuf) -> Result<usize, String> {
    create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let frame = frame(dir);
    write_frame(dir, frame)?;
    let mut gallery = super::Gallery::mounted();
    let skin = gallery.skin;
    let theme = theme(skin);
    let mut renderer = renderer()?;
    let mut written = 0;
    for shot in Shot::all() {
        gallery.select(shot);
        let rgba = page(&gallery, &theme, frame, &mut renderer)?;
        write_png(
            &dir.join(format!("{}.png", shot.name())),
            &rgba,
            frame.width,
            frame.height,
        )?;
        written += 1;
    }
    Ok(written)
}

/// One page, laid out and rasterised at the frame's geometry.
///
/// The runtime's own interface does the drawing rather than a hand-rolled
/// layout-then-draw. Two things this host needs come only from there: `update`
/// is what builds the overlay every window layer paints from, and `draw` is
/// what resets the renderer between pages.
fn page(
    gallery: &super::Gallery,
    theme: &iced::Theme,
    frame: Frame,
    renderer: &mut iced::Renderer,
) -> Result<Vec<u8>, String> {
    let scale = AsPrimitive::<f32>::as_(frame.scale);
    let logical = Size::new(
        AsPrimitive::<f32>::as_(frame.width) / scale,
        AsPrimitive::<f32>::as_(frame.height) / scale,
    );
    let mut ui = UserInterface::build(
        view(gallery, window::Id::unique()),
        logical,
        Cache::default(),
        renderer,
    );
    drop(ui.update(
        &[],
        Cursor::Unavailable,
        renderer,
        &mut clipboard::Null,
        &mut Vec::new(),
    ));
    // The theme's own base is what the window clears to and writes text in.
    // Passing anything else here shows up wherever the document leaves the
    // background bare, and reads as a difference between the hosts.
    let base = theme.base();
    ui.draw(
        renderer,
        theme,
        &Style {
            text_color: base.text_color,
        },
        Cursor::Unavailable,
    );
    let FallbackRenderer::Primary(renderer) = renderer else {
        return Err("the offscreen capture must rasterise through wgpu".to_owned());
    };
    Ok(renderer.screenshot(
        &Viewport::with_physical_size(Size::new(frame.width, frame.height), scale),
        base.background_color,
    ))
}

/// A renderer with the gallery's own faces registered, drawing into a texture
/// rather than into a surface.
///
/// wgpu and not the software rasteriser, because the window draws through wgpu:
/// a set taken through a different engine answers a different question, and the
/// one this set is here to answer is what the window shows.
fn renderer() -> Result<iced::Renderer, String> {
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
        // The gallery's window opens with iced's own default, which is off.
        None,
        Shell::headless(),
    );
    Ok(FallbackRenderer::Primary(WgpuRenderer::new(
        engine,
        SANS,
        Pixels(14.0),
    )))
}

/// The geometry to photograph at: whatever a capture already sitting in this
/// directory used, so a set taken on a display — where the scale is the
/// screen's, not ours — can be answered on its own terms. Falls back to the
/// gallery's own logical size at 1x, which is what the window opens at.
fn frame(dir: &Path) -> Frame {
    read_frame(dir).unwrap_or_else(|| {
        let size = super::window_size();
        Frame {
            height: AsPrimitive::<u32>::as_(size.height * SCALE),
            scale: SCALE.into(),
            width: AsPrimitive::<u32>::as_(size.width * SCALE),
        }
    })
}
