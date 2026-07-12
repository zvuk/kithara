#[derive(Debug)]
pub(crate) enum HostCmd {
    Configure {
        codec_string: String,
        description: Option<Vec<u8>>,
        sample_rate: u32,
        channels: u16,
        generation: u64,
    },
    Decode {
        data: Vec<u8>,
        pts_us: u64,
        key: bool,
        generation: u64,
    },
    Reset {
        generation: u64,
    },
    Flush {
        generation: u64,
    },
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum HostOut {
    Pcm {
        interleaved: Vec<f32>,
        frames: u32,
        sample_rate: u32,
        channels: u16,
        pts_us: u64,
        generation: u64,
    },
    Configured {
        sample_rate: u32,
        channels: u16,
        generation: u64,
    },
    Error {
        detail: String,
        generation: u64,
    },
}
