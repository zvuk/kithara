pub(crate) struct Mp4Bytes {
    buf: Vec<u8>,
}

impl Mp4Bytes {
    pub(crate) const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub(crate) fn push_fourcc(&mut self, value: [u8; 4]) {
        self.push_bytes(&value);
    }

    pub(crate) fn push_i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_u24(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes()[1..4]);
    }

    pub(crate) fn push_u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_zeroes(&mut self, count: usize) {
        self.buf.resize(self.buf.len() + count, 0);
    }

    delegate::delegate! {
        to self.buf {
            pub(crate) const fn len(&self) -> usize;
            #[call(extend_from_slice)]
            pub(crate) fn push_bytes(&mut self, value: &[u8]);
            #[call(push)]
            pub(crate) fn push_u8(&mut self, value: u8);
        }
    }
}

/// A byte count as the fixed-width integer an MP4 length field carries.
///
/// # Panics
///
/// Panics above `u32::MAX`. Every caller measures a buffer the muxer already
/// holds in memory, and a single box that large has no representation in the
/// 32-bit size field anyway.
pub(crate) fn mp4_len(bytes: usize) -> u32 {
    u32::try_from(bytes).expect("MP4 length fits a 32-bit field")
}

pub(crate) fn mp4_box(name: [u8; 4], contents: impl FnOnce(&mut Mp4Bytes)) -> Vec<u8> {
    let mut payload = Mp4Bytes::new();
    contents(&mut payload);

    let mut buf = Mp4Bytes::new();
    buf.push_u32(mp4_len(payload.len() + 8));
    buf.push_fourcc(name);
    buf.push_bytes(&payload.into_inner());
    buf.into_inner()
}

pub(crate) fn full_box(
    name: [u8; 4],
    version: u8,
    flags: u32,
    contents: impl FnOnce(&mut Mp4Bytes),
) -> Vec<u8> {
    mp4_box(name, |buf| {
        buf.push_u8(version);
        buf.push_u24(flags);
        contents(buf);
    })
}

pub(crate) fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Mp4Bytes::new();
    buf.push_u8(tag);

    let mut value = payload.len();
    let mut stack = [0u8; 4];
    let mut len = 0;
    stack[len] = value.to_le_bytes()[0] & 0x7f;
    len += 1;
    value >>= 7;
    while value > 0 {
        stack[len] = (value.to_le_bytes()[0] & 0x7f) | 0x80;
        len += 1;
        value >>= 7;
    }
    for byte in stack[..len].iter().rev() {
        buf.push_u8(*byte);
    }
    buf.push_bytes(payload);
    buf.into_inner()
}
