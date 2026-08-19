//! One clipped list, both backends, one question: does what a clip contains
//! reach the pixels?
//!
//! Every other test in the crate asks a backend what it *recorded* — that a
//! clip is in the list, that the scene balanced its layers. Both answered yes
//! while the production renderer painted nothing inside the clip, because the
//! headless harness rasterises through a different renderer than the
//! application does. This module rasterises instead, through the renderer the
//! application actually uses, and looks at a pixel.

use futures_lite::future::block_on;
use iced::{
    Color, Pixels, Size, Vector,
    advanced::{
        Renderer as _,
        graphics::{geometry::Renderer as _, text::font_system},
        renderer::Headless,
    },
    widget::canvas::Frame,
};
use kithara_test_utils::kithara;
use num_traits::cast::cast;
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene,
    kurbo::Affine,
    peniko::{Color as VelloColor, color::palette},
    wgpu::{
        BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device, DeviceDescriptor,
        Extent3d, Instance, InstanceDescriptor, MapMode, PollType, Queue, RequestAdapterOptions,
        TexelCopyBufferInfo, TexelCopyBufferLayout, TextureDescriptor, TextureDimension,
        TextureFormat, TextureUsages, TextureViewDescriptor,
    },
};

use super::{VelloBackend, replay_ordered};
use crate::{
    builtin,
    draw::{
        DrawList, DrawListBuilder, FillRule, LineCap, LineJoin, Paint, Path, Pen, Pt, Rect, Rgba,
        Stop, Stops, Verb, replay,
    },
    render::fonts::{FONT_BYTES, SANS},
};

/// The one fixture both backends are asked to paint.
struct Fixture;

impl Fixture {
    /// The surface they paint into. Narrow enough that every width the two
    /// rasterisers and the sampler ask for converts exactly.
    const SURFACE: (u16, u16) = (96, 96);

    /// Where the control sits on it. A widget almost never starts at the
    /// window's origin, and the defect this module exists for only appears when
    /// it does not.
    const ORIGIN: (f32, f32) = (24.0, 24.0);

    /// The clip, and a rectangle wholly inside it.
    const REGION: Rect = Rect {
        h: 40.0,
        w: 40.0,
        x: 12.0,
        y: 12.0,
    };

    const INSIDE: Rect = Rect {
        h: 20.0,
        w: 20.0,
        x: 22.0,
        y: 22.0,
    };

    /// What the clip contains: white, so every channel is high.
    const INK: Rgba = Rgba {
        a: 1.0,
        b: 1.0,
        g: 1.0,
        r: 1.0,
    };

    /// What is drawn under it, outside any clip: red, so a single channel tells
    /// which of the two won.
    const UNDER: Rgba = Rgba {
        a: 1.0,
        b: 0.0,
        g: 0.0,
        r: 1.0,
    };

    /// The outer square of a ring, and the square that must be a hole in it.
    const OUTER: Rect = Rect {
        h: 48.0,
        w: 48.0,
        x: 8.0,
        y: 8.0,
    };

    const HOLE: Rect = Rect {
        h: 16.0,
        w: 16.0,
        x: 24.0,
        y: 24.0,
    };

    /// How wide the pen is when a test asks about its ends and corners. Wide
    /// enough that what a cap or a join adds is several pixels across.
    const NIB: f32 = 12.0;

    /// A line whose free ends the cap shapes.
    const RULE: (Pt, Pt) = (Pt { x: 20.0, y: 8.0 }, Pt { x: 40.0, y: 8.0 });

    /// A box whose four right angles the join shapes.
    const CORNER: Rect = Rect {
        h: 40.0,
        w: 40.0,
        x: 24.0,
        y: 24.0,
    };
}

/// A control that fills its own box and then draws inside a clip — the order
/// every real control uses, and the one a backend has to keep.
fn clipped() -> DrawList {
    let mut list = DrawListBuilder::default();
    list.fill_rect(Fixture::REGION, Fixture::UNDER);
    let mut inner = DrawListBuilder::default();
    inner.fill_rect(Fixture::INSIDE, Fixture::INK);
    list.clip(Fixture::REGION, inner.finish());
    list.finish()
}

fn square(rect: Rect) -> [Verb; 5] {
    [
        Verb::MoveTo(Pt {
            x: rect.x,
            y: rect.y,
        }),
        Verb::LineTo(Pt {
            x: rect.x + rect.w,
            y: rect.y,
        }),
        Verb::LineTo(Pt {
            x: rect.x + rect.w,
            y: rect.y + rect.h,
        }),
        Verb::LineTo(Pt {
            x: rect.x,
            y: rect.y + rect.h,
        }),
        Verb::Close,
    ]
}

