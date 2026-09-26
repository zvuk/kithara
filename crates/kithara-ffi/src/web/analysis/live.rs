mod encode {
    use std::num::NonZeroU32;

    use js_sys::{Float32Array, Float64Array, Object, Reflect};
    use kithara::{
        analysis::{BeatArtifact, BeatSnapshot, BeatState, TrackAnalysis, Waveform},
        queue::TrackId,
    };
    use num_traits::cast;
    use wasm_bindgen::JsValue;

    use crate::analysis::seconds_at;

    pub(super) mod consts {
        /// Scope tag every analysis message carries on the player event channel.
        pub(in super::super) const ANALYSIS_SCOPE: &str = "analysis";
        pub(super) const BANDS_PER_BUCKET: usize = 3;
        pub(super) const STAGE: usize = 768;
    }

    /// One analysis publication as the plain JS object the registered callback
    /// receives.
    pub(crate) fn encode(track_id: TrackId, analysis: &TrackAnalysis) -> Object {
        let rate = analysis.source_sample_rate();
        let beat = analysis.beat();
        let artifact = beat.map(BeatSnapshot::artifact);

        let message = Object::new();
        set(
            &message,
            "scope",
            &JsValue::from_str(consts::ANALYSIS_SCOPE),
        );
        set(&message, "trackId", &number(track_id.as_u64()));
        set(&message, "revision", &number(analysis.revision()));
        set(
            &message,
            "settled",
            &JsValue::from_bool(analysis.is_settled()),
        );
        set(
            &message,
            "sampleRate",
            &JsValue::from_f64(f64::from(rate.get())),
        );
        set(&message, "sourceFrames", &number(analysis.source_frames()));
        set(&message, "waveform", &waveform(analysis).into());
        let beats = artifact.map_or(&[][..], |beat| beat.beats());
        let downbeats = artifact.map_or(&[][..], |beat| beat.downbeats());
        set(&message, "beats", &markers(beats, rate).into());
        set(&message, "downbeats", &markers(downbeats, rate).into());
        set(
            &message,
            "bpm",
            &JsValue::from_f64(artifact.map_or(0.0, BeatArtifact::bpm)),
        );
        set(
            &message,
            "beatFinal",
            &JsValue::from_bool(beat.is_some_and(|snapshot| snapshot.state() == BeatState::Final)),
        );
        message
    }

    fn waveform(analysis: &TrackAnalysis) -> Float32Array {
        let buckets = analysis.waveform().map_or(&[][..], Waveform::buckets);
        let out =
            Float32Array::new_with_length(length_of(buckets.len() * consts::BANDS_PER_BUCKET));
        let mut staged = [0.0_f32; consts::STAGE];
        let mut written: u32 = 0;
        for group in buckets.chunks(consts::STAGE / consts::BANDS_PER_BUCKET) {
            for (slot, bucket) in staged.chunks_exact_mut(consts::BANDS_PER_BUCKET).zip(group) {
                let [low, mid, high] = slot else { continue };
                *low = bucket.low();
                *mid = bucket.mid();
                *high = bucket.high();
            }
            let filled = group.len() * consts::BANDS_PER_BUCKET;
            let end = written.saturating_add(length_of(filled));
            out.subarray(written, end).copy_from(&staged[..filled]);
            written = end;
        }
        out
    }

    fn markers(frames: &[u64], rate: NonZeroU32) -> Float64Array {
        let out = Float64Array::new_with_length(length_of(frames.len()));
        let mut staged = [0.0_f64; consts::STAGE];
        let mut written: u32 = 0;
        for group in frames.chunks(consts::STAGE) {
            for (slot, frame) in staged.iter_mut().zip(group) {
                *slot = seconds_at(*frame, rate);
            }
            let end = written.saturating_add(length_of(group.len()));
            out.subarray(written, end).copy_from(&staged[..group.len()]);
            written = end;
        }
        out
    }

    fn length_of(count: usize) -> u32 {
        cast(count).unwrap_or(u32::MAX)
    }

    fn number(value: u64) -> JsValue {
        JsValue::from_f64(cast(value).unwrap_or(f64::MAX))
    }

    fn set(target: &Object, key: &str, value: &JsValue) {
        let _ = Reflect::set(target, &JsValue::from_str(key), value);
    }
}

