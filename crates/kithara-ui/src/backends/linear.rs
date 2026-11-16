//! Linear gradient meshes split at colour stops before GPU interpolation.

use std::mem::size_of;

use bytemuck::cast;
use half::f16;
use iced::advanced::graphics::{
    gradient::Packed,
    mesh::{Indexed, Mesh, SolidVertex2D},
};
use num_traits::cast::AsPrimitive;

const _: () = assert!(size_of::<Packed>() == size_of::<[u32; 24]>());

#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    offset: f32,
}

struct Ramp {
    start: [f32; 2],
    vector: [f32; 2],
    stops: Vec<(f32, [f32; 4])>,
}

impl Ramp {
    fn new(packed: Packed) -> Self {
        // Iced's public Pod layout is eight RGBA16 colours, eight f16 offsets, then endpoints.
        let words: [u32; 24] = cast(packed);
        let start = [f32::from_bits(words[20]), f32::from_bits(words[21])];
        let end = [f32::from_bits(words[22]), f32::from_bits(words[23])];
        let stops = (0..8)
            .map(|i| {
                let offsets = halves(words[16 + i / 2]);
                let [r, g] = halves(words[i * 2]);
                let [b, a] = halves(words[i * 2 + 1]);
                (offsets[i % 2], [r * a, g * a, b * a, a])
            })
            .take_while(|(offset, _)| *offset <= 1.0)
            .collect();
        Self {
            start,
            vector: [end[0] - start[0], end[1] - start[1]],
            stops,
        }
    }

    fn vertex(&self, position: [f32; 2]) -> Vertex {
        let [dx, dy] = self.vector;
        let length = dx.mul_add(dx, dy * dy);
        let offset = if length > 0.0 {
            dx.mul_add(
                position[0] - self.start[0],
                dy * (position[1] - self.start[1]),
            ) / length
        } else {
            1.0
        };
        Vertex { position, offset }
    }

    fn triangle(&self, points: [Vertex; 3], into: &mut Indexed<SolidVertex2D>) {
        let Some(&first) = self.stops.first() else {
            return;
        };
        let mut previous = first;
        let mut remaining = Vec::from(points);
        for &stop in &self.stops {
            let (before, after) = split(&remaining, stop.0);
            paint(&before, previous, stop, into);
            remaining = after;
            previous = stop;
        }
        paint(&remaining, previous, previous, into);
    }
}

pub(super) fn linearize(mesh: &mut Mesh) {
    let Mesh::Gradient {
        buffers,
        transformation,
        clip_bounds,
    } = mesh
    else {
        return;
    };
    let mut solid = Indexed {
        vertices: Vec::new(),
        indices: Vec::new(),
    };
    for indices in buffers.indices.as_chunks::<3>().0 {
        let vertices = indices.map(|index| buffers.vertices[AsPrimitive::<usize>::as_(index)]);
        let ramp = Ramp::new(vertices[0].gradient);
        ramp.triangle(
            vertices.map(|vertex| ramp.vertex(vertex.position)),
            &mut solid,
        );
    }
    *mesh = Mesh::Solid {
        buffers: solid,
        transformation: *transformation,
        clip_bounds: *clip_bounds,
    };
}

fn halves(word: u32) -> [f32; 2] {
    [word >> 16, word & u32::from(u16::MAX)].map(|bits| f16::from_bits(bits.as_()).to_f32())
}

fn split(points: &[Vertex], offset: f32) -> (Vec<Vertex>, Vec<Vertex>) {
    let mut before: Vec<Vertex> = Vec::with_capacity(points.len() + 1);
    let mut after: Vec<Vertex> = Vec::with_capacity(points.len() + 1);
    let Some(&last) = points.last() else {
        return (before, after);
    };
    let mut previous = last;
    for &point in points {
        if (previous.offset <= offset) != (point.offset <= offset) {
            let fraction = (offset - previous.offset) / (point.offset - previous.offset);
            let position = [0, 1].map(|axis| {
                fraction.mul_add(
                    point.position[axis] - previous.position[axis],
                    previous.position[axis],
                )
            });
            let edge = Vertex { position, offset };
            before.push(edge);
            after.push(edge);
        }
        if point.offset <= offset {
            before.push(point);
        } else {
            after.push(point);
        }
        previous = point;
    }
    (before, after)
}

fn paint(
    points: &[Vertex],
    from: (f32, [f32; 4]),
    to: (f32, [f32; 4]),
    into: &mut Indexed<SolidVertex2D>,
) {
    if points.len() < 3 {
        return;
    }
    let start: u32 = into.vertices.len().as_();
    into.vertices.extend(points.iter().map(|point| {
        let t = if to.0 > from.0 {
            ((point.offset - from.0) / (to.0 - from.0)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let [r, g, b, a] = [0, 1, 2, 3].map(|i| t.mul_add(to.1[i] - from.1[i], from.1[i]));
        let color = if a > 0.0 {
            [r / a, g / a, b / a, a]
        } else {
            [0.0; 4]
        };
        SolidVertex2D {
            position: point.position,
            color: cast(color),
        }
    }));
    for i in 1..points.len() - 1 {
        let i: u32 = i.as_();
        into.indices.extend([start, start + i, start + i + 1]);
    }
}