/// A square with a square hole, both wound the same way. Only the even-odd rule
/// makes the middle a hole; under the default it is simply more ink. An
/// authored icon is exactly this shape, which is why the rule travels with the
/// outline rather than being assumed by a backend.
fn ring() -> DrawList {
    let mut verbs = Vec::from(square(Fixture::OUTER));
    verbs.extend(square(Fixture::HOLE));
    let mut list = DrawListBuilder::default();
    list.fill_path(Path::new(FillRule::EvenOdd, verbs), Fixture::INK);
    list.finish()
}

/// Dark at one end of the ramp and light at the other.
fn stops() -> Stops {
    Stops::new(&[
        Stop {
            color: Fixture::UNDER,
            offset: 0.0,
        },
        Stop {
            color: Fixture::INK,
            offset: 1.0,
        },
    ])
    .unwrap_or_else(|error| panic!("a two-stop ramp is valid: {error}"))
}

/// One shape filled with a ramp that runs left to right, black to white. A
/// backend that drops the ramp paints one flat colour, and both ends read the
/// same.
fn ramped() -> DrawList {
    let mut list = DrawListBuilder::default();
    list.fill_rect(
        Fixture::OUTER,
        Paint::Linear {
            from: Pt {
                x: Fixture::OUTER.x,
                y: Fixture::OUTER.y,
            },
            stops: stops(),
            to: Pt {
                x: Fixture::OUTER.x + Fixture::OUTER.w,
                y: Fixture::OUTER.y,
            },
        },
    );
    list.finish()
}

/// A plain rectangle and, over it, one filled with a ramp out from its centre.
///
/// The plain one is what makes a refusal legible: a backend that merely skipped
/// the command it could not draw would still paint it, while one that refuses
/// the list whole paints neither.
fn radial() -> DrawList {
    let mut list = DrawListBuilder::default();
    list.fill_rect(Fixture::REGION, Fixture::UNDER);
    list.fill_rect(
        Fixture::OUTER,
        Paint::Radial {
            center: Pt {
                x: Fixture::OUTER.x + Fixture::OUTER.w / 2.0,
                y: Fixture::OUTER.y + Fixture::OUTER.h / 2.0,
            },
            radius: Fixture::OUTER.w / 2.0,
            stops: stops(),
        },
    );
    list.finish()
}

/// Dark at the ramp's start and light at its end, in the same shape.
fn ramp_runs_across(rgba: &[u8]) -> bool {
    let middle = Fixture::OUTER.y + Fixture::OUTER.h / 2.0;
    let near = sample(rgba, Fixture::OUTER.x + 2.0, middle);
    let far = sample(rgba, Fixture::OUTER.x + Fixture::OUTER.w - 2.0, middle);
    near < 64 && far > 192
}

/// A pen that keeps a stroke inside its geometry: ends cut flush, corners
/// flattened.
fn flush() -> Pen {
    Pen::new(Fixture::NIB).with_join(LineJoin::Bevel)
}

/// A pen that reaches past it: ends rounded off, corners carried to a point.
fn reaching() -> Pen {
    Pen::new(Fixture::NIB).with_cap(LineCap::Round)
}

/// A line and a box, both stroked with the same pen. Nothing else separates one
/// pen from another here: same geometry, same width, same colour.
fn shaped(pen: Pen) -> DrawList {
    let mut list = DrawListBuilder::default();
    list.stroke_line(Fixture::RULE.0, Fixture::RULE.1, Fixture::INK, pen);
    list.stroke_rounded_rect(Fixture::CORNER, 0.0, Fixture::INK, pen);
    list.finish()
}

/// Ink past the line's last point — where only a round cap reaches.
fn marks_past_its_end(rgba: &[u8]) -> bool {
    let reach = Fixture::NIB / 3.0;
    sample(rgba, Fixture::RULE.1.x + reach, Fixture::RULE.1.y) > 128
}

/// Ink out at the box's outer corner — where only a mitred join reaches.
fn marks_past_its_corner(rgba: &[u8]) -> bool {
    let reach = Fixture::NIB / 3.0;
    sample(rgba, Fixture::CORNER.x - reach, Fixture::CORNER.y - reach) > 128
}