mod route {
    use js_sys::{Function, Object, Reflect};
    use kithara::platform::sync::{Arc, Mutex};
    use send_wrapper::SendWrapper;
    use wasm_bindgen::{JsCast, JsValue};

    use super::encode::consts::ANALYSIS_SCOPE;

    #[derive(Clone, Default)]
    pub(crate) struct AnalysisRoute {
        sink: Arc<Mutex<Option<SendWrapper<Function>>>>,
    }

    impl AnalysisRoute {
        pub(crate) fn dispatch(&self, scope: Option<&str>, data: &JsValue) -> bool {
            if scope != Some(ANALYSIS_SCOPE) {
                return false;
            }
            let func = self.sink.lock().as_ref().map(|func| (*func).clone());
            if let Some(func) = func {
                if let Some(payload) = data.dyn_ref::<Object>() {
                    let _ = Reflect::delete_property(payload, &JsValue::from_str("scope"));
                }
                let _ = func.call1(&JsValue::UNDEFINED, data);
            }
            true
        }

        pub(crate) fn set(&self, func: Function) {
            *self.sink.lock() = Some(SendWrapper::new(func));
        }
    }
}

mod runs {
    use std::{cell::RefCell, collections::HashMap, num::NonZeroU32, rc::Rc};

    use kithara::{
        analysis::{
            AnalysisDemand, AnalysisProgress, AnalysisToken, AnalysisWorker, AnalysisWorkerConfig,
            AnalyzerBuilder, BeatAnalysisConfig,
        },
        audio::AudioReader,
        platform::{
            CancelToken,
            sync::Arc,
            tokio::{self, sync::watch, task::spawn as task_spawn},
        },
        prelude::{PlaybackResamplerBackend, Resource},
        queue::TrackId,
    };
    use web_sys::BroadcastChannel;

    use super::encode::encode;
    use crate::{
        pools::{FfiPools, FfiQueueControl, FfiResourceConfig, Pools},
        web::{interop::send_reply, observer::source::consts::EVENT_CHANNEL},
    };

    mod consts {
        pub(super) const CALLER_HOLDS_NO_REVISION: u64 = 0;
        pub(super) const WAVEFORM_MAX_BUCKETS: usize = 96_000;
    }

    type WebAnalyzerBuilder = AnalyzerBuilder<PlaybackResamplerBackend, FfiPools>;
    type Live = Rc<RefCell<HashMap<TrackId, CancelToken>>>;

    /// The engine worker's analysis owner: one shared [`AnalysisWorker`] and the
    /// cancel token of the single live pass per track.
    pub(crate) struct AnalysisRuns {
        worker: Arc<AnalysisWorker>,
        live: Live,
    }

    impl AnalysisRuns {
        pub(crate) fn new(pools: Pools) -> Self {
            let builder: WebAnalyzerBuilder = AnalyzerBuilder::new(pools)
                .with_beat_config(BeatAnalysisConfig::default())
                .with_waveform(consts::WAVEFORM_MAX_BUCKETS)
                .with_beat();
            Self {
                worker: Arc::new(AnalysisWorker::new(
                    AnalysisWorkerConfig::for_builder(builder).build(),
                )),
                live: Live::default(),
            }
        }

        pub(crate) fn cancel(&mut self, id: TrackId) {
            if let Some(cancel) = self.live.borrow_mut().remove(&id) {
                cancel.cancel();
            }
        }

        pub(crate) fn clear(&mut self) {
            for (_, cancel) in self.live.borrow_mut().drain() {
                cancel.cancel();
            }
        }

