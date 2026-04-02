pub(crate) struct Mp4Bytes {
    buf: Vec<u8>,
}

impl Mp4Bytes {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    pub(crate) fn push_u8(&mut self, value: u8) {
        self.buf.push(value);
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

    pub(crate) fn push_i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn push_bytes(&mut self, value: &[u8]) {
        self.buf.extend_from_slice(value);
    }

    pub(crate) fn push_fourcc(&mut self, value: [u8; 4]) {
        self.push_bytes(&value);
    }

    pub(crate) fn push_zeroes(&mut self, count: usize) {
        self.buf.resize(self.buf.len() + count, 0);
    }
}

pub(crate) fn mp4_box(name: [u8; 4], contents: impl FnOnce(&mut Mp4Bytes)) -> Vec<u8> {
    let mut payload = Mp4Bytes::new();
    contents(&mut payload);

    let mut buf = Mp4Bytes::new();
    buf.push_u32((payload.len() + 8) as u32);
    buf.push_fourcc(name);
    buf.push_bytes(&payload.into_vec());
    buf.into_vec()
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
    let mut buf = Vec::with_capacity(payload.len() + 6);
    buf.push(tag);

    let mut value = payload.len();
    let mut stack = [0u8; 4];
    let mut len = 0;
    stack[len] = (value & 0x7F) as u8;
    len += 1;
    value >>= 7;
    while value > 0 {
        stack[len] = ((value & 0x7F) as u8) | 0x80;
        len += 1;
        value >>= 7;
    }
    for byte in stack[..len].iter().rev() {
        buf.push(*byte);
    }
    buf.extend_from_slice(payload);
    buf
}
