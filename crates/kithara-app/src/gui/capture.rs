use std::{
    borrow::Cow,
    env,
    fs::{File, create_dir_all, write},
    io::BufWriter,
    path::{Path, PathBuf},
};

use futures::executor::block_on;
use iced::{
    Pixels, Size,
    advanced::{
        clipboard,
        graphics::text::font_system,
        mouse::Cursor,
        renderer::{Headless, Style},
    },
    theme::Base as _,
};
use iced_runtime::{UserInterface, user_interface::Cache};
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Frame as UiFrame, Ui},
    builtin,
    draw::PoolStats,
    registry::ValueKind,
    render::{
        Clock, PortalMapView, PortalTarget, ReadValue, Reads, ScalarRange, StereoLevels, TableCell,
        TableRow, TreeIcon, TreeRow, UiEvent, WaveBucket, WaveformView, fonts, shader::ShaderPass,
        tree, vis::VisPass,
    },
};
use png::{BitDepth, ColorType, Encoder};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
    wgpu::{
        Backends, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device,
        DeviceDescriptor, Extent3d, Instance, InstanceDescriptor, MapMode, PollType, Queue,
        RequestAdapterOptions, TexelCopyBufferInfo, TexelCopyBufferLayout, Texture,
        TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor,
    },
};

use super::{
    frontend::window_size,
    theme,
    ui::{
        self,
        cache::DeckLayout,
        endpoints::{Registry, readable_kind},
    },
};
use crate::theme::Palette;

#[derive(Clone, Copy)]
struct Geometry {
    height: u32,
    scale: f32,
    width: u32,
}

struct PoolSample {
    first: PoolStats,
    second: PoolStats,
}

impl PoolSample {
    fn line(&self, page: &str) -> String {
        format!(
            "{page} first_misses={} second_misses={} first_home_hits={} second_home_hits={} \
             first_drops={} second_drops={}\n",
            self.first.alloc_misses,
            self.second.alloc_misses,
            self.first.home_hits,
            self.second.home_hits,
            self.first.put_drops,
            self.second.put_drops,
        )
    }

    fn stable(&self) -> bool {
        self.first.alloc_misses > 0
            && self.first.alloc_misses == self.second.alloc_misses
            && self.first.put_drops == self.second.put_drops
    }
}

impl Geometry {
    const BYTES_PER_PIXEL: u32 = 4;
    const ROW_ALIGNMENT: u32 = 256;
    const TEXT_SIZE: Pixels = Pixels(14.0);

    fn studio() -> Self {
        let ((width, height), _) = window_size();
        Self {
            height,
            scale: 1.0,
            width,
        }
    }
}

struct Fixture {
    layout: DeckLayout,
    rows: Vec<TableRow<'static>>,
}

impl Fixture {
    const BPM: f32 = 124.0;
    const LEVELS: StereoLevels = StereoLevels {
        l: 0.58,
        r: 0.46,
        volume: 0.72,
    };
    const SCALAR: f64 = 0.5;

    /// The settings sheet is open in both captures, because a control only the
    /// sheet carries is compared across the hosts on no page otherwise. Its two
    /// sections cannot show at once, so each layout photographs one of them.
    fn on(&self, endpoint: &str) -> bool {
        match endpoint {
            "deck.eq.three_band" | "ui.settings.open" => true,
            "ui.settings.on_view" => self.layout == DeckLayout::Dual,
            "ui.settings.on_audio" => self.layout == DeckLayout::Single,
            _ => false,
        }
    }

    fn new(layout: DeckLayout) -> Self {
        Self {
            layout,
            rows: vec![
                TableRow::new(
                    vec![
                        TableCell::text("deck", "A"),
                        TableCell::text("title", "Midnight Signal"),
                        TableCell::text("artist", "Kithara"),
                    ],
                    true,
                ),
                TableRow::new(
                    vec![
                        TableCell::text("deck", "B"),
                        TableCell::text("title", "Parallel Lines"),
                        TableCell::text("artist", "Studio Fixture"),
                    ],
                    false,
                ),
            ],
        }
    }
}

