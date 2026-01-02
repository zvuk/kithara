use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("not implemented")]
    Unimplemented,
    #[error("communication channel closed")]
    ChannelClosed,
    #[error("buffer is full")]
    BufferFull,
}

pub type IoResult<T> = Result<T, IoError>;
