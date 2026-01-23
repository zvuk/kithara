use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_assets::{Assets, ResourceKey};
use kithara_storage::Resource;
use kithara_worker::{AsyncWorkerSource, Fetch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

use super::{BufferTracker, HlsCommand, HlsMessage, ThroughputAccumulator, VariantMetadata};
use crate::{
    HlsResult,
    abr::{AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource, Variant},
    cache::Loader,
    events::HlsEvent,
    fetch::DefaultFetchManager,
    options::AbrOptions,
    parsing::MasterPlaylist,
};

/// HLS worker source implementing AsyncWorkerSource.
///
/// Fetches HLS segments and emits chunks with complete metadata.
pub struct HlsWorkerSource {
    /// Generic loader (FetchLoader or test mock).
    loader: Arc<dyn Loader>,

    /// Fetch manager for reading segment data.
    fetch_manager: Arc<DefaultFetchManager>,

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

    /// Cached init segments per variant (for combining with media).
    init_segments_cache: std::collections::HashMap<usize, Bytes>,

    /// Current epoch (incremented on seek/switch).
    epoch: u64,

    /// Paused state.
    paused: bool,

    /// Throughput tracking.
    throughput_accumulator: ThroughputAccumulator,

    /// Buffer tracking.
    buffer_tracker: BufferTracker,

    /// ABR controller.
    abr_controller: Option<AbrController<ThroughputEstimator>>,

    /// ABR variants list (for decision making).
    abr_variants: Vec<Variant>,

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
    /// - `fetch_manager`: Fetch manager for reading segment data
    /// - `master`: Parsed master playlist
    /// - `variant_metadata`: Metadata for each variant
    /// - `initial_variant`: Starting variant index
    /// - `abr_options`: ABR configuration (None for no ABR)
    /// - `events_tx`: Optional event broadcast channel
    /// - `cancel`: Cancellation token
    pub fn new(
        loader: Arc<dyn Loader>,
        fetch_manager: Arc<DefaultFetchManager>,
        master: MasterPlaylist,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_options: Option<AbrOptions>,
        events_tx: Option<tokio::sync::broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        // Build ABR variants list from variant_metadata
        let abr_variants: Vec<Variant> = variant_metadata
            .iter()
            .map(|v| Variant {
                variant_index: v.index,
                bandwidth_bps: v.bitrate.unwrap_or(0),
            })
            .collect();

        // Create ABR controller if enabled
        let abr_controller = abr_options.as_ref().and_then(|opts| {
            if matches!(opts.mode, crate::options::AbrMode::Auto(_)) {
                use std::time::Duration;
                let config = crate::abr::AbrConfig {
                    mode: opts.mode.clone(),
                    min_buffer_for_up_switch_secs: opts.min_buffer_for_up_switch as f64,
                    down_switch_buffer_secs: opts.down_switch_buffer as f64,
                    throughput_safety_factor: opts.throughput_safety_factor as f64,
                    up_hysteresis_ratio: opts.up_hysteresis_ratio as f64,
                    down_hysteresis_ratio: opts.down_hysteresis_ratio as f64,
                    min_switch_interval: opts.min_switch_interval,
                    sample_window: Duration::from_secs(30),
                };
                Some(AbrController::new(config, None))
            } else {
                None
            }
        });

        Self {
            loader,
            fetch_manager,
            master,
            variant_metadata,
            current_variant: initial_variant,
            current_segment_index: 0,
            sent_init_for_variant: HashSet::new(),
            init_segments_cache: std::collections::HashMap::new(),
            epoch: 0,
            paused: false,
            throughput_accumulator: ThroughputAccumulator::new(),
            buffer_tracker: BufferTracker::new(),
            abr_controller,
            abr_variants,
            byte_offset: 0,
            events_tx,
            cancel,
        }
    }

    /// Load segment bytes from SegmentMeta.
    async fn load_segment_bytes(&self, meta: &crate::cache::SegmentMeta) -> HlsResult<Bytes> {
        trace!(
            url = %meta.url,
            size = meta.len,
            "reading segment bytes from storage"
        );

        let key = ResourceKey::from_url(&meta.url);

        let bytes = if let Some(ref _enc) = meta.key {
            return Err(crate::HlsError::Unimplemented);
        } else {
            let resource = self
                .fetch_manager
                .assets()
                .open_streaming_resource(&key)
                .await?;
            resource.inner().read().await?
        };

        Ok(bytes)
    }

    /// Load init segment for a variant and cache it.
    ///
    /// Returns cached init segment if already loaded.
    async fn load_init_segment(&mut self, variant: usize) -> HlsResult<Bytes> {
        // Check cache first
        if let Some(cached) = self.init_segments_cache.get(&variant) {
            debug!(
                variant,
                cached_size = cached.len(),
                "using cached init segment"
            );
            return Ok(cached.clone());
        }

        debug!(variant, "loading init segment");
        let meta = self.loader.load_segment(variant, usize::MAX).await?;
        debug!(variant, url = %meta.url, size = meta.len, "init segment metadata loaded");

        let bytes = self.load_segment_bytes(&meta).await?;
        debug!(
            variant,
            bytes_len = bytes.len(),
            "init segment bytes loaded"
        );

        // Cache for future use
        self.init_segments_cache.insert(variant, bytes.clone());

        Ok(bytes)
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
    type Chunk = HlsMessage;
    type Command = HlsCommand;

    async fn fetch_next(&mut self) -> Fetch<HlsMessage> {
        if self.cancel.is_cancelled() {
            debug!("worker cancelled, returning EOF");
            return Fetch::new(HlsMessage::empty(), true, self.epoch);
        }

        if self.paused {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            return Fetch::new(HlsMessage::empty(), false, self.epoch);
        }

        let current_variant = self.current_variant;

        // Check if this is a variant switch (first segment after switch)
        let is_variant_switch = !self.sent_init_for_variant.contains(&current_variant);

        let num_segments = match self.loader.num_segments(current_variant).await {
            Ok(n) => n,
            Err(e) => {
                debug!(error = ?e, "failed to get segment count");
                return Fetch::new(HlsMessage::empty(), true, self.epoch);
            }
        };

        if self.current_segment_index >= num_segments {
            debug!("reached end of playlist");
            self.emit_event(HlsEvent::EndOfStream);
            return Fetch::new(HlsMessage::empty(), true, self.epoch);
        }

        let meta = match self
            .loader
            .load_segment(current_variant, self.current_segment_index)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                debug!(
                    variant = current_variant,
                    segment_index = self.current_segment_index,
                    error = ?e,
                    "segment metadata load failed"
                );
                return Fetch::new(HlsMessage::empty(), true, self.epoch);
            }
        };

        // Measure download time for ABR
        let download_start = std::time::Instant::now();
        let media_bytes = match self.load_segment_bytes(&meta).await {
            Ok(data) => data,
            Err(e) => {
                debug!(
                    variant = current_variant,
                    segment_index = self.current_segment_index,
                    error = ?e,
                    "segment bytes load failed"
                );
                return Fetch::new(HlsMessage::empty(), true, self.epoch);
            }
        };
        let download_duration = download_start.elapsed();

        // Combine init + media for EVERY segment (fMP4 requires init for each fragment)
        let combined_bytes = match self.load_init_segment(current_variant).await {
            Ok(init_bytes) => {
                if is_variant_switch {
                    debug!(
                        variant = current_variant,
                        init_size = init_bytes.len(),
                        media_size = media_bytes.len(),
                        "combining init + media for variant switch"
                    );
                    self.sent_init_for_variant.insert(current_variant);
                }
                let mut combined = Vec::with_capacity(init_bytes.len() + media_bytes.len());
                combined.extend_from_slice(&init_bytes);
                combined.extend_from_slice(&media_bytes);
                Bytes::from(combined)
            }
            Err(e) => {
                debug!(
                    variant = current_variant,
                    error = ?e,
                    "init segment load failed, using media only"
                );
                media_bytes
            }
        };

        let variant_meta = &self.variant_metadata[current_variant];

        let chunk = HlsMessage {
            bytes: combined_bytes.clone(),
            byte_offset: self.byte_offset,
            variant: current_variant,
            segment_index: self.current_segment_index,
            segment_url: meta.url.clone(),
            segment_duration: meta.duration,
            codec: variant_meta.codec,
            container: meta.container,
            bitrate: variant_meta.bitrate,
            encryption: None,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch,
        };

        self.byte_offset += combined_bytes.len() as u64;
        self.current_segment_index += 1;

        // ABR: Update buffer level
        if let Some(duration) = meta.duration {
            self.buffer_tracker.add_segment(duration);
        }

        // ABR: Calculate throughput
        let throughput_bps = if download_duration.as_secs_f64() > 0.0 {
            (meta.len as f64 * 8.0) / download_duration.as_secs_f64()
        } else {
            0.0
        };
        let buffer_level = self.buffer_tracker.buffer_level_secs();

        // ABR: Record throughput sample and make decision
        let abr_decision = if let Some(ref mut controller) = self.abr_controller {
            let sample = ThroughputSample {
                bytes: meta.len,
                duration: download_duration,
                at: download_start,
                source: ThroughputSampleSource::Network,
            };
            controller.push_throughput_sample(sample);
            Some(controller.decide(&self.abr_variants, buffer_level, std::time::Instant::now()))
        } else {
            None
        };

        // Emit events (after releasing mutable borrow)
        self.emit_event(HlsEvent::ThroughputSample {
            bytes_per_second: throughput_bps,
        });
        self.emit_event(HlsEvent::BufferLevel {
            level_seconds: buffer_level as f32,
        });

        // Apply ABR decision (only if variant actually changes)
        if let Some(decision) = abr_decision {
            if decision.changed {
                let new_variant = decision.target_variant_index;

                // Only apply switch if variant actually changed
                if new_variant != current_variant {
                    debug!(
                        from = current_variant,
                        to = new_variant,
                        reason = ?decision.reason,
                        "ABR switching variant"
                    );
                    self.current_variant = new_variant;
                    self.sent_init_for_variant.remove(&new_variant);
                    self.emit_event(HlsEvent::VariantApplied {
                        from_variant: current_variant,
                        to_variant: new_variant,
                        reason: decision.reason,
                    });
                } else {
                    debug!(
                        variant = current_variant,
                        reason = ?decision.reason,
                        "ABR decided to stay on current variant"
                    );
                }
            }
        }

        trace!(
            variant = current_variant,
            segment = self.current_segment_index - 1,
            bytes = meta.len,
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
    use std::time::Duration;

    use kithara_assets::AssetStore;
    use kithara_net::HttpClient;
    use url::Url;

    use super::*;
    use crate::{
        cache::{MockLoader, SegmentMeta},
        fetch::DefaultFetchManager,
        parsing::{MasterPlaylist, VariantId, VariantStream},
    };

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

    fn create_test_segment_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
        let url_str = if segment_index == usize::MAX {
            format!("http://test.com/v{}/init.mp4", variant)
        } else {
            format!("http://test.com/v{}/seg{}.ts", variant, segment_index)
        };

        SegmentMeta {
            variant,
            segment_index,
            sequence: if segment_index == usize::MAX {
                0
            } else {
                segment_index as u64
            },
            url: Url::parse(&url_str).expect("valid url"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(crate::parsing::ContainerFormat::Fmp4),
        }
    }

    struct TestContext {
        _temp_dir: tempfile::TempDir,
        fetch_manager: Arc<DefaultFetchManager>,
    }

    impl TestContext {
        fn new() -> Self {
            let temp_dir = tempfile::TempDir::new().expect("temp dir");
            let cache_dir = temp_dir.path().to_path_buf();

            let assets = kithara_assets::AssetStoreBuilder::new()
                .asset_root("test_root")
                .root_dir(&cache_dir)
                .build();

            let net = HttpClient::new(Default::default());
            let fetch_manager = Arc::new(DefaultFetchManager::new(assets, net));

            Self {
                _temp_dir: temp_dir,
                fetch_manager,
            }
        }

        async fn populate_data(&self, url: &Url, len: u64) {
            use kithara_assets::ResourceKey;
            use tokio::fs;

            let key = ResourceKey::from_url(url);
            let file_path = self
                ._temp_dir
                .path()
                .join("test_root")
                .join(&key.rel_path());

            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).await.expect("create dirs");
            }

            let fake_data = vec![0u8; len as usize];
            fs::write(&file_path, &fake_data).await.expect("write file");
        }
    }

    #[tokio::test]
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_basic() {
        let ctx = TestContext::new();

        // Populate test data
        ctx.populate_data(&Url::parse("http://test.com/v0/init.mp4").unwrap(), 1000)
            .await;
        ctx.populate_data(&Url::parse("http://test.com/v0/seg0.ts").unwrap(), 10000)
            .await;
        ctx.populate_data(&Url::parse("http://test.com/v0/seg1.ts").unwrap(), 10000)
            .await;

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
            ctx.fetch_manager.clone(),
            master,
            metadata,
            0,
            None, // no ABR
            None, // no events
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
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_eof() {
        let ctx = TestContext::new();

        // Populate test data
        ctx.populate_data(&Url::parse("http://test.com/v0/init.mp4").unwrap(), 1000)
            .await;

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
            ctx.fetch_manager.clone(),
            master,
            metadata,
            0,
            None, // no ABR
            None, // no events
            cancel,
        );

        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof);

        let fetch2 = source.fetch_next().await;
        assert!(fetch2.is_eof);
    }

    #[tokio::test]
    #[ignore] // TODO: fix asset store setup in tests
    async fn test_worker_source_seek_command() {
        let ctx = TestContext::new();

        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 1);

        loader.expect_num_segments().returning(|_| Ok(10));

        let master = create_test_master();
        let metadata = create_test_metadata();
        let cancel = CancellationToken::new();

        let mut source = HlsWorkerSource::new(
            Arc::new(loader),
            ctx.fetch_manager.clone(),
            master,
            metadata,
            0,
            None, // no ABR
            None, // no events
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