        /// Open a pass for `id` on the queue's decoded-audio axis.
        pub(crate) fn start(
            &mut self,
            queue: &FfiQueueControl,
            config: FfiResourceConfig,
            id: TrackId,
            token: AnalysisToken,
            request_id: u32,
        ) {
            let Some(rate) = NonZeroU32::new(queue.sample_rate()) else {
                send_reply(
                    request_id,
                    Err("the queue reports no sample rate".to_owned()),
                );
                return;
            };
            self.cancel(id);

            let (rx, producer, pass) = match self.worker.open(
                token,
                rate,
                consts::CALLER_HOLDS_NO_REVISION,
                AnalysisDemand::ALL,
            ) {
                Ok(opened) => opened,
                Err(error) => {
                    send_reply(request_id, Err(error.to_string()));
                    return;
                }
            };
            let cancel = pass.cancel_token().clone();
            self.live.borrow_mut().insert(id, cancel.clone());

            let worker = Arc::clone(&self.worker);
            let reader_cancel = cancel.clone();
            let queue = queue.clone();
            task_spawn(async move {
                match open_reader(config, &reader_cancel, rate).await {
                    Ok(reader) => {
                        queue.attach_observer(id, producer);
                        worker.start(pass, reader);
                        send_reply(request_id, Ok(()));
                    }
                    Err(error) => {
                        tracing::warn!(?error, "analysis: reader did not open; pass never started");
                        send_reply(request_id, Err(error));
                    }
                }
            });
            let live = Rc::clone(&self.live);
            task_spawn(async move {
                publish(id, rx, &cancel).await;
                if !cancel.is_cancelled() {
                    live.borrow_mut().remove(&id);
                }
            });
        }

        pub(crate) fn start_queued<F>(
            &mut self,
            queue: &FfiQueueControl,
            id: TrackId,
            request_id: u32,
            config_for: F,
        ) -> Result<(), String>
        where
            F: FnOnce(&str) -> Option<FfiResourceConfig>,
        {
            let source = queue
                .track_source(id)
                .ok_or_else(|| format!("track {id:?} is not queued"))?;
            let token = source
                .uri()
                .map(AnalysisToken::from)
                .ok_or_else(|| format!("track {id:?} has a source with no readable location"))?;
            let config = match source {
                crate::pools::FfiTrackSource::Config(config) => *config,
                crate::pools::FfiTrackSource::Uri(ref url) => config_for(url).ok_or_else(|| {
                    format!("track {id:?} carries a url kithara cannot parse: {url}")
                })?,
                _ => {
                    return Err(format!(
                        "track {id:?} carries a source this build cannot open"
                    ));
                }
            };
            self.start(queue, config, id, token, request_id);
            Ok(())
        }
    }

    async fn publish(
        id: TrackId,
        mut rx: watch::Receiver<Option<AnalysisProgress>>,
        cancel: &CancelToken,
    ) {
        let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
            tracing::warn!("analysis: BroadcastChannel unavailable in worker");
            return;
        };
        loop {
            let changed = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                changed = rx.changed() => changed,
            };
            if changed.is_err() {
                return;
            }
            let message = rx
                .borrow_and_update()
                .as_ref()
                .map(|progress| encode(id, progress.analysis()));
            if let Some(message) = message {
                let _ = channel.post_message(&message);
            }
        }
    }

    async fn open_reader(
        mut config: FfiResourceConfig,
        cancel: &CancelToken,
        rate: NonZeroU32,
    ) -> Result<Box<dyn AudioReader>, String> {
        if cancel.is_cancelled() {
            return Err("the pass was cancelled before its reader opened".to_owned());
        }
        config.set_cancel(cancel.child());
        config.set_host_sample_rate(rate);
        let mut resource = Resource::new(config)
            .await
            .map_err(|error| format!("resource open failed: {error}"))?;
        resource
            .preload()
            .await
            .map_err(|error| format!("preload failed: {error}"))?;
        Ok(resource.into())
    }
}

pub(crate) use route::AnalysisRoute;
pub(crate) use runs::AnalysisRuns;
