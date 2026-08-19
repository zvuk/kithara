use super::VisFrame;

pub(crate) const SHADER: &str = include_str!("../../../assets/shaders/vis.wgsl");

#[derive(Clone, Copy)]
pub(crate) struct Uniforms {
    origin: [f32; 2],
    resolution: [f32; 2],
    frame: VisFrame,
}

impl Uniforms {
    pub(crate) const BYTE_COUNT: usize = 32;
    pub(crate) const BUFFER_SIZE: u64 = 32;

    pub(crate) const fn new(frame: VisFrame, origin: [f32; 2], resolution: [f32; 2]) -> Self {
        Self {
            origin,
            resolution,
            frame,
        }
    }

    pub(crate) fn bytes(self) -> [u8; Self::BYTE_COUNT] {
        let mut bytes = [0; Self::BYTE_COUNT];
        for (index, value) in [
            self.resolution[0],
            self.resolution[1],
            self.origin[0],
            self.origin[1],
            self.frame.time(),
            self.frame.level(),
        ]
        .into_iter()
        .enumerate()
        {
            let offset = index * size_of::<f32>();
            bytes[offset..offset + size_of::<f32>()].copy_from_slice(&value.to_ne_bytes());
        }
        bytes[24..28].copy_from_slice(&self.frame.preset().to_ne_bytes());
        bytes
    }
}
