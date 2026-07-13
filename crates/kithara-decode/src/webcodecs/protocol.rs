pub(crate) enum HostCmd {
    Open {
        id: u64,
        reply_tx: kithara_platform::sync::mpsc::Sender<HostOut>,
    },
    Configure {
        decoder_id: u64,
        codec_string: String,
        description: Option<Vec<u8>>,
        sample_rate: u32,
        channels: u16,
        generation: u64,
    },
    Decode {
        decoder_id: u64,
        data: Vec<u8>,
        pts_us: u64,
        key: bool,
        generation: u64,
    },
    Reset {
        decoder_id: u64,
        generation: u64,
    },
    Flush {
        decoder_id: u64,
        generation: u64,
    },
    Close {
        id: u64,
    },
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
    Flushed {
        generation: u64,
    },
    Error {
        detail: String,
        generation: u64,
    },
}
