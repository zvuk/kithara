#![forbid(unsafe_code)]

use std::io::{Read, Seek};

use kithara_core::CoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("not implemented")]
    Unimplemented,

    #[error("core error: {0}")]
    Core(#[from] CoreError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRate(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelCount(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    pub sample_rate: SampleRate,
    pub channels: ChannelCount,
}

#[derive(Clone, Debug)]
pub struct PcmChunk<T> {
    pub spec: AudioSpec,
    pub samples: Vec<T>,
}

pub trait ReadSeek: Read + Seek {}
impl<T> ReadSeek for T where T: Read + Seek {}

pub trait MediaSource: Send + 'static {
    fn reader(&self) -> Box<dyn ReadSeek + Send>;
    fn file_ext(&self) -> Option<&str> {
        None
    }
}

#[derive(Clone, Debug, Default)]
pub struct DecoderSettings;

pub struct Decoder<T> {
    _phantom: std::marker::PhantomData<T>,
}

impl<T> Decoder<T> {
    pub fn new(_source: Box<dyn MediaSource>, _settings: DecoderSettings) -> DecodeResult<Self> {
        unimplemented!("kithara-decode: Decoder::new is not implemented yet")
    }

    pub fn seek(&mut self, _pos: std::time::Duration) -> DecodeResult<()> {
        unimplemented!("kithara-decode: Decoder::seek is not implemented yet")
    }

    pub fn next(&mut self) -> DecodeResult<Option<PcmChunk<T>>> {
        unimplemented!("kithara-decode: Decoder::next is not implemented yet")
    }
}
