use std::sync::atomic::{AtomicUsize, Ordering};

use iced::{
    Element, Length, Rectangle, wgpu,
    widget::{Space, shader},
};

use super::{SHADER, Uniforms, VisFrame};
use crate::render::{ReadValue, UiEvent, document::Ctx};

pub(crate) fn view<'a>(preset: Option<&ReadValue<'_>>, ctx: Ctx<'_, '_>) -> Element<'a, UiEvent> {
    let Some(frame) = VisFrame::read(preset.copied(), &ctx) else {
        return Space::new().into();
    };
    shader::Shader::new(Program(frame))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[derive(Clone, Copy, Debug)]
struct Program(VisFrame);

impl shader::Program<UiEvent> for Program {
    type Primitive = Primitive;
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: iced::mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        Primitive::new(self.0)
    }
}

#[derive(Debug)]
struct Primitive {
    slot: AtomicUsize,
    frame: VisFrame,
}

impl Primitive {
    fn new(frame: VisFrame) -> Self {
        Self {
            frame,
            slot: AtomicUsize::new(usize::MAX),
        }
    }
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.vis.encode"))]
    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = pipeline.slots.get(self.slot.load(Ordering::Relaxed)) else {
            return true;
        };
        render_pass.set_pipeline(&pipeline.render_pipeline);
        render_pass.set_bind_group(0, &slot.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }

    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.vis.prepare"))]
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let index = pipeline.prepared;
        pipeline.prepared += 1;
        if index == pipeline.slots.len() {
            pipeline
                .slots
                .push(UniformSlot::new(device, &pipeline.bind_group_layout));
        }
        let Some(slot) = pipeline.slots.get(index) else {
            return;
        };
        let scale = viewport.scale_factor();
        let uniforms = Uniforms::new(
            self.frame,
            [bounds.x * scale, bounds.y * scale],
            [bounds.width * scale, bounds.height * scale],
        );
        queue.write_buffer(&slot.buffer, 0, &uniforms.bytes());
        self.slot.store(index, Ordering::Relaxed);
    }
}

struct Pipeline {
    bind_group_layout: wgpu::BindGroupLayout,
    render_pipeline: wgpu::RenderPipeline,
    slots: Vec<UniformSlot>,
    prepared: usize,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kithara_ui.vis.bind_group_layout"),
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
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kithara_ui.vis.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kithara_ui.vis.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kithara_ui.vis.pipeline"),
            layout: Some(&pipeline_layout),
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
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Self {
            bind_group_layout,
            render_pipeline,
            prepared: 0,
            slots: Vec::new(),
        }
    }

    fn trim(&mut self) {
        self.prepared = 0;
    }
}

struct UniformSlot {
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
}

impl UniformSlot {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.vis.uniforms"),
            size: Uniforms::BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout,
            label: Some("kithara_ui.vis.bind_group"),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self { bind_group, buffer }
    }
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use iced::wgpu::util::DeviceExt as _;
    use kithara_test_utils::kithara;

    use super::*;

    fn render(uniforms: Uniforms) -> Vec<u8> {
        const SIDE: u32 = 64;
        const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
        const ROW_BYTES: u32 = SIDE * 4;

        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("this lane runs where a graphics device exists");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("the adapter must give up a device");

        let pipeline = <Pipeline as shader::Pipeline>::new(&device, &queue, FORMAT);
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("kithara_ui.vis.test.uniforms"),
            contents: &uniforms.bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kithara_ui.vis.test.bind_group"),
            layout: &pipeline.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kithara_ui.vis.test.target"),
            size: wgpu::Extent3d {
                width: SIDE,
                height: SIDE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.vis.test.readback"),
            size: u64::from(ROW_BYTES * SIDE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kithara_ui.vis.test.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline.render_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW_BYTES),
                    rows_per_image: Some(SIDE),
                },
            },
            wgpu::Extent3d {
                width: SIDE,
                height: SIDE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("the readback buffer must map");
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("the queue drains");
        let pixels = readback.slice(..).get_mapped_range().to_vec();
        readback.unmap();
        pixels
    }

    fn uniforms(level: f32, preset: u32) -> Uniforms {
        Uniforms::new(VisFrame::new(level, 0.5, preset), [0.0, 0.0], [64.0, 64.0])
    }

    #[kithara::test]
    fn the_visualiser_draws_what_the_uniform_block_says() {
        let silent = render(uniforms(0.0, 0));
        let loud = render(uniforms(1.0, 0));
        let other_preset = render(uniforms(1.0, 1));

        assert_eq!(silent, render(uniforms(0.0, 0)));
        assert!(silent.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
        assert_ne!(silent, loud, "the level did not reach the shader");
        assert_ne!(loud, other_preset, "the preset did not reach the shader");
    }
}
