use masonry::vello::wgpu;
use num_traits::ToPrimitive;

use super::{SHADER, Uniforms, VisFrame};

/// One retained visualiser draw in logical window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct VisDeclaration {
    frame: VisFrame,
    rect: [f64; 4],
}

impl VisDeclaration {
    pub(crate) fn logical(frame: VisFrame, rect: [f64; 4]) -> Option<Self> {
        if rect.iter().any(|value| !value.is_finite()) || rect[2] <= rect[0] || rect[3] <= rect[1] {
            return None;
        }
        Some(Self { frame, rect })
    }

    /// Values read for this leaf in the current frame.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn frame(self) -> VisFrame {
        self.frame
    }

    /// Unclipped logical rectangle as left, top, right, and bottom.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn rect(self) -> [f64; 4] {
        self.rect
    }

    fn prepare(self, scale: f64, target: [u32; 2]) -> Option<PreparedDraw> {
        if !scale.is_finite() || scale <= 0.0 || target[0] == 0 || target[1] == 0 {
            return None;
        }
        let [left, top, right, bottom] = self.rect;
        if [left, top, right, bottom]
            .iter()
            .any(|value| !value.is_finite())
            || right <= left
            || bottom <= top
        {
            return None;
        }

        let physical = [left * scale, top * scale, right * scale, bottom * scale];
        if physical.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let origin = [physical_f32(physical[0])?, physical_f32(physical[1])?];
        let resolution = [
            physical_f32((right - left) * scale)?,
            physical_f32((bottom - top) * scale)?,
        ];

        let clipped = [
            physical[0].max(0.0),
            physical[1].max(0.0),
            physical[2].min(f64::from(target[0])),
            physical[3].min(f64::from(target[1])),
        ];
        if clipped[2] <= clipped[0] || clipped[3] <= clipped[1] {
            return None;
        }
        let x0 = clipped[0].round().to_u32()?;
        let y0 = clipped[1].round().to_u32()?;
        let x1 = clipped[2].round().to_u32()?;
        let y1 = clipped[3].round().to_u32()?;
        let width = x1.checked_sub(x0)?;
        let height = y1.checked_sub(y0)?;
        (width > 0 && height > 0).then_some(PreparedDraw {
            uniforms: Uniforms::new(self.frame, origin, resolution),
            scissor: [x0, y0, width, height],
        })
    }
}

fn physical_f32(value: f64) -> Option<f32> {
    value
        .to_f32()
        .filter(|converted| converted.is_finite() && (*converted != 0.0 || value == 0.0))
}

struct PreparedDraw {
    uniforms: Uniforms,
    scissor: [u32; 4],
}

/// Thin wgpu 26 pass for retained visualiser declarations.
#[non_exhaustive]
pub struct VisPass {
    bind_group_layout: wgpu::BindGroupLayout,
    draws: Vec<PreparedDraw>,
    pipeline: wgpu::RenderPipeline,
    slots: Vec<UniformSlot>,
}

