use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
};

use futures_lite::future::block_on;
use hotpath::measure_block;
use iced::advanced::graphics::Shell;
use iced_wgpu::{
    Engine,
    wgpu::{
        Backends, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device,
        DeviceDescriptor, Extent3d, Instance, InstanceDescriptor, MapMode, PollType, Queue,
        RequestAdapterOptions, TexelCopyBufferInfo, TexelCopyBufferLayout, Texture,
        TextureDescriptor, TextureDimension, TextureUsages,
    },
};
use kithara_ui::{
    app::Frame,
    backends::paint_color,
    builtin,
    render::{shader::ShaderPass, vis::VisPass},
};
use masonry::vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions,
    wgpu as vello_wgpu,
};
use num_traits::cast::AsPrimitive as _;

use crate::{
    fixture::consts::{HEIGHT, WIDTH},
    pages::consts::IMMEDIATE_FORMAT,
};

/// A wgpu device with no window, plus iced's engine on it. Building one per page
/// would measure device creation once per page.
pub(crate) struct ImmediateGpu {
    pub(crate) device: Device,
    pub(crate) engine: Engine,
    pub(crate) queue: Queue,
}

impl ImmediateGpu {
    pub(crate) fn new() -> Result<Self, String> {
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
            device.clone(),
            queue.clone(),
            IMMEDIATE_FORMAT,
            None,
            Shell::headless(),
        );
        Ok(Self {
            device,
            engine,
            queue,
        })
    }

    pub(crate) fn texture(&self) -> Texture {
        self.device.create_texture(&TextureDescriptor {
            label: Some("kithara_ui.page_perf.immediate"),
            size: Extent3d {
                width: width(),
                height: height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: IMMEDIATE_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}

/// The retained host's own device, Vello renderer, and the two native passes the
/// application runs around the scene.
pub(crate) struct RetainedGpu {
    pub(crate) device: vello_wgpu::Device,
    pub(crate) queue: vello_wgpu::Queue,
    pub(crate) shaders: ShaderPass,
    pub(crate) texture: vello_wgpu::Texture,
    vello: VelloRenderer,
    pub(crate) vis: VisPass,
}

impl RetainedGpu {
    pub(crate) fn new() -> Result<Self, String> {
        const RETAINED_FORMAT: vello_wgpu::TextureFormat = vello_wgpu::TextureFormat::Rgba8Unorm;

        let instance = vello_wgpu::Instance::new(&vello_wgpu::InstanceDescriptor {
            backends: vello_wgpu::Backends::PRIMARY,
            ..vello_wgpu::InstanceDescriptor::default()
        });
        let adapter =
            block_on(instance.request_adapter(&vello_wgpu::RequestAdapterOptions::default()))
                .map_err(|error| format!("no wgpu adapter: {error}"))?;
        let (device, queue) =
            block_on(adapter.request_device(&vello_wgpu::DeviceDescriptor::default()))
                .map_err(|error| format!("no wgpu device: {error}"))?;
        let vello = VelloRenderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|error| format!("vello renderer: {error}"))?;
        let texture = device.create_texture(&vello_wgpu::TextureDescriptor {
            label: Some("kithara_ui.page_perf.retained"),
            size: vello_wgpu::Extent3d {
                width: width(),
                height: height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello_wgpu::TextureDimension::D2,
            format: RETAINED_FORMAT,
            usage: vello_wgpu::TextureUsages::STORAGE_BINDING
                | vello_wgpu::TextureUsages::RENDER_ATTACHMENT
                | vello_wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let shaders = ShaderPass::new(&device);
        let vis = VisPass::new(&device, RETAINED_FORMAT);
        Ok(Self {
            device,
            queue,
            shaders,
            texture,
            vello,
            vis,
        })
    }

    /// The same three passes with the queue drained after each, so each label
    /// carries its own GPU time instead of the next pass's submit.
    pub(crate) fn fenced_passes(&mut self, frame: &Frame) {
        let view = self
            .texture
            .create_view(&vello_wgpu::TextureViewDescriptor::default());
        measure_block!("vello.pass.shader.gpu.fenced", {
            self.shaders
                .render(&self.device, &self.queue, &mut self.vello, frame.shaders());
            drain_vello(&self.device);
        });
        measure_block!("vello.scene.gpu.fenced", {
            self.scene(frame, &view);
            drain_vello(&self.device);
        });
        measure_block!("vello.pass.vis.gpu.fenced", {
            self.vis.render(
                &self.device,
                &self.queue,
                &view,
                frame.vis(),
                1.0,
                [width(), height()],
            );
            drain_vello(&self.device);
        });
    }

    /// Shader images, then the scene, then the native visualiser draws - the
    /// order the window runner uses.
    pub(crate) fn passes(&mut self, frame: &Frame) {
        let view = self
            .texture
            .create_view(&vello_wgpu::TextureViewDescriptor::default());
        measure_block!(
            "vello.pass.shader",
            self.shaders
                .render(&self.device, &self.queue, &mut self.vello, frame.shaders())
        );
        measure_block!("vello.scene", self.scene(frame, &view));
        measure_block!(
            "vello.pass.vis",
            self.vis.render(
                &self.device,
                &self.queue,
                &view,
                frame.vis(),
                1.0,
                [width(), height()],
            )
        );
    }

    fn scene(&mut self, frame: &Frame, view: &vello_wgpu::TextureView) {
        let background = paint_color(builtin::skin().palette.bg);
        self.vello
            .render_to_texture(
                &self.device,
                &self.queue,
                frame.scene(),
                view,
                &RenderParams {
                    base_color: background,
                    width: width(),
                    height: height(),
                    antialiasing_method: AaConfig::Area,
                },
            )
            .unwrap_or_else(|error| panic!("the retained host must rasterise: {error}"));
    }
}

/// Which host a case measures, and the device it needs before it can.
pub(crate) enum Gpu {
    Immediate(Box<ImmediateGpu>),
    Retained(Box<RetainedGpu>),
}

pub(crate) fn drain(device: &Device) {
    device
        .poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap_or_else(|error| panic!("the immediate queue must drain: {error}"));
}

fn drain_vello(device: &vello_wgpu::Device) {
    device
        .poll(vello_wgpu::PollType::Wait)
        .unwrap_or_else(|error| panic!("the retained queue must drain: {error}"));
}

/// The page every run is laid out and rasterised at: the gallery's own window
/// size at 1x, so a page measured here is the page the application shows.
pub(crate) fn width() -> u32 {
    WIDTH.as_()
}

pub(crate) fn height() -> u32 {
    HEIGHT.as_()
}

fn unpadded_row() -> u32 {
    width() * 4
}

/// wgpu requires each copied row to start on a 256-byte boundary.
fn padded_row() -> u32 {
    unpadded_row().div_ceil(256) * 256
}

pub(crate) fn readback(device: &Device, queue: &Queue, texture: &Texture) -> Vec<u8> {
    let padded = padded_row();
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("kithara_ui.page_perf.immediate.readback"),
        size: u64::from(padded) * u64::from(height()),
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
                rows_per_image: Some(height()),
            },
        },
        Extent3d {
            width: width(),
            height: height(),
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(MapMode::Read, |_| {});
    drain(device);
    let mapped = slice.get_mapped_range();
    let rgba = rows(&mapped);
    drop(mapped);
    buffer.unmap();
    rgba
}

pub(crate) fn readback_vello(
    device: &vello_wgpu::Device,
    queue: &vello_wgpu::Queue,
    texture: &vello_wgpu::Texture,
) -> Vec<u8> {
    let padded = padded_row();
    let buffer = device.create_buffer(&vello_wgpu::BufferDescriptor {
        label: Some("kithara_ui.page_perf.retained.readback"),
        size: u64::from(padded) * u64::from(height()),
        usage: vello_wgpu::BufferUsages::MAP_READ | vello_wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&vello_wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello_wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: vello_wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height()),
            },
        },
        vello_wgpu::Extent3d {
            width: width(),
            height: height(),
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(vello_wgpu::MapMode::Read, |_| {});
    drain_vello(device);
    let mapped = slice.get_mapped_range();
    let rgba = rows(&mapped);
    drop(mapped);
    buffer.unmap();
    rgba
}

fn rows(mapped: &[u8]) -> Vec<u8> {
    let unpadded = usize::try_from(unpadded_row()).unwrap_or(0);
    let padded = usize::try_from(padded_row()).unwrap_or(0);
    let height = usize::try_from(height()).unwrap_or(0);
    let mut rgba = Vec::with_capacity(unpadded * height);
    for row in 0..height {
        let start = row * padded;
        rgba.extend_from_slice(&mapped[start..start + unpadded]);
    }
    rgba
}

pub(crate) fn digest(pixels: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    pixels.hash(&mut hasher);
    hasher.finish()
}

/// Whether the frame is more than one flat colour. A run that rasterised
/// nothing reports a beautiful number for nothing.
pub(crate) fn painted(pixels: &[u8]) -> bool {
    let Some(first) = pixels.get(..4) else {
        return false;
    };
    pixels.chunks_exact(4).any(|pixel| pixel != first)
}