impl Reads for Fixture {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        /// The browser's source groups, in the order `LibraryView::groups`
        /// lists them. An empty tree would let the page pass its budget while
        /// neither host drew a single row.
        const TREE: [TreeRow<'static>; 3] = [
            TreeRow {
                label: "ALL",
                count: Some(2),
                expanded: None,
                icon: TreeIcon::Collection,
                muted: false,
                selected: false,
                depth: 0,
            },
            TreeRow {
                label: "LOCAL",
                count: Some(2),
                expanded: None,
                icon: TreeIcon::Folder,
                muted: false,
                selected: true,
                depth: 0,
            },
            TreeRow {
                label: "STREAM",
                count: Some(0),
                expanded: None,
                icon: TreeIcon::Playlist,
                muted: false,
                selected: false,
                depth: 0,
            },
        ];
        /// Two decks on the tempo axis. An empty target list would draw an
        /// axis with nothing on it and still pass the page's budget.
        const TEMPOS: [PortalTarget; 2] = [
            PortalTarget {
                bpm: Fixture::BPM,
                is_selected: true,
            },
            PortalTarget {
                bpm: 128.0,
                is_selected: false,
            },
        ];
        const WAVE: [WaveBucket; 8] = [
            WaveBucket {
                high: 0.3,
                low: -0.2,
                mid: 0.05,
            },
            WaveBucket {
                high: 0.6,
                low: -0.4,
                mid: 0.1,
            },
            WaveBucket {
                high: 0.4,
                low: -0.3,
                mid: 0.0,
            },
            WaveBucket {
                high: 0.8,
                low: -0.7,
                mid: 0.08,
            },
            WaveBucket {
                high: 0.5,
                low: -0.4,
                mid: -0.04,
            },
            WaveBucket {
                high: 0.7,
                low: -0.5,
                mid: 0.03,
            },
            WaveBucket {
                high: 0.35,
                low: -0.25,
                mid: 0.0,
            },
            WaveBucket {
                high: 0.55,
                low: -0.45,
                mid: 0.05,
            },
        ];

        let base = endpoint.split_once('@').map_or(endpoint, |(base, _)| base);
        let value = match readable_kind(base)? {
            ValueKind::Bool => ReadValue::Bool(self.on(base)),
            ValueKind::Scalar => ReadValue::Scalar(Self::SCALAR),
            ValueKind::Stereo => ReadValue::Stereo(Self::LEVELS),
            ValueKind::Text => ReadValue::Text(text(base)),
            ValueKind::Waveform => ReadValue::Waveform(WaveformView {
                buckets: &WAVE,
                revision: 0,
                beats: &[],
                cues: &[],
                downbeats: &[],
                bpm: Some(Self::BPM),
                r#loop: None,
            }),
            ValueKind::PortalMap => ReadValue::PortalMap(PortalMapView {
                master: Self::BPM,
                min: 60.0,
                max: 200.0,
                targets: &TEMPOS,
            }),
            ValueKind::Range => ReadValue::Range(ScalarRange {
                min: 0.25,
                max: 0.8,
            }),
            ValueKind::Table => ReadValue::Table(&self.rows),
            ValueKind::Tree => ReadValue::Tree(&TREE),
            _ => return None,
        };
        Some(value)
    }
}

fn text(endpoint: &str) -> &'static str {
    match endpoint {
        "deck.playback.bpm" => "124.0",
        "library.breadcrumb" => "LOCAL \u{b7} 2",
        "library.query" => "",
        "deck.playback.remain" => "-03:42",
        "deck.playback.tempo" => "+0.0%",
        "deck.stream.quality" => "320 kbps",
        "broadcast.url" => "OFF AIR",
        "ui.drag.track" => "",
        _ => "Fixture",
    }
}

impl App for Fixture {
    fn document(&self) -> &str {
        ui::entry(self.layout)
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self)
    }

    fn update(&mut self, _event: UiEvent) {}
}

struct Offscreen {
    device: Device,
    queue: Queue,
    renderer: Renderer,
    shaders: ShaderPass,
    texture: Texture,
    vis: VisPass,
    geometry: Geometry,
}

