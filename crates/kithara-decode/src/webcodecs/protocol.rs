use kithara_bufpool::{ByteBuffer, SampleBuffer};
use kithara_platform::{
    sync::{Arc, mpsc},
    time::Duration,
};

pub(crate) enum HostCmd {
    Open {
        id: u64,
        reply_tx: mpsc::Sender<HostOut>,
    },
    Configure {
        decoder_id: u64,
        codec_string: String,
        description: Option<Arc<[u8]>>,
        sample_rate: u32,
        channels: u16,
        generation: u64,
    },
    Decode {
        decoder_id: u64,
        data: ByteBuffer,
        pts: Duration,
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
        interleaved: SampleBuffer,
        frames: u32,
        sample_rate: u32,
        channels: u16,
        pts: Duration,
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
