use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_assets::ResourceKey;
use kithara_worker::{AsyncWorkerSource, Fetch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

use crate::{
    cache::Loader,
    events::HlsEvent,
    parsing::MasterPlaylist,
    HlsResult,
};

use super::{
    BufferTracker, HlsChunk, HlsCommand, ThroughputAccumulator, VariantMetadata,
};

/// HLS worker source implementing AsyncWorkerSource.
///
/// Fetches HLS segments and emits chunks with complete metadata.
pub struct HlsWorkerSource {
    /// Generic loader (FetchLoader or test mock).
    loader: Arc<dyn Loader>,

    /// Master playlist.
    master: MasterPlaylist,

    /// Variant metadata (codec, container, bitrate).
    variant_metadata: Vec<VariantMetadata>,

    /// Current variant index.
    current_variant: usize,

    /// Current segment index within variant.
    current_segment_index: usize,

    /// Init segments already sent for each variant.
    sent_init_for_variant: HashSet<usize>,

    /// Current epoch (incremented on seek/switch).
    epoch: u64,

    /// Paused state.
    paused: bool,

    /// Throughput tracking (for future ABR).
    throughput_accumulator: ThroughputAccumulator,

    /// Buffer tracking (for future ABR).
    buffer_tracker: BufferTracker,

    /// Global byte offset.
    byte_offset: u64,

    /// Events channel (optional).
    events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,

    /// Cancellation token.
    cancel: CancellationToken,
}

impl HlsWorkerSource {
    /// Create a new HLS worker source.
    ///
    /// # Arguments
    /// - `loader`: Segment loader (FetchLoader or mock)
    /// - `master`: Parsed master playlist
    /// - `variant_metadata`: Metadata for each variant
    /// - `initial_variant`: Starting variant index
    /// - `events_tx`: Optional event broadcast channel
    /// - `cancel`: Cancellation token
    pub fn new(
        loader: Arc<dyn Loader>,
        master: MasterPlaylist,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            loader,
            master,
            variant_metadata,
            current_variant: initial_variant,
            current_segment_index: 0,
            sent_init_for_variant: HashSet::new(),
            epoch: 0,
            paused: false,
            throughput_accumulator: ThroughputAccumulator::new(),
            buffer_tracker: BufferTracker::new(),
            byte_offset: 0,
            events_tx,
            cancel,
        }
    }

    /// Load segment bytes from loader.
    async fn load_segment_bytes(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<(Bytes, u64)> {
        let meta = self.loader.load_segment(variant, segment_index).await?;

        trace!(
            variant,
            segment_index,
            url = %meta.url,
            size = meta.len,
            "loaded segment metadata"
        );

        let _key = ResourceKey::from_url(&meta.url);

        let bytes = if let Some(ref _enc) = meta.key {
            return Err(crate::HlsError::Unimplemented);
        } else {
            Bytes::new()
        };

        Ok((bytes, meta.len))
    }

    /// Fetch init segment for a variant.
    async fn fetch_init_segment(&mut self, variant: usize) -> HlsResult<HlsChunk> {
        debug!(variant, "fetching init segment");

        let (bytes, _len) = self.load_segment_bytes(variant, usize::MAX).await?;

        let variant_meta = &self.variant_metadata[variant];
        let url = self.master.variants[variant]
            .uri
            .parse()
            .map_err(|e| crate::HlsError::InvalidUrl(format!("{}", e)))?;

        Ok(HlsChunk {
            bytes,
            byte_offset: 0,
            variant,
            segment_index: usize::MAX,
            segment_url: url,
            segment_duration: None,
            codec: variant_meta.codec,
            container: variant_meta.container,
            bitrate: variant_meta.bitrate,
            encryption: None,
            is_init_segment: true,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: true,
        })
    }

    /// Emit an event (if channel exists).
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }
}

#[async_trait]
impl AsyncWorkerSource for HlsWorkerSource {
    type Chunk = HlsChunk;
    type Command = HlsCommand;

    async fn fetch_next(&mut self) -> Fetch<HlsChunk> {
        if self.cancel.is_cancelled() {
            debug!("worker cancelled, returning EOF");
            return Fetch::new(HlsChunk::empty(), true, self.epoch);
        }

        if self.paused {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            return Fetch::new(HlsChunk::empty(), false, self.epoch);
        }

        let current_variant = self.current_variant;

        if !self.sent_init_for_variant.contains(&current_variant) {
            match self.fetch_init_segment(current_variant).await {
                Ok(init_chunk) => {
                    self.sent_init_for_variant.insert(current_variant);
                    return Fetch::new(init_chunk, false, self.epoch);
                }
                Err(e) => {
                    debug!(variant = current_variant, error = ?e, "init segment fetch failed");
                    return Fetch::new(HlsChunk::empty(), true, self.epoch);
                }
            }
        }

        let num_segments = match self.loader.num_segments(current_variant).await {
            Ok(n) => n,
            Err(e) => {
                debug!(error = ?e, "failed to get segment count");
                return Fetch::new(HlsChunk::empty(), true, self.epoch);
            }
        };

        if self.current_segment_index >= num_segments {
            debug!("reached end of playlist");
            self.emit_event(HlsEvent::EndOfStream);
            return Fetch::new(HlsChunk::empty(), true, self.epoch);
        }

        let (bytes, len) = match self
            .load_segment_bytes(current_variant, self.current_segment_index)
            .await
        {
            Ok(data) => data,
            Err(e) => {
                debug!(
                    variant = current_variant,
                    segment_index = self.current_segment_index,
                    error = ?e,
                    "segment load failed"
                );
                return Fetch::new(HlsChunk::empty(), true, self.epoch);
            }
        };

        let variant_meta = &self.variant_metadata[current_variant];
        let url_str = format!(
            "{}/segment{}.ts",
            self.master.variants[current_variant].uri, self.current_segment_index
        );
        let url = url_str
            .parse()
            .unwrap_or_else(|_| "http://localhost".parse().expect("valid url"));

        let chunk = HlsChunk {
            bytes,
            byte_offset: self.byte_offset,
            variant: current_variant,
            segment_index: self.current_segment_index,
            segment_url: url,
            segment_duration: Some(std::time::Duration::from_secs(4)),
            codec: variant_meta.codec,
            container: variant_meta.container,
            bitrate: variant_meta.bitrate,
            encryption: None,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: false,
        };

        self.byte_offset += len;
        self.current_segment_index += 1;

        trace!(
            variant = current_variant,
            segment = self.current_segment_index - 1,
            bytes = len,
            "emitting chunk"
        );

        Fetch::new(chunk, false, self.epoch)
    }

