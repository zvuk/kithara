use super::*;
pub(in crate::pipeline::source) fn produced_data(fetch: Fetch<AudioChunk>) -> AudioChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

pub(in crate::pipeline::source) fn spec(sample_rate: u32) -> AudioSpec {
    AudioSpec::new(
        consts::REBUILD_CHANNELS,
        NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
    )
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(in crate::pipeline::source) struct TestDecoder {
    pub(in crate::pipeline::source) drops: Arc<Mutex<Vec<u64>>>,
    pub(in crate::pipeline::source) preparations: Arc<AtomicU64>,
    pub(in crate::pipeline::source) id: u64,
    #[field(with, option_set_some, vis = "pub(in crate::pipeline::source)")]
    pub(in crate::pipeline::source) seek_error: Option<DecodeError>,
}

impl TestDecoder {
    pub(in crate::pipeline::source) fn new(id: u64, drops: Arc<Mutex<Vec<u64>>>) -> Self {
        Self {
            drops,
            id,
            preparations: Arc::new(AtomicU64::new(0)),
            seek_error: None,
        }
    }
}

impl Drop for TestDecoder {
    fn drop(&mut self) {
        self.drops.lock().push(self.id);
    }
}

impl Decoder for TestDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Eof)
    }

    fn prepare_next_chunk(&mut self) {
        self.preparations.fetch_add(1, Ordering::Relaxed);
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        if let Some(error) = self.seek_error.take() {
            return Err(error);
        }
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> AudioSpec {
        AudioSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

pub(in crate::pipeline::source) struct FailingDecoder;

impl Decoder for FailingDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Err(DecodeError::InvalidData {
            detail: "fixture decode failure",
        })
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: position,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> AudioSpec {
        AudioSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

pub(in crate::pipeline::source) struct ProfileCountingDecoder {
    pub(in crate::pipeline::source) gapless_profile_reads: Arc<AtomicU64>,
}

impl Decoder for ProfileCountingDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn gapless_profile(&self, _codec: Option<AudioCodec>) -> GaplessProfile {
        self.gapless_profile_reads.fetch_add(1, Ordering::AcqRel);
        GaplessProfile::new(self.spec(), None, None, 0)
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Eof)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> AudioSpec {
        AudioSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(in crate::pipeline::source) struct RouteSignalDecoder {
    pub(in crate::pipeline::source) drops: Arc<Mutex<Vec<u64>>>,
    pub(in crate::pipeline::source) pcm: Arc<[f32]>,
    pub(in crate::pipeline::source) gapless: Option<GaplessInfo>,
    pub(in crate::pipeline::source) remaining_chunks: Option<usize>,
    pub(in crate::pipeline::source) pools: Pools,
    pub(in crate::pipeline::source) sample_rate: u32,
    pub(in crate::pipeline::source) id: u64,
    pub(in crate::pipeline::source) next_frame: u64,
    #[field(with, vis = "pub(in crate::pipeline::source)")]
    pub(in crate::pipeline::source) timeline_gap: u64,
    pub(in crate::pipeline::source) phase: Option<Arc<Mutex<SourcePhase>>>,
}

impl RouteSignalDecoder {
    pub(in crate::pipeline::source) fn new(
        route_pcm: &RoutePcm,
        id: u64,
        sample_rate: u32,
        gapless: Option<GaplessInfo>,
        remaining_chunks: Option<usize>,
        drops: Arc<Mutex<Vec<u64>>>,
        pools: Pools,
    ) -> Self {
        let index = match sample_rate {
            44_100 => 0,
            48_000 => 1,
            _ => panic!("unprepared route sample rate: {sample_rate}"),
        };
        Self {
            drops,
            gapless,
            id,
            pools,
            remaining_chunks,
            sample_rate,
            pcm: route_pcm[index].clone(),
            next_frame: 0,
            timeline_gap: 0,
            phase: None,
        }
    }

    pub(in crate::pipeline::source) fn audio_spec(&self) -> AudioSpec {
        spec(self.sample_rate)
    }
}

impl Drop for RouteSignalDecoder {
    fn drop(&mut self) {
        self.drops.lock().push(self.id);
    }
}

impl Decoder for RouteSignalDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn gapless_profile(&self, _codec: Option<AudioCodec>) -> GaplessProfile {
        GaplessProfile::new(self.audio_spec(), self.gapless, None, 0)
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if self.phase.as_ref().is_some_and(|phase| {
            matches!(
                *phase.lock(),
                SourcePhase::Waiting | SourcePhase::WaitingDemand | SourcePhase::WaitingMetadata
            )
        }) {
            return Ok(DecoderChunkOutcome::Pending(
                kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::SourcePending,
                ),
            ));
        }
        if self.remaining_chunks == Some(0) {
            return Ok(DecoderChunkOutcome::Eof);
        }
        if let Some(remaining) = self.remaining_chunks.as_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        let spec = self.audio_spec();
        let channels = usize::from(consts::REBUILD_CHANNELS);
        let frames = consts::ROUTE_CHUNK_FRAMES;
        let start_sample =
            usize::try_from(self.next_frame).expect("fixture frame index") * channels;
        let samples = &self.pcm[start_sample..start_sample + frames * channels];
        let frame_count = u32::try_from(frames).unwrap_or(u32::MAX);
        let start = self.next_frame;
        let end = start.saturating_add(u64::from(frame_count));
        self.next_frame = end;
        Ok(DecoderChunkOutcome::Chunk(Box::new(AudioChunk::new(
            AudioChunkInfo {
                spec,
                timestamp: spec
                    .duration_for(start)
                    .expect("route signal timestamp fits Duration"),
                end_timestamp: spec
                    .duration_for(end)
                    .expect("route signal end timestamp fits Duration"),
                frame_offset: start,
                frames: frame_count,
                ..Default::default()
            },
            sample_buffer(&self.pools, samples),
        ))))
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let frame = u64::try_from(
            self.audio_spec()
                .frames_for(pos)
                .expect("route signal seek fits frame count")
                .get(),
        )
        .unwrap_or(u64::MAX);
        self.next_frame = frame;
        Ok(DecoderSeekOutcome::Landed {
            landed_at: self
                .audio_spec()
                .duration_for(frame)
                .expect("route signal landing fits Duration"),
            landed_frame: frame,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> AudioSpec {
        self.audio_spec()
    }

    fn timeline_gap_frames(&self) -> u64 {
        self.timeline_gap
    }

    fn update_byte_len(&self, _len: u64) {}
}

pub(in crate::pipeline::source) struct TestControl {
    pub(in crate::pipeline::source) byte_map_enabled: AtomicBool,
    pub(in crate::pipeline::source) demand_in_flight: AtomicBool,
    pub(in crate::pipeline::source) exact_reader_ready: AtomicBool,
    pub(in crate::pipeline::source) exact_reader_taken: AtomicBool,
    pub(in crate::pipeline::source) plan_calls: AtomicU64,
    pub(in crate::pipeline::source) prepare_calls: AtomicU64,
    pub(in crate::pipeline::source) promote_calls: AtomicU64,
    pub(in crate::pipeline::source) take_calls: AtomicU64,
    pub(in crate::pipeline::source) aborted_transition: Mutex<Option<VariantTransition>>,
    pub(in crate::pipeline::source) exact_plan: Mutex<Option<VariantReaderPlan>>,
    pub(in crate::pipeline::source) format_range: Mutex<Option<Range<u64>>>,
    pub(in crate::pipeline::source) landing: Mutex<Option<Duration>>,
    pub(in crate::pipeline::source) media_info: Mutex<Option<MediaInfo>>,
    pub(in crate::pipeline::source) prepared_profile: Mutex<Option<ReaderProfile>>,
    pub(in crate::pipeline::source) promotion: Mutex<VariantPromotion>,
}

impl TestControl {
    pub(in crate::pipeline::source) fn new(media_info: MediaInfo) -> Self {
        Self {
            aborted_transition: Mutex::new(None),
            byte_map_enabled: AtomicBool::new(false),
            demand_in_flight: AtomicBool::new(false),
            exact_plan: Mutex::new(None),
            exact_reader_ready: AtomicBool::new(false),
            exact_reader_taken: AtomicBool::new(false),
            landing: Mutex::new(None),
            media_info: Mutex::new(Some(media_info)),
            plan_calls: AtomicU64::new(0),
            prepare_calls: AtomicU64::new(0),
            prepared_profile: Mutex::new(None),
            promote_calls: AtomicU64::new(0),
            promotion: Mutex::new(VariantPromotion::Stale),
            take_calls: AtomicU64::new(0),
            format_range: Mutex::new(Some(0..32)),
        }
    }

    pub(in crate::pipeline::source) fn aborted_transition(&self) -> Option<VariantTransition> {
        *self.aborted_transition.lock()
    }

    pub(in crate::pipeline::source) fn enable_byte_map(&self) {
        self.byte_map_enabled.store(true, Ordering::Release);
    }

    pub(in crate::pipeline::source) fn landing(&self) -> Option<Duration> {
        *self.landing.lock()
    }

    pub(in crate::pipeline::source) fn plan_calls(&self) -> u64 {
        self.plan_calls.load(Ordering::Acquire)
    }

    pub(in crate::pipeline::source) fn prepare_calls(&self) -> u64 {
        self.prepare_calls.load(Ordering::Acquire)
    }

    pub(in crate::pipeline::source) fn prepared_profile(&self) -> Option<ReaderProfile> {
        *self.prepared_profile.lock()
    }

    pub(in crate::pipeline::source) fn promote_calls(&self) -> u64 {
        self.promote_calls.load(Ordering::Acquire)
    }

    pub(in crate::pipeline::source) fn set_demand_in_flight(&self, in_flight: bool) {
        self.demand_in_flight.store(in_flight, Ordering::Release);
    }

    pub(in crate::pipeline::source) fn set_exact_plan(&self, plan: VariantReaderPlan) {
        *self.exact_plan.lock() = Some(plan);
        *self.prepared_profile.lock() = None;
        self.exact_reader_ready.store(false, Ordering::Release);
        self.exact_reader_taken.store(false, Ordering::Release);
    }

    pub(in crate::pipeline::source) fn set_exact_reader_ready(&self) {
        self.exact_reader_ready.store(true, Ordering::Release);
    }

    /// Publish a new active variant on the source, the way a promoted ABR
    /// switch does. `rebuild::policy::superseded` reads exactly this.
    pub(in crate::pipeline::source) fn set_media_info(&self, media_info: MediaInfo) {
        *self.media_info.lock() = Some(media_info);
    }

    pub(in crate::pipeline::source) fn set_promotion(&self, promotion: VariantPromotion) {
        *self.promotion.lock() = promotion;
    }

    pub(in crate::pipeline::source) fn take_calls(&self) -> u64 {
        self.take_calls.load(Ordering::Acquire)
    }
}

impl VariantControl for TestControl {
    fn abort_variant(&self, transition: VariantTransition) -> bool {
        let mut exact_plan = self.exact_plan.lock();
        if exact_plan
            .as_ref()
            .is_none_or(|plan| plan.transition() != transition)
        {
            return false;
        }
        *exact_plan = None;
        drop(exact_plan);
        *self.aborted_transition.lock() = Some(transition);
        true
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.format_range
            .lock()
            .clone()
            .ok_or(StreamError::Source(SourceError::FormatChangeNotApplicable))
    }

    fn plan_variant_reader(
        &self,
        landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        self.plan_calls.fetch_add(1, Ordering::AcqRel);
        if let Some(landing) = landing {
            *self.landing.lock() = Some(landing);
        }
        Ok(self.exact_plan.lock().clone())
    }

    fn prepare_variant_reader(
        &self,
        plan: VariantReaderPlan,
        profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        self.prepare_calls.fetch_add(1, Ordering::AcqRel);
        *self.prepared_profile.lock() = Some(profile);
        Ok((self.exact_plan.lock().as_ref() == Some(&plan)).then(|| plan.transition()))
    }

    fn promote_variant(&self, transition: VariantTransition) -> VariantPromotion {
        self.promote_calls.fetch_add(1, Ordering::AcqRel);
        if !self
            .exact_plan
            .lock()
            .as_ref()
            .is_some_and(|plan| plan.transition() == transition)
        {
            return VariantPromotion::Stale;
        }
        let promotion = *self.promotion.lock();
        if promotion == VariantPromotion::Promoted {
            *self.exact_plan.lock() = None;
        }
        promotion
    }

    fn take_prepared_variant_reader(
        &self,
        transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        self.take_calls.fetch_add(1, Ordering::AcqRel);
        let Some(plan) = self
            .exact_plan
            .lock()
            .clone()
            .filter(|plan| plan.transition() == transition)
        else {
            return Ok(VariantReaderTake::Stale);
        };
        if !self.exact_reader_ready.load(Ordering::Acquire) {
            return Ok(VariantReaderTake::Preparing);
        }
        if self.exact_reader_taken.swap(true, Ordering::AcqRel) {
            return Ok(VariantReaderTake::Taken);
        }
        let reader = OpenedReader::new(Cursor::new(Vec::new()), Some(0), None, None, None);
        Ok(VariantReaderTake::Ready(OpenedVariantReader::new(
            plan, reader,
        )))
    }

    fn transition_demand_in_flight(&self, transition: VariantTransition) -> bool {
        self.demand_in_flight.load(Ordering::Acquire)
            && self
                .exact_plan
                .lock()
                .as_ref()
                .is_some_and(|plan| plan.transition() == transition)
    }
}

/// Optional park inside `wait_range`, letting a test hold the stream's
/// control mutex the way a real construction read does: `Stream::read`
/// enters `Source::wait_range` under the `SharedStream` mutex and stays
/// there until data lands. Disarmed by default — no other test changes.
#[derive(Default)]
pub(in crate::pipeline::source) struct WaitPark {
    pub(in crate::pipeline::source) armed: AtomicBool,
    pub(in crate::pipeline::source) condvar: Condvar,
    pub(in crate::pipeline::source) state: Mutex<WaitParkState>,
    pub(in crate::pipeline::source) entered: Notify,
}

#[derive(Default)]
pub(in crate::pipeline::source) struct WaitParkState {
    pub(in crate::pipeline::source) entered: bool,
    pub(in crate::pipeline::source) released: bool,
}

impl WaitPark {
    pub(in crate::pipeline::source) fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    pub(in crate::pipeline::source) fn enter_if_armed(&self) {
        if !self.armed.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock();
        state.entered = true;
        self.entered.notify_one();
        while !state.released {
            state = self.condvar.wait(state);
        }
        drop(state);
    }

    pub(in crate::pipeline::source) fn release(&self) {
        let mut state = self.state.lock();
        state.released = true;
        drop(state);
        self.condvar.notify_all();
    }

    /// Wait until the holder is inside `wait_range` — i.e. the control
    /// mutex is held by a parked blocking read.
    pub(in crate::pipeline::source) async fn wait_entered(&self) {
        while !self.state.lock().entered {
            self.entered.notified().await;
        }
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(in crate::pipeline::source) struct TestSource {
    pub(in crate::pipeline::source) byte_map: Arc<TestByteMap>,
    pub(in crate::pipeline::source) control: Arc<TestControl>,
    pub(in crate::pipeline::source) park: Arc<WaitPark>,
    pub(in crate::pipeline::source) phase: Arc<Mutex<SourcePhase>>,
    pub(in crate::pipeline::source) playhead: Arc<PlayheadState>,
    pub(in crate::pipeline::source) position: Arc<AtomicU64>,
    pub(in crate::pipeline::source) activity: Activity,
    pub(in crate::pipeline::source) writer: Option<ActivityWriter>,
    pub(in crate::pipeline::source) waits: Arc<Mutex<Vec<Range<u64>>>>,
    #[field(with = with_peer_wake, option_set_some, vis = "pub(in crate::pipeline::source)")]
    pub(in crate::pipeline::source) peer: Option<Arc<DeferredWake>>,
}

impl TestSource {
    pub(in crate::pipeline::source) fn new(control: Arc<TestControl>) -> Self {
        let writer = ActivityWriter::new();
        Self {
            activity: writer.reader(),
            writer: Some(writer),
            control,
            byte_map: Arc::new(TestByteMap),
            park: Arc::new(WaitPark::default()),
            peer: None,
            phase: Arc::new(Mutex::new(SourcePhase::Ready)),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            waits: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(in crate::pipeline::source) fn park_handle(&self) -> Arc<WaitPark> {
        Arc::clone(&self.park)
    }

    pub(in crate::pipeline::source) fn phase_handle(&self) -> Arc<Mutex<SourcePhase>> {
        Arc::clone(&self.phase)
    }

    pub(in crate::pipeline::source) fn segmented(control: Arc<TestControl>) -> Self {
        control.enable_byte_map();
        Self::new(control)
    }

    pub(in crate::pipeline::source) fn waits_handle(&self) -> Arc<Mutex<Vec<Range<u64>>>> {
        Arc::clone(&self.waits)
    }
}

/// Byte-space probe sharing the test source's scripted cells — the same
/// phase, cursor, and byte-map gating as the `Source` impl below.
pub(in crate::pipeline::source) struct SharedPhaseProbe {
    pub(in crate::pipeline::source) byte_map: Arc<TestByteMap>,
    pub(in crate::pipeline::source) control: Arc<TestControl>,
    pub(in crate::pipeline::source) phase: Arc<Mutex<SourcePhase>>,
    pub(in crate::pipeline::source) position: Arc<AtomicU64>,
}

impl SourceProbe for SharedPhaseProbe {
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        if self.control.byte_map_enabled.load(Ordering::Acquire) {
            Some(self.byte_map.clone() as Arc<dyn ByteMap>)
        } else {
            None
        }
    }

    fn len(&self) -> Option<u64> {
        Some(4096)
    }

    fn phase(&self) -> SourcePhase {
        *self.phase.lock()
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        *self.phase.lock()
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }
}

impl Source for TestSource {
    fn activity(&self) -> Activity {
        self.activity.clone()
    }

    fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.writer.take()
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        if self.control.byte_map_enabled.load(Ordering::Acquire) {
            Some(self.byte_map.clone() as Arc<dyn ByteMap>)
        } else {
            None
        }
    }

    fn len(&self) -> Option<u64> {
        Some(4096)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.control.media_info.lock().clone()
    }

    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        self.peer.clone()
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        *self.phase.lock()
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(SharedPhaseProbe {
            phase: Arc::clone(&self.phase),
            position: Arc::clone(&self.position),
            control: Arc::clone(&self.control),
            byte_map: Arc::clone(&self.byte_map),
        })
    }

    fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        Ok(ReadOutcome::Eof)
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        Some(Arc::clone(&self.control) as Arc<dyn VariantControl>)
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        self.park.enter_if_armed();
        self.waits.lock().push(range);
        match *self.phase.lock() {
            SourcePhase::Ready => Ok(WaitOutcome::Ready),
            SourcePhase::Eof => Ok(WaitOutcome::Eof),
            _ => Err(StreamError::Source(SourceError::WaitBudgetExceeded)),
        }
    }
}

pub(in crate::pipeline::source) struct TestByteMap;

impl TestByteMap {
    const CONTAINER_ORIGIN: u64 = 0;
    const INIT_BYTES: u64 = 627;
    const SEGMENT_BYTES: u64 = 4096;
    const SEGMENT_SECS: u64 = 4;

    pub(in crate::pipeline::source) fn descriptor(index: u64) -> SegmentDescriptor {
        let start = Self::INIT_BYTES.saturating_add(index.saturating_mul(Self::SEGMENT_BYTES));
        SegmentDescriptor::builder()
            .byte_range(start..start.saturating_add(Self::SEGMENT_BYTES))
            .decode_time(Duration::from_secs(
                index.saturating_mul(Self::SEGMENT_SECS),
            ))
            .duration(Duration::from_secs(Self::SEGMENT_SECS))
            .segment_index(u32::try_from(index).unwrap_or(u32::MAX))
            .variant_index(0)
            .build()
    }
}

impl ByteMap for TestByteMap {
    fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        let segment = Self::descriptor(position.as_secs() / Self::SEGMENT_SECS);
        Ok(Some(
            SourceSeekAnchor::builder()
                .segment_start(segment.decode_time)
                .segment_end(segment.decode_time.saturating_add(segment.duration))
                .segment_index(segment.segment_index)
                .variant_index(segment.variant_index)
                .byte_offset(segment.byte_range.start)
                .build(),
        ))
    }

    fn init_segment_range(&self) -> Range<u64> {
        Self::CONTAINER_ORIGIN..Self::INIT_BYTES
    }

    fn len(&self) -> Option<u64> {
        Some(Self::INIT_BYTES.saturating_add(Self::SEGMENT_BYTES))
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        (byte_offset < Self::INIT_BYTES).then(|| Self::descriptor(0))
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        Some(Self::descriptor(t.as_secs() / Self::SEGMENT_SECS))
    }

    fn segment_count(&self) -> Option<u32> {
        Some(1)
    }
}

pub(in crate::pipeline::source) struct TestConfig {
    pub(in crate::pipeline::source) source: TestSource,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            source: TestSource::new(Arc::new(TestControl::new(media_info(0)))),
        }
    }
}

pub(in crate::pipeline::source) struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Events = ();
    type Source = TestSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(config.source)
    }
}