impl VisPass {
    /// Builds the retained pass for the Vello intermediate target format.
    #[must_use]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kithara_ui.vis.retained.bind_group_layout"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kithara_ui.vis.retained.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kithara_ui.vis.retained.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kithara_ui.vis.retained.pipeline"),
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
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Self {
            bind_group_layout,
            draws: Vec::new(),
            pipeline,
            slots: Vec::new(),
        }
    }

    /// Converts logical declarations at `scale`, clips their scissors to the
    /// physical target size, and draws them after Vello.
    ///
    /// Everything here is CPU work: filtering, preparing, writing uniforms and
    /// recording the pass. The submitted commands are timed by whoever fences
    /// the queue, which is a different measurement and a different scenario.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "vis.pass.cpu"))]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        declarations: &[VisDeclaration],
        scale: f64,
        target_size: [u32; 2],
    ) {
        self.draws.clear();
        self.draws.extend(
            declarations
                .iter()
                .filter_map(|declaration| declaration.prepare(scale, target_size)),
        );
        if self.draws.is_empty() {
            return;
        }
        while self.slots.len() < self.draws.len() {
            self.slots
                .push(UniformSlot::new(device, &self.bind_group_layout));
        }
        for (draw, slot) in self.draws.iter().zip(&self.slots) {
            queue.write_buffer(&slot.buffer, 0, &draw.uniforms.bytes());
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kithara_ui.vis.retained.encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kithara_ui.vis.retained.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (draw, slot) in self.draws.iter().zip(&self.slots) {
                let [x, y, width, height] = draw.scissor;
                pass.set_scissor_rect(x, y, width, height);
                pass.set_bind_group(0, &slot.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
    }
}

struct UniformSlot {
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
}

impl UniformSlot {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.vis.retained.uniforms"),
            size: Uniforms::BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kithara_ui.vis.retained.bind_group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self { bind_group, buffer }
    }
}

#[cfg(test)]
mod declaration_tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn declaration(rect: [f64; 4]) -> VisDeclaration {
        VisDeclaration {
            frame: VisFrame::new(0.75, 1.25, 2),
            rect,
        }
    }

    #[kithara::test]
    fn fractional_geometry_keeps_exact_uniforms_and_snaps_both_scissor_corners_nearest() {
        let declaration = declaration([1.6, 2.6, 7.4, 8.4]);
        let draw = declaration
            .prepare(1.0, [100, 100])
            .unwrap_or_else(|| panic!("valid fractional geometry must prepare"));

        assert_eq!(
            draw.uniforms.bytes(),
            Uniforms::new(declaration.frame(), [1.6, 2.6], [5.8, 5.8]).bytes()
        );
        assert_eq!(draw.scissor, [2, 3, 5, 5]);
    }

    #[kithara::test]
    fn offscreen_geometry_clips_exactly_before_nearest_corner_snapping() {
        let declaration = declaration([-2.5, 1.6, 7.4, 10.0]);
        let draw = declaration
            .prepare(1.0, [6, 8])
            .unwrap_or_else(|| panic!("a partially visible declaration must prepare"));

        assert_eq!(
            draw.uniforms.bytes(),
            Uniforms::new(declaration.frame(), [-2.5, 1.6], [9.9, 8.4]).bytes()
        );
        assert_eq!(draw.scissor, [0, 2, 6, 6]);
    }

    #[kithara::test]
    fn invalid_or_empty_declarations_do_not_prepare() {
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                declaration([0.0, 0.0, 10.0, 10.0])
                    .prepare(scale, [20, 20])
                    .is_none()
            );
        }
        for rect in [
            [0.0, 0.0, 0.0, 10.0],
            [0.0, 0.0, 10.0, 0.0],
            [10.0, 0.0, 0.0, 10.0],
            [0.0, 10.0, 10.0, 0.0],
            [f64::NAN, 0.0, 10.0, 10.0],
            [f64::NEG_INFINITY, 0.0, 10.0, 10.0],
        ] {
            assert!(declaration(rect).prepare(1.0, [20, 20]).is_none());
            assert!(VisDeclaration::logical(declaration(rect).frame, rect).is_none());
        }
        assert!(
            declaration([21.0, 0.0, 30.0, 10.0])
                .prepare(1.0, [20, 20])
                .is_none()
        );
        assert!(
            declaration([0.0, 0.0, f64::from(f32::MAX), 10.0])
                .prepare(2.0, [20, 20])
                .is_none()
        );
        assert!(
            declaration([0.0, 0.0, f64::MIN_POSITIVE, 10.0])
                .prepare(1.0, [20, 20])
                .is_none()
        );
        assert!(
            declaration([0.1, 0.1, 0.4, 0.4])
                .prepare(1.0, [20, 20])
                .is_none()
        );
        assert!(
            declaration([0.0, 0.0, 10.0, 10.0])
                .prepare(1.0, [0, 20])
                .is_none()
        );
    }
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn retained_pass_changes_only_pixels_inside_the_leaf_scissor() {
        const SIDE: u32 = 64;
        const ROW_BYTES: u32 = SIDE * 4;
        const CLEAR: [u8; 4] = [0, 0, 0, 255];

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("this lane runs where a graphics device exists");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("the adapter must provide a device");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kithara_ui.vis.retained.test.target"),
            size: wgpu::Extent3d {
                width: SIDE,
                height: SIDE,
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
        let mut clear = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = clear.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kithara_ui.vis.retained.test.clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        queue.submit([clear.finish()]);

        let mut pass = VisPass::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        pass.render(
            &device,
            &queue,
            &view,
            &[
                VisDeclaration::logical(VisFrame::new(1.0, 0.5, 0), [16.0, 16.0, 48.0, 48.0])
                    .unwrap_or_else(|| panic!("the test rectangle must be valid")),
            ],
            1.0,
            [SIDE, SIDE],
        );

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kithara_ui.vis.retained.test.readback"),
            size: u64::from(ROW_BYTES * SIDE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut copy = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        copy.copy_texture_to_buffer(
            texture.as_image_copy(),
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
        queue.submit([copy.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("the retained readback must map");
        });
        device
            .poll(wgpu::PollType::Wait)
            .expect("the retained queue must drain");
        let pixels = readback.slice(..).get_mapped_range();
        let pixel = |x: u32, y: u32| {
            let offset = ((y * SIDE + x) * 4) as usize;
            &pixels[offset..offset + 4]
        };

        let mut changed_inside = false;
        for y in 0..SIDE {
            for x in 0..SIDE {
                if (16..48).contains(&x) && (16..48).contains(&y) {
                    changed_inside |= pixel(x, y) != CLEAR;
                } else {
                    assert_eq!(
                        pixel(x, y),
                        CLEAR,
                        "the Vis pass changed a pixel outside its scissor at ({x}, {y})"
                    );
                }
            }
        }
        assert!(changed_inside, "the Vis rectangle was not drawn");
        drop(pixels);
        readback.unmap();
    }
}