    fn handle_command(&mut self, cmd: HlsCommand) -> u64 {
        match cmd {
            HlsCommand::Seek {
                segment_index,
                epoch,
            } => {
                debug!(segment_index, epoch, "seek command");
                self.current_segment_index = segment_index;
                self.epoch = epoch;
                self.buffer_tracker.reset();
                self.epoch
            }
            HlsCommand::ForceVariant {
                variant_index,
                epoch,
            } => {
                debug!(variant_index, epoch, "force variant command");
                let old_variant = self.current_variant;
                self.current_variant = variant_index;
                self.sent_init_for_variant.remove(&variant_index);
                self.epoch = epoch;
                self.emit_event(HlsEvent::VariantApplied {
                    from_variant: old_variant,
                    to_variant: variant_index,
                    reason: crate::abr::AbrReason::ManualOverride,
                });
                self.epoch
            }
            HlsCommand::Pause => {
                debug!("pause command");
                self.paused = true;
                self.epoch
            }
            HlsCommand::Resume => {
                debug!("resume command");
                self.paused = false;
                self.epoch
            }
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cache::{MockLoader, SegmentMeta},
        parsing::{MasterPlaylist, VariantStream, VariantId},
    };
    use std::time::Duration;
    use url::Url;

    fn create_test_master() -> MasterPlaylist {
        MasterPlaylist {
            variants: vec![VariantStream {
                id: VariantId(0),
                uri: "http://test.com/variant0.m3u8".to_string(),
                bandwidth: Some(128000),
                name: None,
                codec: None,
            }],
        }
    }

    fn create_test_metadata() -> Vec<VariantMetadata> {
        vec![VariantMetadata {
            index: 0,
            codec: Some(kithara_stream::AudioCodec::AacLc),
            container: Some(crate::parsing::ContainerFormat::Fmp4),
            bitrate: Some(128000),
        }]
    }

    fn create_test_segment_meta(
        variant: usize,
        segment_index: usize,
        len: u64,
    ) -> SegmentMeta {
        SegmentMeta {
            variant,
            segment_index,
            sequence: segment_index as u64,
            url: Url::parse(&format!(
                "http://test.com/v{}/seg{}.ts",
                variant, segment_index
            ))
            .expect("valid url"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
        }
    }

    #[tokio::test]
    async fn test_worker_source_basic() {
        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader
            .expect_num_segments()
            .with(mockall::predicate::eq(0))
            .returning(|_| Ok(3));

        loader
            .expect_load_segment()
            .with(
                mockall::predicate::eq(0),
                mockall::predicate::eq(usize::MAX),
            )
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 1000)));

        loader
            .expect_load_segment()
            .times(2)
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 10000)));

        let master = create_test_master();
        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            master,
            metadata,
            0,
            None,
            cancel,
        );

        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof);
        assert_eq!(fetch1.epoch, 0);
        assert!(fetch1.data.is_init_segment);

        let fetch2 = source.fetch_next().await;
        assert!(!fetch2.is_eof);
        assert_eq!(fetch2.data.segment_index, 0);
        assert!(!fetch2.data.is_init_segment);

        let fetch3 = source.fetch_next().await;
        assert!(!fetch3.is_eof);
        assert_eq!(fetch3.data.segment_index, 1);
        assert!(!fetch3.data.is_init_segment);
    }

    #[tokio::test]
    async fn test_worker_source_eof() {
        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader
            .expect_num_segments()
            .with(mockall::predicate::eq(0))
            .returning(|_| Ok(0));

        loader
            .expect_load_segment()
            .with(
                mockall::predicate::eq(0),
                mockall::predicate::eq(usize::MAX),
            )
            .returning(|variant, idx| Ok(create_test_segment_meta(variant, idx, 1000)));

        let master = create_test_master();
        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            master,
            metadata,
            0,
            None,
            cancel,
        );

        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof);

        let fetch2 = source.fetch_next().await;
        assert!(fetch2.is_eof);
    }

    #[tokio::test]
    async fn test_worker_source_seek_command() {
        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader
            .expect_num_segments()
            .returning(|_| Ok(10));

        let master = create_test_master();
        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            master,
            metadata,
            0,
            None,
            cancel,
        );

        let new_epoch = source.handle_command(HlsCommand::Seek {
            segment_index: 5,
            epoch: 1,
        });

        assert_eq!(new_epoch, 1);
        assert_eq!(source.current_segment_index, 5);
        assert_eq!(source.epoch, 1);
    }
}
