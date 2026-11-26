use std::collections::{BTreeMap, BTreeSet};

use kithara_platform::sync::Arc;
use masonry::vello::{
    Renderer,
    peniko::ImageData,
    wgpu,
    wgpu::{
        BindingType, BufferBindingType, BufferUsages, Color, ColorWrites, LoadOp, MultisampleState,
        Origin3d, PipelineCompilationOptions, PrimitiveState, PrimitiveTopology, ShaderSource,
        ShaderStages, StoreOp, TextureAspect, TextureDimension, TextureFormat, TextureUsages,
        TextureViewDescriptor,
    },
};
use num_traits::cast::AsPrimitive as _;

use crate::{draw::ImageId, hosts::shader::ShaderFrame};

/// One retained shader image referenced by the Vello scene for this frame.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ShaderDeclaration {
    image: ImageData,
    frame: ShaderFrame,
}

impl ShaderDeclaration {
    pub(crate) const fn new(frame: ShaderFrame, image: ImageData) -> Self {
        Self { image, frame }
    }
}

/// Executes retained document shaders into reusable textures before Vello.
#[non_exhaustive]
pub struct ShaderPass {
    programs: BTreeMap<Arc<str>, wgpu::RenderPipeline>,
    slots: BTreeMap<ImageId, Slot>,
    active: BTreeSet<ImageId>,
    uniform_layout: wgpu::BindGroupLayout,
    stale: Vec<ImageId>,
}

impl ShaderPass {
    /// Creates an empty pass. Programs and images are allocated lazily from the
    /// declarations actually present in a frame.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            active: BTreeSet::new(),
            programs: BTreeMap::new(),
            slots: BTreeMap::new(),
            stale: Vec::new(),
            uniform_layout: uniform_layout(device),
        }
    }

    fn release_stale(&mut self, renderer: &mut Renderer) {
        self.stale.clear();
        self.stale.extend(
            self.slots
                .keys()
                .filter(|image| !self.active.contains(*image))
                .cloned(),
        );
        for image in self.stale.drain(..) {
            let Some(slot) = self.slots.remove(&image) else {
                continue;
            };
            if let Some(image) = slot.image {
                drop(renderer.override_image(&image, None));
            }
        }
    }

    /// Renders every declaration and installs its GPU texture into the Vello
    /// renderer's image registry. The following Vello render consumes those
    /// images at their scene-authored clip and z-order positions.
    ///
    /// Everything here is CPU work up to the submit; the shader programs
    /// themselves are timed by whoever fences the queue.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        declarations: &[ShaderDeclaration],
    ) {
        self.active.clear();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kithara_ui.shader.retained.encoder"),
        });
        let mut drew = false;
        for declaration in declarations {
            let frame = &declaration.frame;
            let size = [declaration.image.width, declaration.image.height];
            if size[0] == 0 || size[1] == 0 {
                tracing::error!(image = ?frame.image(), "retained shader declared an empty image");
                continue;
            }
            self.active.insert(frame.image().clone());
            let source = frame.source();
            if !self.programs.contains_key(source) {
                self.programs.insert(
                    source.clone(),
                    program_pipeline(device, &self.uniform_layout, source),
                );
            }
            let slot = self.slots.entry(frame.image().clone()).or_insert_with(|| {
                Slot::new(
                    device,
                    &self.uniform_layout,
                    source.clone(),
                    frame.uniform_byte_len(),
                    size,
                )
            });
            if slot.source != *source
                || slot.uniform_size != frame.uniform_byte_len()
                || slot.size != size
            {
                if let Some(image) = slot.image.take() {
                    drop(renderer.override_image(&image, None));
                }
                *slot = Slot::new(
                    device,
                    &self.uniform_layout,
                    source.clone(),
                    frame.uniform_byte_len(),
                    size,
                );
            }
            if slot.image.as_ref() != Some(&declaration.image) {
                if let Some(image) = slot.image.take() {
                    drop(renderer.override_image(&image, None));
                }
                drop(renderer.override_image(
                    &declaration.image,
                    Some(wgpu::TexelCopyTextureInfoBase {
                        texture: slot.texture.clone(),
                        mip_level: 0,
                        origin: Origin3d::ZERO,
                        aspect: TextureAspect::All,
                    }),
                ));
                slot.image = Some(declaration.image.clone());
            }
            let viewport: [f32; 2] = [size[0].as_(), size[1].as_()];
            frame.write_uniforms(viewport, &mut slot.uniform_bytes);
            queue.write_buffer(&slot.uniform_buffer, 0, &slot.uniform_bytes);
            let Some(program) = self.programs.get(source) else {
                continue;
            };
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("kithara_ui.shader.retained.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &slot.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: LoadOp::Clear(Color::TRANSPARENT),
                            store: StoreOp::Store,
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
            drew = true;
        }
        if drew {
            queue.submit([encoder.finish()]);
        }
        self.release_stale(renderer);
    }
}

struct Slot {
    source: Arc<str>,
    uniform_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    image: Option<ImageData>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    uniform_bytes: Vec<u8>,
    size: [u32; 2],
    uniform_size: usize,
}

impl Slot {
    fn new(
        device: &wgpu::Device,
        uniform_layout: &wgpu::BindGroupLayout,
        source: Arc<str>,
        uniform_size: usize,
        size: [u32; 2],
    ) -> Self {
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.shader.retained.uniforms"),
            size: uniform_size.as_(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kithara_ui.shader.retained.uniform_bind_group"),
            layout: uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kithara_ui.shader.retained.image"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());
        Self {
            image: None,
            size,
            source,
            texture,
            uniform_bind_group,
            uniform_buffer,
            uniform_bytes: Vec::with_capacity(uniform_size),
            uniform_size,
            view,
        }
    }
}

fn uniform_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kithara_ui.shader.retained.uniform_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn program_pipeline(
    device: &wgpu::Device,
    uniform_layout: &wgpu::BindGroupLayout,
    source: &str,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kithara_ui.shader.retained.layout"),
        bind_group_layouts: &[uniform_layout],
        push_constant_ranges: &[],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kithara_ui.shader.retained.module"),
        source: ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kithara_ui.shader.retained.pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: PrimitiveState {
            topology: PrimitiveTopology::TriangleList,
            ..PrimitiveState::default()
        },
        depth_stencil: None,
        multisample: MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}
