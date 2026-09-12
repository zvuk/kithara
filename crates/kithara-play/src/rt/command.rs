use std::sync::atomic::Ordering;

use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use ringbuf::traits::{Consumer, Producer};
use smallvec::SmallVec;

use super::{
    TrackSlot,
    processor::PlayerNodeProcessor,
    track::{PlayerResource, PlayerTrack},
};
use crate::bridge::{
    PlayerCmd, PlayerNotification, ScheduledSeekDisposition, TrackState, TrackTransition,
};

impl PlayerNodeProcessor {
    fn apply_fade_duration(&mut self, duration: f32) {
        self.crossfade.duration = duration;
        for (_, track) in self.tracks.iter_mut() {
            track.update_fade_duration(duration, self.sample_rate);
        }
    }

    fn apply_prefetch_duration(&mut self, duration: f32) {
        self.prefetch_duration = duration.max(0.0);
        for (_, track) in self.tracks.iter_mut() {
            track.set_prefetch_duration(self.prefetch_duration);
        }
    }

    fn apply_seek(&mut self, seconds: f64, seek_epoch: u64) {
        if seek_epoch != self.playback.seek_epoch.load(Ordering::SeqCst) {
            return;
        }

        let mut revived = false;
        for (_, track) in self.tracks.iter_mut() {
            // WHY: Slot-wide: the re-base releases the natural-end hold on every loaded track, including the ones this seek does not move.
            track.observe_seek_epoch(seek_epoch);
            match track.state() {
                TrackState::FadingIn | TrackState::Playing => {
                    track.seek(seconds);
                    track.play();
                }
                TrackState::FadingOut => {
                    track.stop();
                }
                TrackState::Finished if track.ended_at_eof() && seconds < track.duration() => {
                    track.seek(seconds);
                    track.play();
                    revived = true;
                }
                _ => {}
            }
        }
        if revived {
            self.playback.playing.store(true, Ordering::SeqCst);
        }
    }

    fn clear_all_tracks(&mut self) {
        let loaded: SmallVec<[TrackSlot; Self::MAX_TRACKS]> =
            self.tracks.iter().map(|(slot, _)| slot).collect();
        for slot in loaded {
            self.unload_slot(slot);
        }
        self.tracks_transitions.clear();
        self.playback.playing.store(false, Ordering::SeqCst);
        self.playback.position.store(0.0, Ordering::Relaxed);
        self.playback.frontier.store(0.0, Ordering::Relaxed);
        self.playback.cached.store(0.0, Ordering::Relaxed);
        self.playback.duration.store(0.0, Ordering::Relaxed);
    }

    /// Drain all pending commands from the channel.
    pub fn drain_commands(&mut self) {
        while let Some(cmd) = self.cmd_rx.try_pop() {
            match cmd {
                PlayerCmd::LoadTrack { resource, item_id } => {
                    self.load_track(resource, item_id);
                }
                PlayerCmd::UnloadTrack { item_id } => {
                    if let Some(slot) = self.tracks.slot_of(item_id) {
                        self.unload_slot(slot);
                    }
                }
                PlayerCmd::Clear => {
                    self.clear_all_tracks();
                }
                PlayerCmd::Transition(transition) => {
                    self.handle_transition(transition);
                }
                PlayerCmd::Seek {
                    seconds,
                    seek_epoch,
                } => {
                    self.apply_seek(seconds, seek_epoch);
                }
                PlayerCmd::ScheduleSeek {
                    item_id,
                    seek_epoch,
                    disposition,
                    armed,
                } => {
                    if let Some(track) = self.tracks.get_mut(item_id) {
                        track.schedule_seek(seek_epoch, disposition, armed);
                        if let ScheduledSeekDisposition::PreparedLaunch(identity) = disposition {
                            kithara_test_macros::probe_event!(
                                prepared_launch_command_admitted,
                                item_id = item_id.as_u64(),
                                seek_epoch,
                                activation_output = i64::from(identity.activation),
                                warp_map_revision = u64::from(identity.warp_map),
                                armed
                            );
                        }
                    }
                }
                PlayerCmd::CancelPreparedLaunch {
                    item_id,
                    prepared_seek_epoch,
                    replacement_seek_epoch,
                    transport_seek_epoch,
                    target,
                    resume,
                } => {
                    let presented = self.tracks.get_mut(item_id).is_some_and(|track| {
                        track.replace_prepared_launch(
                            prepared_seek_epoch,
                            replacement_seek_epoch,
                            transport_seek_epoch,
                            target.as_secs_f64(),
                        )
                    });
                    kithara_test_macros::probe_event!(
                        prepared_launch_cancelled,
                        item_id = item_id.as_u64(),
                        prepared_seek_epoch,
                        replacement_seek_epoch,
                        transport_seek_epoch,
                        target_nanos = u64::try_from(target.as_nanos()).unwrap_or(u64::MAX),
                        presented
                    );
                    if presented {
                        self.playback.playing.store(resume, Ordering::SeqCst);
                    }
                }
                PlayerCmd::SetPaused { paused, item_id } => {
                    let mut prepared = false;
                    for (_, track) in self.tracks.iter_mut() {
                        if paused || item_id.is_some_and(|item| item == track.item_id()) {
                            prepared |= track.set_prepared_launch_armed(!paused);
                        }
                    }
                    if prepared {
                        self.playback.playing.store(false, Ordering::SeqCst);
                        continue;
                    }
                    let playing = !paused;
                    self.playback.playing.store(playing, Ordering::SeqCst);
                }
                PlayerCmd::SetFadeDuration(duration) => {
                    self.apply_fade_duration(duration);
                }
                PlayerCmd::SetPrefetchDuration(duration) => {
                    self.apply_prefetch_duration(duration);
                }
            }
        }
    }

