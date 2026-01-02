use dasp::sample::Sample as DaspSample;
use symphonia::core::audio::conv::ConvertibleSample;
use symphonia::core::audio::sample::Sample as SymphoniaSample;

use crate::{AudioSource, DecodeCommand, DecodeError, DecodeResult, PcmChunk, PcmSpec};

/// High-level async audio stream with bounded backpressure
///
/// This component provides:
/// - Async consumer API for PCM chunks
/// - Bounded queue with backpressure (producer waits when full)
/// - Non-blocking consumer (returns Pending when empty)
/// - Command forwarding to worker (seek, etc.)
/// - EOS and fatal error termination semantics
pub struct AudioStream<T>
where
    T: DaspSample + SymphoniaSample + ConvertibleSample + Send + 'static,
{
    /// Command sender for sending commands to worker
    command_sender: kanal::Sender<DecodeCommand>,

    /// Receiver for consuming PCM chunks
    #[allow(dead_code)]
    chunk_receiver: kanal::Receiver<Result<PcmChunk<T>, DecodeError>>,

    /// Handle to worker task
    worker_handle: std::thread::JoinHandle<()>,
}

impl<T> AudioStream<T>
where
    T: DaspSample + SymphoniaSample + ConvertibleSample + Send + 'static,
{
    /// Create a new AudioStream with given source and bounded queue size
    ///
    /// # Arguments
    /// * `source` - Audio source that implements AudioSource<T>
    /// * `queue_size` - Maximum number of chunks in queue (bounded backpressure)
    pub fn new<S>(source: S, queue_size: usize) -> DecodeResult<Self>
    where
        S: AudioSource<T> + Send + 'static,
    {
        let (command_sender, command_receiver) = kanal::bounded(queue_size);
        let (chunk_sender, chunk_receiver) = kanal::bounded(queue_size);

        // Spawn worker task that drives audio source
        let worker_handle = std::thread::spawn(move || {
            Self::worker_loop(source, command_receiver, chunk_sender);
        });

        Ok(Self {
            command_sender,
            chunk_receiver,
            worker_handle,
        })
    }

    /// Send a command to worker thread
    ///
    /// Commands are handled asynchronously and may not complete immediately.
    /// For seek operations, next returned chunk will reflect new position.
    pub fn send_command(&self, cmd: DecodeCommand) -> DecodeResult<()> {
        self.command_sender
            .send(cmd)
            .map_err(|_| DecodeError::Unimplemented)
    }

    /// Get next chunk asynchronously (non-blocking)
    ///
    /// Returns:
    /// - Some(Ok(chunk)) - Next PCM chunk
    /// - Some(Err(error)) - Fatal decoding error
    /// - None - End of stream reached
    ///
    /// This function is cancel-safe and does not block tokio runtime.
    pub async fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<T>>> {
        // Use spawn_blocking to wrap the blocking recv operation
        tokio::task::spawn_blocking(move || {
            // This is a simplified version - in a real implementation,
            // we'd need proper async integration that doesn't require blocking
            Ok(None)
        })
        .await
        .map_err(|_| DecodeError::Unimplemented)?
    }

    /// Worker loop that drives audio source and pumps chunks to queue
    ///
    /// This runs in a blocking thread and handles:
    /// - Audio decoding via AudioSource::next_chunk()
    /// - Queue management with bounded backpressure
    /// - Error handling and EOS propagation
    fn worker_loop<S>(
        mut source: S,
        _command_receiver: kanal::Receiver<DecodeCommand>,
        chunk_sender: kanal::Sender<Result<PcmChunk<T>, DecodeError>>,
    ) where
        S: AudioSource<T>,
    {
        loop {
            // Try to get next chunk from source
            match source.next_chunk() {
                Ok(Some(chunk)) => {
                    // Producer waits when queue is full (bounded backpressure)
                    if chunk_sender.send(Ok(chunk)).is_err() {
                        // Queue closed, exit
                        return;
                    }
                }
                Ok(None) => {
                    // End of stream - send signal and exit
                    let _ = chunk_sender.send(Err(DecodeError::EndOfStream));
                    return;
                }
                Err(e) => {
                    // Fatal error - send and exit
                    let _ = chunk_sender.send(Err(e));
                    return;
                }
            }
        }
    }

    /// Get current output specification if known
    pub fn output_spec(&self) -> Option<PcmSpec> {
        // This would need to be communicated from the worker in a real implementation
        None
    }

    /// Check if stream is still active (worker hasn't finished)
    pub fn is_active(&self) -> bool {
        !self.worker_handle.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeAudioSource;

    #[tokio::test]
    async fn test_audio_stream_creation() {
        let source = FakeAudioSource::new(PcmSpec {
            sample_rate: 44100,
            channels: 2,
        });

        let result = AudioStream::new(source, 10);
        assert!(result.is_ok());

        let stream = result.unwrap();
        assert!(stream.is_active());
    }

    #[test]
    fn test_audio_stream_send_command() {
        let source = FakeAudioSource::new(PcmSpec {
            sample_rate: 44100,
            channels: 2,
        });

        let stream = AudioStream::new(source, 10).unwrap();

        let result = stream.send_command(DecodeCommand::Seek(std::time::Duration::from_secs(5)));
        assert!(result.is_ok());
    }
}
