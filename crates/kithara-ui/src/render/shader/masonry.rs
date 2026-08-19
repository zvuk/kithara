use std::collections::{BTreeMap, BTreeSet};

use kithara_platform::sync::Arc;
use masonry::vello::{Renderer, peniko::ImageData, wgpu};
use num_traits::cast::AsPrimitive as _;

use super::ShaderFrame;
use crate::draw::ImageId;

/// One retained shader image referenced by the Vello scene for this frame.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ShaderDeclaration {
    frame: ShaderFrame,
    image: ImageData,
}

impl ShaderDeclaration {
    pub(crate) const fn new(frame: ShaderFrame, image: ImageData) -> Self {
        Self { frame, image }
    }
}

/// Executes retained document shaders into reusable textures before Vello.
#[non_exhaustive]
pub struct ShaderPass {
    active: BTreeSet<ImageId>,
    programs: BTreeMap<Arc<str>, wgpu::RenderPipeline>,
    slots: BTreeMap<ImageId, Slot>,
    stale: Vec<ImageId>,
    uniform_layout: wgpu::BindGroupLayout,
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

    /// Renders every declaration and installs its GPU texture into the Vello
    /// renderer's image registry. The following Vello render consumes those
    /// images at their scene-authored clip and z-order positions.
    ///
    /// Everything here is CPU work up to the submit; the shader programs
    /// themselves are timed by whoever fences the queue.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "shader.pass.cpu"))]
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
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
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
            drew = true;
        }
        if drew {
            queue.submit([encoder.finish()]);
        }
        self.release_stale(renderer);
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
}

struct Slot {
    image: Option<ImageData>,
    size: [u32; 2],
    source: Arc<str>,
    texture: wgpu::Texture,
    uniform_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_bytes: Vec<u8>,
    uniform_size: usize,
    view: wgpu::TextureView,
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
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
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
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kithara_ui.shader.retained.pipeline"),
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
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}