/// Dark at the centre and light out at the rim, in the same shape.
fn ramp_runs_outward(rgba: &[u8]) -> bool {
    let center = Fixture::OUTER.x + Fixture::OUTER.w / 2.0;
    let middle = Fixture::OUTER.y + Fixture::OUTER.h / 2.0;
    sample(rgba, center, middle) < 64
        && sample(rgba, center + Fixture::OUTER.w / 2.0 - 1.0, middle) > 192
}

/// Nothing at all reached the pixels.
fn surface_is_untouched(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4)
        .all(|pixel| pixel[..3].iter().all(|channel| *channel == 0))
}

/// The green channel where the surface was asked about, or zero off-surface.
fn sample(rgba: &[u8], x: f32, y: f32) -> u8 {
    let x = pixel_index(Fixture::ORIGIN.0 + x);
    let y = pixel_index(Fixture::ORIGIN.1 + y);
    rgba.get((y * usize::from(Fixture::SURFACE.0) + x) * 4 + 1)
        .copied()
        .unwrap_or_default()
}

/// The pixel a fixture coordinate lands in. Every coordinate the module samples
/// is built from the constants above, so one that will not convert means the
/// fixture is wrong rather than the sample being off the surface.
fn pixel_index(coordinate: f32) -> usize {
    cast(coordinate)
        .unwrap_or_else(|| panic!("the fixture must sample the surface, not at {coordinate}"))
}

/// Ink on the ring itself, and nothing in the hole.
fn ring_has_a_hole(rgba: &[u8]) -> bool {
    let middle = Fixture::HOLE.y + Fixture::HOLE.h / 2.0;
    sample(rgba, Fixture::OUTER.x + 4.0, middle) > 128
        && sample(rgba, Fixture::HOLE.x + Fixture::HOLE.w / 2.0, middle) < 128
}

/// Whether the clip's contents won the centre pixel. The rectangle under it is
/// red and the clipped one is white, so a high green channel means the clipped
/// rectangle is on top — which is the order the list asked for.
fn clip_is_on_top(rgba: &[u8]) -> bool {
    sample(
        rgba,
        Fixture::INSIDE.x + Fixture::INSIDE.w / 2.0,
        Fixture::INSIDE.y + Fixture::INSIDE.h / 2.0,
    ) > 128
}

fn through_vello(list: &DrawList) -> Vec<u8> {
    let mut control = Scene::new();
    replay(list, &mut VelloBackend::new(&mut control));
    let mut scene = Scene::new();
    scene.append(
        &control,
        Some(Affine::translate((
            f64::from(Fixture::ORIGIN.0),
            f64::from(Fixture::ORIGIN.1),
        ))),
    );
    rasterise(&scene).unwrap_or_else(|error| panic!("vello must rasterise: {error}"))
}

#[kithara::test]
fn vello_paints_what_a_clip_contains() {
    assert!(
        clip_is_on_top(&through_vello(&clipped())),
        "the Vello backend painted its own geometry over what the clip contained"
    );
}

#[kithara::test]
fn vello_leaves_the_hole_an_outline_asked_for() {
    assert!(
        ring_has_a_hole(&through_vello(&ring())),
        "the Vello backend ignored the outline's fill rule"
    );
}

#[kithara::test]
fn vello_runs_a_ramp_across_the_shape_it_fills() {
    assert!(
        ramp_runs_across(&through_vello(&ramped())),
        "the Vello backend flattened the ramp"
    );
}

#[kithara::test]
fn vello_ramps_a_radial_out_from_the_centre_it_was_given() {
    assert!(
        ramp_runs_outward(&through_vello(&radial())),
        "the Vello backend flattened the radial ramp"
    );
}

/// iced has no radial gradient, so the honest answer is nothing — including the
/// plain rectangle underneath, which it could have drawn perfectly well. Half a
/// picture is a picture the list never described.
#[kithara::test]
fn iced_refuses_a_list_holding_a_ramp_it_cannot_draw() {
    assert!(
        surface_is_untouched(&through_iced(&radial())),
        "the iced backend painted part of a list it cannot draw whole"
    );
}

/// A pen that cuts flush and bevels stops at its geometry; one that rounds its
/// ends and mitres its corners reaches past it. The cap and the join are asked
/// about separately, so a backend that honours one and drops the other is
/// caught by the half it dropped.
#[kithara::test]
fn vello_shapes_a_stroke_the_way_the_pen_asked() {
    let flush = through_vello(&shaped(flush()));
    let reaching = through_vello(&shaped(reaching()));

    assert!(
        !marks_past_its_end(&flush),
        "vello: a flush end reached out"
    );
    assert!(
        !marks_past_its_corner(&flush),
        "vello: a bevelled corner reached out"
    );
    assert!(
        marks_past_its_end(&reaching),
        "vello: a round end stopped short"
    );
    assert!(
        marks_past_its_corner(&reaching),
        "vello: a mitred corner stopped short"
    );
}