pub(in crate::pipeline::source) fn media_info(variant: u32) -> MediaInfo {
    let mut info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::Fmp4))
        .build();
    info.variant_index = Some(variant);
    info
}

pub(in crate::pipeline::source) fn recreate_state(variant: u32) -> RecreateState {
    RecreateState {
        media_info: Some(media_info(variant)),
        cause: RecreateCause::FormatBoundary,
        offset: 0,
    }
}

pub(in crate::pipeline::source) struct RebuildFixture {
    pub(in crate::pipeline::source) control: Arc<TestControl>,
    pub(in crate::pipeline::source) drops: Arc<Mutex<Vec<u64>>>,
    pub(in crate::pipeline::source) pools: Pools,
    pub(in crate::pipeline::source) source: StreamAudioSource<TestStream>,
}

pub(in crate::pipeline::source) struct RouteFixture {
    pub(in crate::pipeline::source) control: Arc<TestControl>,
    pub(in crate::pipeline::source) drops: Arc<Mutex<Vec<u64>>>,
    pub(in crate::pipeline::source) phase: Arc<Mutex<SourcePhase>>,
    pub(in crate::pipeline::source) pools: Pools,
    pub(in crate::pipeline::source) source: StreamAudioSource<TestStream>,
}

pub(in crate::pipeline::source) async fn test_source(variant: u32) -> RebuildFixture {
    test_source_with_mode(variant, GaplessMode::Disabled).await
}