    fn handle_transition(&mut self, transition: TrackTransition) {
        let mut leading_changed = false;

        if let TrackTransition::FadeIn(item_id) = &transition {
            self.tracks_transitions.clear();

            let maybe_old = self
                .tracks
                .iter()
                .find_map(|(_, track)| track.state().is_leading().then(|| track.item_id()));

            if let Some(old_id) = maybe_old
                && old_id != *item_id
            {
                leading_changed = true;
                self.tracks_transitions
                    .push_back(TrackTransition::FadeOut(old_id));
            }
        }

        self.tracks_transitions.push_back(transition);
        let playback = Arc::clone(&self.playback);
        let mut changed_src = None;
        self.tracks_transitions.retain(|transition| {
            let item_id = match transition {
                TrackTransition::FadeIn(item_id) | TrackTransition::FadeOut(item_id) => *item_id,
            };
            if let Some(track) = self.tracks.get_mut(item_id) {
                match transition {
                    TrackTransition::FadeIn(_) => {
                        changed_src = Some(Arc::clone(track.src()));
                        if track.position() > Self::FADE_IN_SEEK_THRESHOLD {
                            track.seek(0.0);
                        }
                        track.fade_in();
                        playback.position.store(track.position(), Ordering::Relaxed);
                        playback.duration.store(track.duration(), Ordering::Relaxed);
                    }
                    TrackTransition::FadeOut(_) => {
                        track.fade_out();
                    }
                }
                return false;
            }
            true
        });

        if leading_changed && let Some(new_src) = changed_src {
            self.notif_tx
                .try_push(PlayerNotification::Changed { src: new_src })
                .ok();
        }
    }

