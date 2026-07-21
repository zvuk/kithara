use std::{
    borrow::Cow,
    sync::atomic::{AtomicUsize, Ordering},
};

use iced::{Rectangle, wgpu, widget::shader};

#[derive(Debug)]
pub(super) struct VisPrimitive {
    level: f32,
    preset: u32,
    slot: AtomicUsize,
    time: f32,
}

impl VisPrimitive {
    pub(super) fn new(level: f32, preset: u32, time: f32) -> Self {
        Self {
            level,
            preset,
            slot: AtomicUsize::new(usize::MAX),
            time,
        }
    }
}

impl shader::Primitive for VisPrimitive {
    type Pipeline = VisPipeline;

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
        let uniforms = Uniforms {
            resolution: [bounds.width * scale, bounds.height * scale],
            origin: [bounds.x * scale, bounds.y * scale],
            time: self.time,
            level: self.level,
            preset: self.preset,
        };
        queue.write_buffer(&slot.buffer, 0, &uniforms.bytes());
        self.slot.store(index, Ordering::Relaxed);
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = pipeline.slots.get(self.slot.load(Ordering::Relaxed)) else {
            return true;
        };
        render_pass.set_pipeline(&pipeline.render_pipeline);
        render_pass.set_bind_group(0, &slot.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}

pub(super) struct VisPipeline {
    bind_group_layout: wgpu::BindGroupLayout,
    prepared: usize,
    render_pipeline: wgpu::RenderPipeline,
    slots: Vec<UniformSlot>,
}

impl shader::Pipeline for VisPipeline {
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
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../../../assets/shaders/vis.wgsl"
            ))),
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
            prepared: 0,
            render_pipeline,
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
    const BUFFER_SIZE: u64 = Uniforms::BYTE_COUNT as u64;

    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.vis.uniforms"),
            size: Self::BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kithara_ui.vis.bind_group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self { bind_group, buffer }
    }
}

#[derive(Clone, Copy)]
struct Uniforms {
    resolution: [f32; 2],
    origin: [f32; 2],
    time: f32,
    level: f32,
    preset: u32,
}

impl Uniforms {
    const BYTE_COUNT: usize = 32;

    fn bytes(self) -> [u8; Self::BYTE_COUNT] {
        let mut bytes = [0; Self::BYTE_COUNT];
        for (index, value) in [
            self.resolution[0],
            self.resolution[1],
            self.origin[0],
            self.origin[1],
            self.time,
            self.level,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = index * size_of::<f32>();
            bytes[offset..offset + size_of::<f32>()].copy_from_slice(&value.to_ne_bytes());
        }
        bytes[24..28].copy_from_slice(&self.preset.to_ne_bytes());
        bytes
    }
}
