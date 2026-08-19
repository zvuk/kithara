use std::collections::BTreeMap;

use iced::{
    Element, Length, Rectangle, wgpu,
    widget::{Space, shader},
};
use num_traits::cast::AsPrimitive as _;

use super::{ShaderFrame, logical_extent};
use crate::{
    draw::ImageId,
    render::{UiEvent, document::Ctx},
    shader::ShaderSpec,
};

const COMPOSITE: &str = r#"
@group(0) @binding(0) var image_sampler: sampler;
@group(0) @binding(1) var image: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    var output: VertexOutput;
    output.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    output.uv = vec2<f32>(x, y);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv);
}
"#;

pub(crate) fn view<'a>(spec: &ShaderSpec, path: &str, ctx: Ctx<'_, '_>) -> Element<'a, UiEvent> {
    let frame = match ShaderFrame::read(spec, path, &ctx, ctx.ui) {
        Ok(frame) => frame,
        Err(error) => {
            tracing::error!(%error, "document shader did not produce a frame");
            return Space::new().into();
        }
    };
    shader::Shader::new(Program(frame))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[derive(Clone, Debug)]
struct Program(ShaderFrame);

impl shader::Program<UiEvent> for Program {
    type Primitive = Primitive;
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: iced::mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        Primitive(self.0.clone())
    }
}

#[derive(Debug)]
struct Primitive(ShaderFrame);

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    /// The immediate host draws the document shader in two passes: this one
    /// pastes the offscreen image [`Self::prepare`] filled. The retained host
    /// has no counterpart - it hands Vello the same image and Vello composites
    /// it - so this cost belongs to the split, not to either host being slower.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.shader.composite"))]
    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = pipeline.slots.get(self.0.image()) else {
            return true;
        };
        render_pass.set_pipeline(&pipeline.composite);
        render_pass.set_bind_group(0, &slot.texture_bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }

    /// Resolves the program, the image slot and the uniforms for this frame,
    /// then runs the document's own fragment into that image.
    ///
    /// The two halves are timed apart: everything up to the uniform write is
    /// bookkeeping this host does per frame, and [`offscreen`] is the shader
    /// itself. Folding them together would report a slow shader for a slow
    /// lookup. The outer label encloses the inner one, so the bookkeeping is
    /// their difference, not the outer number.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.shader.prepare"))]
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &shader::Viewport,
    ) {
        let Some(size) = logical_extent(bounds.width, bounds.height) else {
            pipeline.slots.remove(self.0.image());
            return;
        };
        let source = self.0.source();
        if !pipeline.programs.contains_key(source) {
            pipeline.programs.insert(
                source.clone(),
                program_pipeline(device, &pipeline.uniform_layout, source),
            );
        }
        let slot = pipeline
            .slots
            .entry(self.0.image().clone())
            .or_insert_with(|| {
                Slot::new(
                    device,
                    &pipeline.uniform_layout,
                    &pipeline.texture_layout,
                    &pipeline.sampler,
                    source.clone(),
                    self.0.uniform_byte_len(),
                    size,
                )
            });
        if slot.source != *source || slot.uniform_size != self.0.uniform_byte_len() {
            *slot = Slot::new(
                device,
                &pipeline.uniform_layout,
                &pipeline.texture_layout,
                &pipeline.sampler,
                source.clone(),
                self.0.uniform_byte_len(),
                size,
            );
        } else if slot.size != size {
            slot.resize(device, &pipeline.texture_layout, &pipeline.sampler, size);
        }
        let Some(program) = pipeline.programs.get(source) else {
            return;
        };
        let viewport: [f32; 2] = [size[0].as_(), size[1].as_()];
        self.0.write_uniforms(viewport, &mut slot.uniform_bytes);
        queue.write_buffer(&slot.uniform_buffer, 0, &slot.uniform_bytes);
        offscreen(device, queue, program, slot);
    }
}

/// Runs one document fragment into the slot's own texture, on its own submit.
#[cfg_attr(feature = "perf", hotpath::measure(label = "iced.shader.offscreen"))]
fn offscreen(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    program: &wgpu::RenderPipeline,
    slot: &Slot,
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("kithara_ui.shader.iced.encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kithara_ui.shader.iced.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &slot.texture_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(program);
        pass.set_bind_group(0, &slot.uniform_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
}

struct Pipeline {
    composite: wgpu::RenderPipeline,
    programs: BTreeMap<kithara_platform::sync::Arc<str>, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    slots: BTreeMap<ImageId, Slot>,
    texture_layout: wgpu::BindGroupLayout,
    uniform_layout: wgpu::BindGroupLayout,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let uniform_layout = uniform_layout(device);
        let texture_layout = texture_layout(device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kithara_ui.shader.iced.sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let composite = composite_pipeline(device, &texture_layout, format);
        Self {
            composite,
            programs: BTreeMap::new(),
            sampler,
            slots: BTreeMap::new(),
            texture_layout,
            uniform_layout,
        }
    }
}

struct Slot {
    size: [u32; 2],
    source: kithara_platform::sync::Arc<str>,
    texture_bind_group: wgpu::BindGroup,
    texture_view: wgpu::TextureView,
    uniform_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_bytes: Vec<u8>,
    uniform_size: usize,
}

impl Slot {
    fn new(
        device: &wgpu::Device,
        uniform_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        source: kithara_platform::sync::Arc<str>,
        uniform_size: usize,
        size: [u32; 2],
    ) -> Self {
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.shader.iced.uniforms"),
            size: uniform_size.as_(),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kithara_ui.shader.iced.uniform_bind_group"),
            layout: uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let (texture_view, texture_bind_group) = texture(
            device,
            texture_layout,
            sampler,
            size,
            "kithara_ui.shader.iced.image",
        );
        Self {
            size,
            source,
            texture_bind_group,
            texture_view,
            uniform_bind_group,
            uniform_buffer,
            uniform_bytes: Vec::with_capacity(uniform_size),
            uniform_size,
        }
    }

    fn resize(
        &mut self,
        device: &wgpu::Device,
        texture_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        size: [u32; 2],
    ) {
        (self.texture_view, self.texture_bind_group) = texture(
            device,
            texture_layout,
            sampler,
            size,
            "kithara_ui.shader.iced.image",
        );
        self.size = size;
    }
}

fn uniform_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kithara_ui.shader.iced.uniform_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn texture_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kithara_ui.shader.iced.texture_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

fn program_pipeline(
    device: &wgpu::Device,
    uniform_layout: &wgpu::BindGroupLayout,
    source: &str,
) -> wgpu::RenderPipeline {
    render_pipeline(
        device,
        uniform_layout,
        source,
        wgpu::TextureFormat::Rgba8Unorm,
        None,
        "kithara_ui.shader.iced.program",
    )
}

fn composite_pipeline(
    device: &wgpu::Device,
    texture_layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    render_pipeline(
        device,
        texture_layout,
        COMPOSITE,
        format,
        Some(wgpu::BlendState::ALPHA_BLENDING),
        "kithara_ui.shader.iced.composite",
    )
}

fn render_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    source: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    label: &'static str,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

fn texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    size: [u32; 2],
    label: &'static str,
) -> (wgpu::TextureView, wgpu::BindGroup) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
        ],
    });
    (view, bind_group)
}