    fn load_track(&mut self, resource: Box<PlayerResource>, item_id: TrackId) {
        let src = Arc::clone(resource.src());
        if let Some(slot) = self.tracks.slot_of(item_id) {
            self.unload_slot(slot);
        }
        self.evict_tracks_if_needed();

        resource.set_host_sample_rate(self.sample_rate);

        let track = PlayerTrack::builder()
            .sample_rate(self.sample_rate)
            .item_id(item_id)
            .fade_duration(self.crossfade.duration)
            .prefetch_duration(self.prefetch_duration)
            .fade_curve(self.crossfade.fade_curve())
            .seek_epoch(self.playback.seek_epoch.load(Ordering::SeqCst))
            .build(resource);

        if let Some(rejected) = self.tracks.insert(track) {
            self.discard_track(rejected);
            return;
        }

        self.notif_tx
            .try_push(PlayerNotification::Loaded { src })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use firewheel::{
        clock::InstantSamples,
        dsp::{buffer::ChannelBuffer, declick::DeclickValues},
        event::{NodeEvent, ProcEvents, ProcEventsIndex, ScheduledEventEntry},
        log::{RealtimeLoggerConfig, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{
            AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
            StreamStatus,
        },
    };
    use kithara_audio::{
        SeekPresentation,
        mock::{AudioControlMock, AudioReadMock, AudioSessionMock},
    };
    use kithara_events::EventBus;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::AudioSpec;
    use kithara_test_utils::kithara;
    use kithara_warp::{SessionFrame, WarpMapRevision};
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        bridge::{PreparedLaunchIdentity, ScheduledSeekDisposition, SharedEq, slot_channels},
        resource::Resource,
        rt::StreamShape,
        test_pools::pools,
    };

    fn processor() -> (PlayerNodeProcessor, crate::bridge::SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let shape = StreamShape {
            sample_rate,
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        (
            PlayerNodeProcessor::new(inputs, shape, &pools(), crate::DEFAULT_GATE_SMOOTHING),
            control,
        )
    }

    fn resource(src: Arc<str>) -> Box<PlayerResource> {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let reader = Unimock::new((
            AudioSessionMock::event_bus
                .each_call(matching!())
                .answers(&|mock| mock.make_ref(EventBus::new(1))),
            AudioSessionMock::duration
                .each_call(matching!())
                .returns(Some(Duration::from_secs(1))),
            AudioReadMock::spec
                .each_call(matching!())
                .returns(AudioSpec::new(2, sample_rate)),
            AudioControlMock::preload
                .next_call(matching!())
                .returns(Ok(())),
        ));
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        PlayerResource::new(resource, src, &pools())
            .map(Box::new)
            .unwrap_or_else(|error| panic!("test player resource: {error}"))
    }

    fn resource_with_presentation(
        src: Arc<str>,
        presentation: SeekPresentation,
    ) -> Box<PlayerResource> {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let reader = Unimock::new((
            AudioSessionMock::event_bus
                .each_call(matching!())
                .answers(&|mock| mock.make_ref(EventBus::new(1))),
            AudioSessionMock::duration
                .each_call(matching!())
                .returns(Some(Duration::from_secs(1))),
            AudioReadMock::spec
                .each_call(matching!())
                .returns(AudioSpec::new(2, sample_rate)),
            AudioControlMock::preload
                .next_call(matching!())
                .returns(Ok(())),
            AudioControlMock::present_seek
                .each_call(matching!())
                .returns(presentation),
        ));
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        PlayerResource::new(resource, src, &pools())
            .map(Box::new)
            .unwrap_or_else(|error| panic!("test player resource: {error}"))
    }

    fn prepared_launch() -> ScheduledSeekDisposition {
        ScheduledSeekDisposition::PreparedLaunch(PreparedLaunchIdentity {
            activation: SessionFrame::new(2_000),
            warp_map: WarpMapRevision::first(),
        })
    }

    fn load_with_presentation(
        control: &mut crate::bridge::SlotControl,
        item_id: TrackId,
        src: &str,
        presentation: SeekPresentation,
    ) {
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: resource_with_presentation(Arc::from(src), presentation),
                item_id,
            })
            .expect("fixture command queue has capacity");
    }