impl Offscreen {
    fn new(geometry: Geometry) -> Result<Self, String> {
        let instance = Instance::new(&InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .map_err(|error| format!("no wgpu adapter: {error}"))?;
        let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
            .map_err(|error| format!("no wgpu device: {error}"))?;
        let renderer = Renderer::new(
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
            label: Some("studio-parity"),
            size: Extent3d {
                width: geometry.width,
                height: geometry.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING
                | TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let shaders = ShaderPass::new(&device);
        let vis = VisPass::new(&device, TextureFormat::Rgba8Unorm);
        Ok(Self {
            device,
            queue,
            renderer,
            shaders,
            texture,
            vis,
            geometry,
        })
    }

    fn rasterise(&mut self, frame: &UiFrame, base: Color) -> Result<Vec<u8>, String> {
        let view = self.texture.create_view(&TextureViewDescriptor::default());
        self.shaders.render(
            &self.device,
            &self.queue,
            &mut self.renderer,
            frame.shaders(),
        );
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                frame.scene(),
                &view,
                &RenderParams {
                    base_color: base,
                    width: self.geometry.width,
                    height: self.geometry.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|error| format!("render_to_texture: {error}"))?;
        self.vis.render(
            &self.device,
            &self.queue,
            &view,
            frame.vis(),
            f64::from(self.geometry.scale),
            [self.geometry.width, self.geometry.height],
        );
        read_back(&self.device, &self.queue, &self.texture, self.geometry)
    }
}

#[kithara::test]
fn studio_capture_writes_both_hosts() {
    let Some(dir) = env::var_os("KITHARA_STUDIO_CAPTURE").map(PathBuf::from) else {
        return;
    };
    capture(&dir).unwrap_or_else(|error| panic!("studio capture failed: {error}"));
}

fn capture(dir: &Path) -> Result<(), String> {
    let geometry = Geometry::studio();
    let iced_dir = dir.join("iced");
    let masonry_dir = dir.join("masonry");
    create_dir_all(&iced_dir).map_err(|error| format!("create {}: {error}", iced_dir.display()))?;
    create_dir_all(&masonry_dir)
        .map_err(|error| format!("create {}: {error}", masonry_dir.display()))?;
    write_frame(&iced_dir, geometry)?;
    write_frame(&masonry_dir, geometry)?;
    load_fonts()?;
    let mut renderer = block_on(<iced::Renderer as Headless>::new(
        fonts::SANS,
        Geometry::TEXT_SIZE,
        Some("wgpu"),
    ))
    .ok_or_else(|| "iced did not provide a headless wgpu renderer".to_owned())?;
    let mut offscreen = Offscreen::new(geometry)?;
    let mut iced_stats = String::new();
    let mut masonry_stats = String::new();

    for (layout, name) in [
        (DeckLayout::Single, "studio-single.png"),
        (DeckLayout::Dual, "studio-dual.png"),
    ] {
        let (iced_rgba, iced_sample) = iced(layout, &mut renderer, geometry)?;
        let (masonry_rgba, masonry_sample) = masonry(layout, &mut offscreen, geometry)?;
        if !iced_sample.stable() || !masonry_sample.stable() {
            return Err(format!(
                "draw pools allocated again on the second {name} frame: iced={} masonry={}",
                iced_sample.line(name).trim(),
                masonry_sample.line(name).trim(),
            ));
        }
        write_png(&iced_dir.join(name), &iced_rgba, geometry)?;
        write_png(&masonry_dir.join(name), &masonry_rgba, geometry)?;
        iced_stats.push_str(&iced_sample.line(name));
        masonry_stats.push_str(&masonry_sample.line(name));
    }
    write(iced_dir.join("draw-pools.txt"), iced_stats)
        .map_err(|error| format!("write iced draw-pools.txt: {error}"))?;
    write(masonry_dir.join("draw-pools.txt"), masonry_stats)
        .map_err(|error| format!("write masonry draw-pools.txt: {error}"))?;
    Ok(())
}

fn iced(
    layout: DeckLayout,
    renderer: &mut iced::Renderer,
    geometry: Geometry,
) -> Result<(Vec<u8>, PoolSample), String> {
    let compiled = ui::compile_ui(layout)
        .map_err(|error| format!("compile {}: {error}", ui::entry(layout)))?;
    let reads = Fixture::new(layout);
    let skin = builtin::skin();
    let theme = theme::kithara_theme(&Palette::default().into());
    let mut ui = UserInterface::build(
        tree::render(&compiled.root, &compiled, &reads, skin, Clock::default()),
        Size::new(geometry.width as f32, geometry.height as f32),
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
    let base = theme.base();
    ui.draw(
        renderer,
        &theme,
        &Style {
            text_color: base.text_color,
        },
        Cursor::Unavailable,
    );
    let rgba = renderer.screenshot(
        Size::new(geometry.width, geometry.height),
        geometry.scale,
        base.background_color,
    );
    let first = compiled.draw_pool_stats();
    ui.draw(
        renderer,
        &theme,
        &Style {
            text_color: base.text_color,
        },
        Cursor::Unavailable,
    );
    Ok((
        rgba,
        PoolSample {
            first,
            second: compiled.draw_pool_stats(),
        },
    ))
}

fn masonry(
    layout: DeckLayout,
    offscreen: &mut Offscreen,
    geometry: Geometry,
) -> Result<(Vec<u8>, PoolSample), String> {
    let resolver = ui::resolver();
    let endpoints = Registry::default();
    let text = ui::text().map_err(|error| format!("catalog: {error}"))?;
    let mut ui = Ui::new(
        Fixture::new(layout),
        Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .skin(builtin::skin())
            .skin_doc(builtin::skin_doc())
            .text(&text)
            .build(),
        (geometry.width, geometry.height),
        f64::from(geometry.scale),
    )
    .map_err(|error| format!("mount {}: {error}", ui::entry(layout)))?;
    let frame = ui
        .render()
        .map_err(|error| format!("draw {}: {error}", ui::entry(layout)))?;
    let rgba = offscreen.rasterise(&frame, ui.background().into())?;
    let first = ui.draw_pool_stats();
    drop(
        ui.render()
            .map_err(|error| format!("second draw {}: {error}", ui::entry(layout)))?,
    );
    Ok((
        rgba,
        PoolSample {
            first,
            second: ui.draw_pool_stats(),
        },
    ))
}

fn load_fonts() -> Result<(), String> {
    let mut system = font_system()
        .write()
        .map_err(|error| format!("iced font system lock: {error}"))?;
    for bytes in fonts::FONT_BYTES {
        system.load_font(Cow::Borrowed(bytes));
    }
    Ok(())
}

fn write_frame(dir: &Path, geometry: Geometry) -> Result<(), String> {
    let path = dir.join("frame.txt");
    write(
        &path,
        format!(
            "{} {} {}\n",
            geometry.width, geometry.height, geometry.scale
        ),
    )
    .map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_png(path: &Path, rgba: &[u8], geometry: Geometry) -> Result<(), String> {
    let file = File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut encoder = Encoder::new(BufWriter::new(file), geometry.width, geometry.height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgba))
        .map_err(|error| format!("encode {}: {error}", path.display()))
}

fn read_back(
    device: &Device,
    queue: &Queue,
    texture: &Texture,
    geometry: Geometry,
) -> Result<Vec<u8>, String> {
    let unpadded = geometry.width * Geometry::BYTES_PER_PIXEL;
    let padded = unpadded.div_ceil(Geometry::ROW_ALIGNMENT) * Geometry::ROW_ALIGNMENT;
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("studio-parity-readback"),
        size: u64::from(padded) * u64::from(geometry.height),
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
                rows_per_image: Some(geometry.height),
            },
        },
        Extent3d {
            width: geometry.width,
            height: geometry.height,
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
    let mut rgba = Vec::with_capacity((unpadded * geometry.height) as usize);
    for row in 0..geometry.height {
        let start = (row * padded) as usize;
        rgba.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(rgba)
}
