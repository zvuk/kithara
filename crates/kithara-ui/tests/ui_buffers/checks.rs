//! What every gallery page asks of the renderer's bump-allocated buffers.

use kithara_test_utils::kithara;
use kithara_ui::{
    app::{Config, Ui},
    builtin,
};
use masonry::vello::{
    AaConfig, AaSupport, DebugLayers, RenderParams, Renderer, RendererOptions,
    peniko::Color,
    util::block_on_wgpu,
    wgpu::{
        DeviceDescriptor, Extent3d, Instance, InstanceDescriptor, RequestAdapterOptions,
        TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor,
    },
};

use crate::{
    capture::Shot,
    custom, demo,
    fixture::{consts, resolver},
    host::{self, Gallery},
};

/// Every page draws inside the renderer's buffers, with headroom left.
///
/// The memory budget beside this says what the buffers cost; this says whether
/// they still fit the work, and the two have to be asked separately. Vello does
/// not grow a buffer that runs out and does not redraw the frame that did: it
/// reports the overflow and leaves the picture wrong. A size tightened too far
/// therefore shows up as a page that renders incorrectly rather than as a test
/// that fails, and nothing downstream of the picture can tell that apart from
/// an ordinary drawing bug.
///
/// So two things are held: Vello's own verdict, which is exact, and a pinned
/// watermark per buffer, which fails while there is still room to spare.
#[kithara::test]
fn every_page_leaves_the_renderer_room_to_spare() {
    /// Twice what the heaviest page was measured to take, per buffer, in the order
    /// the renderer reports them.
    ///
    /// What the pages ask for, pinned apart from what the renderer lays on for
    /// them: the sizes are derived from the frame's tile grid, and a check written
    /// against that same arithmetic would pass for any arithmetic at all.
    const WATERMARKS: [(&str, u32); 7] = [
        ("binning", 8_192),
        ("ptcl", 262_144),
        ("tile", 65_536),
        ("seg_counts", 65_536),
        ("segments", 65_536),
        ("blend", 8_192),
        ("lines", 65_536),
    ];

    /// The one page this cannot draw: its image arrives empty from the test
    /// resolver, so the renderer refuses the scene before any buffer is touched.
    /// Named rather than skipped by catching the failure, so that a second page
    /// going the same way is a failure rather than a silence.
    const UNDRAWABLE: &str = "shader";

    let (width, height) = physical();
    let endpoints = demo::registry();
    let resolver = resolver();
    let kinds = custom::kinds();
    let config = Config::builder()
        .endpoints(&endpoints)
        .resolver(&resolver)
        .text(builtin::text_doc())
        .kinds(&kinds)
        .build();
    let mut ui = Ui::new(Gallery::default(), config, (width, height), 1.0)
        .unwrap_or_else(|error| panic!("the gallery must mount: {error}"));

    let instance = Instance::new(&InstanceDescriptor::default());
    let adapter =
        futures_lite::future::block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .unwrap_or_else(|error| panic!("no wgpu adapter: {error}"));
    let (device, queue) =
        futures_lite::future::block_on(adapter.request_device(&DeviceDescriptor::default()))
            .unwrap_or_else(|error| panic!("no wgpu device: {error}"));
    let mut renderer = Renderer::new(
        &device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .unwrap_or_else(|error| panic!("vello renderer: {error}"));
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("buffer-watermarks"),
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
    let view = texture.create_view(&TextureViewDescriptor::default());

    let mut drawn = 0;
    for page in Shot::all() {
        if page.to_string() == UNDRAWABLE {
            continue;
        }
        host::stand(&mut ui, page).unwrap_or_else(|error| panic!("page {page} must open: {error}"));
        let frame = ui
            .render()
            .unwrap_or_else(|error| panic!("page {page} must draw: {error}"));
        let bump = block_on_wgpu(
            &device,
            #[expect(
                deprecated,
                reason = "the only render that reads the bump allocators back"
            )]
            renderer.render_to_texture_async(
                &device,
                &queue,
                frame.scene(),
                &view,
                &RenderParams {
                    width,
                    height,
                    base_color: Color::TRANSPARENT,
                    antialiasing_method: AaConfig::Area,
                },
                DebugLayers::none(),
            ),
        )
        .unwrap_or_else(|error| panic!("page {page} must render: {error}"))
        .unwrap_or_else(|| panic!("page {page} must report its allocators"));
        assert_eq!(
            bump.failed, 0,
            "page {page} ran a buffer out: the frame Vello left behind is wrong, and this counter \
             is the only thing that says so"
        );
        let took = [
            bump.binning,
            bump.ptcl,
            bump.tile,
            bump.seg_counts,
            bump.segments,
            bump.blend,
            bump.lines,
        ];
        for ((buffer, watermark), used) in WATERMARKS.into_iter().zip(took) {
            assert!(
                used <= watermark,
                "page {page} took {used} elements of the {buffer} buffer, past the {watermark} \
                 pinned for it: it is eating the margin the renderer was sized with"
            );
        }
        drawn += 1;
    }
    assert_eq!(
        drawn,
        Shot::all().len() - 1,
        "every page but {UNDRAWABLE} must have been measured"
    );
}

/// The pixel geometry the pages are drawn at: the window's own size at one
/// pixel to the point, so the buffers measured are the buffers a window fills.
fn physical() -> (u32, u32) {
    (
        num_traits::cast::AsPrimitive::as_(consts::WIDTH),
        num_traits::cast::AsPrimitive::as_(consts::HEIGHT),
    )
}