    fn load(control: &mut crate::bridge::SlotControl, item_id: TrackId, src: &str) {
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: resource(Arc::from(src)),
                item_id,
            })
            .expect("fixture command queue has capacity");
    }

    #[kithara::test]
    fn armed_prepared_schedule_does_not_release_playback_before_readiness() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load(&mut control, item, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: true,
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();
        assert!(!processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn stale_prepared_launch_cannot_hold_the_current_item_playing() {
        let (mut processor, mut control) = processor();
        let stale = TrackId::allocate();
        let current = TrackId::allocate();
        load(&mut control, stale, "stale.mp3");
        load(&mut control, current, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: stale,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: false,
            })
            .expect("fixture command queue has capacity");
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(current),
            })
            .expect("fixture command queue has capacity");

        processor.drain_commands();

        assert!(processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn current_prepared_launch_stays_paused_after_post_transfer_play() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load(&mut control, item, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: false,
            })
            .expect("fixture command queue has capacity");
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(item),
            })
            .expect("fixture command queue has capacity");

        processor.drain_commands();

        assert!(!processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn cancelling_an_admitted_prepared_launch_releases_requested_playback() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load_with_presentation(&mut control, item, "current.mp3", SeekPresentation::Current);
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: true,
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();
        assert!(!processor.playback().playing.load(Ordering::SeqCst));

        control
            .cmd_tx
            .try_push(PlayerCmd::CancelPreparedLaunch {
                item_id: item,
                prepared_seek_epoch: 7,
                replacement_seek_epoch: 8,
                transport_seek_epoch: 1,
                target: Duration::ZERO,
                resume: true,
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();

        assert!(processor.playback().playing.load(Ordering::SeqCst));
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(item),
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();
        assert!(processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn failed_replacement_presentation_preserves_prepared_epoch_and_playback() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load_with_presentation(
            &mut control,
            item,
            "current.mp3",
            SeekPresentation::Superseded,
        );
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: true,
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();
        processor.playback().position.store(1.0, Ordering::Relaxed);

        control
            .cmd_tx
            .try_push(PlayerCmd::CancelPreparedLaunch {
                item_id: item,
                prepared_seek_epoch: 7,
                replacement_seek_epoch: 8,
                transport_seek_epoch: 1,
                target: Duration::from_secs(2),
                resume: true,
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();

        let track = processor
            .tracks
            .get_mut(item)
            .expect("loaded fixture track");
        assert!(track.set_prepared_launch_armed(true));
        assert_eq!(track.position(), 0.0);
        assert_eq!(processor.playback().position.load(Ordering::Relaxed), 1.0);
        assert_eq!(processor.playback().seek_epoch.load(Ordering::Relaxed), 0);
        assert!(!processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn future_prepared_schedule_survives_cancellation_of_an_older_epoch() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load(&mut control, item, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 7,
                disposition: prepared_launch(),
                armed: true,
            })
            .expect("fixture command queue has capacity");
        control
            .cmd_tx
            .try_push(PlayerCmd::ScheduleSeek {
                item_id: item,
                seek_epoch: 8,
                disposition: prepared_launch(),
                armed: true,
            })
            .expect("fixture command queue has capacity");
        control
            .cmd_tx
            .try_push(PlayerCmd::CancelPreparedLaunch {
                item_id: item,
                prepared_seek_epoch: 7,
                replacement_seek_epoch: 8,
                transport_seek_epoch: 1,
                target: Duration::ZERO,
                resume: false,
            })
            .expect("fixture command queue has capacity");

        processor.drain_commands();

        assert!(
            processor
                .tracks
                .get_mut(item)
                .is_some_and(|track| track.set_prepared_launch_armed(true))
        );
        assert!(!processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn ordinary_launch_play_starts_immediately() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load(&mut control, item, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(item),
            })
            .expect("fixture command queue has capacity");

        processor.drain_commands();

        assert!(processor.playback().playing.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn ordinary_pause_stays_paused_while_the_gate_drains() {
        let (mut processor, mut control) = processor();
        let item = TrackId::allocate();
        load(&mut control, item, "current.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(item)))
            .expect("fixture command queue has capacity");
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(item),
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();
        assert!(processor.playback().playing.load(Ordering::SeqCst));

        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused {
                paused: true,
                item_id: Some(item),
            })
            .expect("fixture command queue has capacity");
        processor.drain_commands();

        let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
        let mut extra = ProcExtra {
            logger,
            store: ProcStore::with_capacity(0),
            scratch_buffers: ChannelBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(512),
            declick_values: DeclickValues::new(NonZeroU32::new(16).expect("static declick length")),
        };
        let info = ProcInfo {
            sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
            frames: 512,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            prev_output_was_silent: false,
            sample_rate_recip: f64::from(44_100).recip(),
            clock_samples: InstantSamples(0),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        let tail_frames = usize::try_from(
            (-(44_100.0 * crate::DEFAULT_GATE_SMOOTHING.smooth_seconds)
                * crate::DEFAULT_GATE_SMOOTHING.settle_epsilon.ln())
            .ceil() as u64,
        )
        .expect("gate-tail frame count fits usize");
        let mut rendered = Vec::new();
        for _ in 0..14 {
            let input: [&[f32]; 0] = [];
            let mut left = [0.0; 512];
            let mut right = [0.0; 512];
            let mut output = [&mut left[..], &mut right[..]];
            let buffers = ProcBuffers {
                inputs: &input,
                outputs: &mut output,
            };
            let mut immediate: [Option<NodeEvent>; 0] = [];
            let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
            let mut indices = Vec::<ProcEventsIndex>::new();
            let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
            let _ = processor.process(&info, buffers, &mut events, &mut extra);

            assert!(!processor.playback().playing.load(Ordering::SeqCst));
            rendered.extend(left);
            assert!(right.iter().all(|sample| *sample == 0.0));
        }
        assert!(rendered[tail_frames..].iter().all(|sample| *sample == 0.0));
    }
}