pub(in crate::pipeline::source) async fn test_source_with_mode(
    variant: u32,
    gapless_mode: GaplessMode,
) -> RebuildFixture {
    let pools = pools();
    let control = Arc::new(TestControl::new(media_info(variant)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let stream = Stream::<TestStream>::new(TestConfig {
        source: TestSource::new(control.clone()),
    })
    .await
    .expect("test stream");
    let shared_stream = SharedStream::new(stream);
    let factory_drops = drops.clone();
    let decoder_factory = DecoderFactory::new(
        move |_reader, _info, _rate| Ok(Box::new(TestDecoder::new(99, factory_drops.clone()))),
        None,
    );
    let decode = ActiveDecode::new(
        DecoderGeneration::new(
            Box::new(TestDecoder::new(1, drops.clone())),
            Some(media_info(0)),
            0,
            None,
            None,
            gapless_mode,
        ),
        gapless_mode,
        None,
        &pools,
    )
    .expect("decode scratch fits test pools");
    let source = StreamAudioSource::new(
        shared_stream,
        decode,
        SourceDecoderConfig {
            factory: decoder_factory,
            host_rate: NonZeroU32::new(consts::SAMPLE_RATE),
            backend: kithara_decode::DecoderBackend::default(),
            playback_resampler_backend: "none",
        },
        Arc::new(DeferredBus::new(EventBus::default(), 16)),
        Arc::new(NoopWorkerWake),
    );
    RebuildFixture {
        control,
        drops,
        pools,
        source,
    }
}

/// `segmented` vends the byte map HLS supplies plus an init-bearing
/// decoder factory: the rebuilt demuxer parses only when it is rooted at
/// the container origin, exactly like the Apple fMP4 segment path. A flat
/// source has neither, so no recreate origin other than `base_offset` is
/// even reachable on it.
pub(in crate::pipeline::source) struct RouteParams {
    pub(in crate::pipeline::source) chunks_before_eof: Option<usize>,
    pub(in crate::pipeline::source) gapless: Option<GaplessInfo>,
    pub(in crate::pipeline::source) incoming_chunks_before_eof: Option<usize>,
    pub(in crate::pipeline::source) segmented: bool,
    pub(in crate::pipeline::source) initial_host_rate: u32,
    pub(in crate::pipeline::source) active_timeline_gap: u64,
    pub(in crate::pipeline::source) incoming_timeline_gap: u64,
}