#[kithara::test]
fn iced_shapes_a_stroke_the_way_the_pen_asked() {
    let flush = through_iced(&shaped(flush()));
    let reaching = through_iced(&shaped(reaching()));

    assert!(!marks_past_its_end(&flush), "iced: a flush end reached out");
    assert!(
        !marks_past_its_corner(&flush),
        "iced: a bevelled corner reached out"
    );
    assert!(
        marks_past_its_end(&reaching),
        "iced: a round end stopped short"
    );
    assert!(
        marks_past_its_corner(&reaching),
        "iced: a mitred corner stopped short"
    );
}

#[kithara::test]
fn iced_runs_a_ramp_across_the_shape_it_fills() {
    assert!(
        ramp_runs_across(&through_iced(&ramped())),
        "the iced backend flattened the ramp"
    );
}

#[kithara::test]
fn iced_leaves_the_hole_an_outline_asked_for() {
    assert!(
        ring_has_a_hole(&through_iced(&ring())),
        "the iced backend ignored the outline's fill rule"
    );
}

#[kithara::test]
fn iced_paints_what_a_clip_contains() {
    assert!(
        clip_is_on_top(&through_iced(&clipped())),
        "the iced backend painted its own geometry over what the clip contained"
    );
}

fn through_iced(list: &DrawList) -> Vec<u8> {
    let skin = builtin::skin();
    let mut fonts = font_system()
        .write()
        .unwrap_or_else(|error| panic!("the iced font system must be available: {error}"));
    for bytes in FONT_BYTES {
        fonts.load_font(std::borrow::Cow::Borrowed(bytes));
    }
    drop(fonts);

    let mut renderer = block_on(<iced::Renderer as Headless>::new(
        SANS,
        Pixels(14.0),
        Some("wgpu"),
    ))
    .unwrap_or_else(|| panic!("iced must give a wgpu renderer without a window"));
    let mut frame = Frame::new(
        &renderer,
        Size::new(f32::from(Fixture::SURFACE.0), f32::from(Fixture::SURFACE.1)),
    );
    replay_ordered(list, &mut frame, skin.text_resources());
    let geometry = frame.into_geometry();
    renderer.with_translation(
        Vector::new(Fixture::ORIGIN.0, Fixture::ORIGIN.1),
        |renderer| {
            renderer.draw_geometry(geometry);
        },
    );
    renderer.screenshot(
        Size::new(u32::from(Fixture::SURFACE.0), u32::from(Fixture::SURFACE.1)),
        1.0,
        Color::from_rgb(0.0, 0.0, 0.0),
    )
}

/// A wgpu device with no surface, and one scene rasterised through it.
fn rasterise(scene: &Scene) -> Result<Vec<u8>, String> {
    rasterise_at(
        scene,
        (u32::from(Fixture::SURFACE.0), u32::from(Fixture::SURFACE.1)),
        palette::css::BLACK,
    )
}

pub(crate) fn rasterise_at(
    scene: &Scene,
    size: (u32, u32),
    base: VelloColor,
) -> Result<Vec<u8>, String> {
    let (width, height) = size;
    let instance = Instance::new(&InstanceDescriptor::default());
    let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
        .map_err(|error| format!("no wgpu adapter: {error}"))?;
    let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
        .map_err(|error| format!("no wgpu device: {error}"))?;
    let mut renderer = Renderer::new(
        &device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .map_err(|error| format!("vello renderer: {error}"))?;
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("clip-conformance"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    renderer
        .render_to_texture(
            &device,
            &queue,
            scene,
            &texture.create_view(&TextureViewDescriptor::default()),
            &RenderParams {
                base_color: base,
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        )
        .map_err(|error| format!("render_to_texture: {error}"))?;
    read_back(&device, &queue, &texture, (width, height))
}

/// Copies the target texture back, undoing wgpu's 256-byte row padding.
fn read_back(
    device: &Device,
    queue: &Queue,
    texture: &vello::wgpu::Texture,
    size: (u32, u32),
) -> Result<Vec<u8>, String> {
    let (width, height) = size;
    let unpadded = width * 4;
    let padded = unpadded.div_ceil(256) * 256;
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("clip-conformance-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(MapMode::Read, |_| {});
    device
        .poll(PollType::Wait)
        .map_err(|error| format!("poll: {error}"))?;
    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        rgba.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(rgba)
}
