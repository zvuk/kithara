use std::num::NonZeroU32;

use bevy_platform::time::Instant;
use firewheel::ActivateInfo;
use num_traits::ToPrimitive;
use wasm_bindgen::prelude::*;

use super::{
    clock::WebClock,
    worklet::{ClockStamp, HostOutput},
};
use crate::session::{
    context::SessionGraph,
    state::{DeckNode, SessionStream},
    transport::TransportState,
};

#[wasm_bindgen(module = "/src/session/web_output/device.js")]
extern "C" {
    #[wasm_bindgen(js_name = HostDevice)]
    #[derive(Clone)]
    pub(super) type OutputLease;
    #[wasm_bindgen(constructor, catch, js_class = HostDevice)]
    fn new(
        rate: u32,
        root: &str,
        script: &str,
        module: &JsValue,
        memory: &JsValue,
        channels: u32,
    ) -> Result<OutputLease, JsValue>;
    #[wasm_bindgen(method, getter, js_name = sampleRate)]
    fn sample_rate(this: &OutputLease) -> f64;
    #[wasm_bindgen(method, getter, js_name = currentTime)]
    pub(super) fn current_time(this: &OutputLease) -> f64;
    #[wasm_bindgen(method, getter)]
    fn acquired(this: &OutputLease) -> bool;
    #[wasm_bindgen(method, getter)]
    pub(super) fn ready(this: &OutputLease) -> bool;
    #[wasm_bindgen(method, getter)]
    pub(super) fn error(this: &OutputLease) -> Option<String>;
    #[wasm_bindgen(method, catch)]
    fn install(this: &OutputLease, owner: JsValue) -> Result<(), JsValue>;
    #[wasm_bindgen(method)]
    fn fail(this: &OutputLease, error: &str);
    #[wasm_bindgen(method)]
    fn close(this: &OutputLease);
}

#[wasm_bindgen]
extern "C" {
    type ImportMeta;
    #[wasm_bindgen(method, getter)]
    fn url(this: &ImportMeta) -> String;
    #[wasm_bindgen(thread_local_v2, js_namespace = import, js_name = meta)]
    static IMPORT_META: ImportMeta;
}

pub(crate) struct WebOutput {
    lease: OutputLease,
    clock: Option<triple_buffer::Output<ClockStamp>>,
    info: Option<ActivateInfo>,
    requested: u32,
}

impl WebOutput {
    fn acquire(graph: &mut SessionGraph, requested: u32) -> Result<Self, String> {
        let root = IMPORT_META.with(ImportMeta::url);
        let lease = OutputLease::new(
            requested,
            &root,
            include_str!("worklet.js"),
            &wasm_bindgen::module(),
            &wasm_bindgen::memory(),
            2,
        )
        .map_err(|error| format!("audio acquisition failed: {error:?}"))?;
        let mut acquired = Self {
            lease,
            clock: None,
            info: None,
            requested,
        };
        let reported = acquired.lease.sample_rate();
        let rate = reported
            .to_u32()
            .filter(|rate| f64::from(*rate) == reported)
            .and_then(NonZeroU32::new)
            .ok_or("invalid actual AudioContext sample rate")?;
        let block = NonZeroU32::new(
            u32::try_from(crate::consts::WEB_RENDER_QUANTUM).map_err(|error| error.to_string())?,
        )
        .ok_or("empty web processing quantum")?;
        let info = ActivateInfo {
            sample_rate: rate,
            max_block_frames: block,
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
        };
        let (input, output) = triple_buffer::triple_buffer(&ClockStamp {
            main: Instant::now(),
            audio: acquired.lease.current_time(),
        });
        acquired.clock = Some(output);
        acquired.info = Some(info);
        graph.install_web_clock(WebClock {
            lease: acquired.lease.clone(),
            input,
            requested,
            acknowledged: false,
            info,
            initial: graph.initial_output(),
        });
        Ok(acquired)
    }

    pub(crate) fn start<S>(
        graph: &mut SessionGraph,
        requested: u32,
        decks: &mut [DeckNode],
        previous: Option<SessionStream<Self>>,
    ) -> Result<Self, String>
    where
        S: kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
    {
        let mut acquired = match previous {
            Some(SessionStream::Realtime { _backend }) => _backend,
            None => Self::acquire(graph, requested)?,
            #[cfg(feature = "offline")]
            Some(SessionStream::Offline(_)) => {
                return Err("an offline stream cannot become web output".into());
            }
        };
        if acquired.requested != requested {
            return Err("the previous output acquisition belongs to another requested rate".into());
        }
        if let Some(error) = acquired.lease.error() {
            return Err(error);
        }
        if let Err(error) = acquired.advance::<S>(graph, decks) {
            acquired.lease.fail(&error);
            return Err(error);
        }
        Ok(acquired)
    }

    fn advance<S>(&mut self, graph: &mut SessionGraph, decks: &mut [DeckNode]) -> Result<(), String>
    where
        S: kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
    {
        if self.clock.is_none() {
            return Ok(());
        }
        let info = self.info.ok_or("actual web format is unavailable")?;
        let shape = kithara_render::rt::StreamShape::new(info.max_block_frames, info.sample_rate);
        graph.cycle_detected().map_err(|error| error.to_string())?;
        let mut retained = Vec::<crate::DeckId>::new();
        for deck in decks
            .iter_mut()
            .filter(|deck| graph.node_info(deck.node).is_some())
        {
            let initial = kithara_render::rt::PlayerNode::<S, TransportState>::prepare_if_initial(
                graph, deck.node, shape,
            )
            .map_err(|error| error.to_string())?;
            if !initial {
                deck.owner
                    .reserve_stream()
                    .map_err(|error| error.to_string())?;
                deck.owner
                    .prepare_stream(shape)
                    .map_err(|error| error.to_string())?;
                retained.push(deck.id);
            }
        }
        if !self.lease.acquired() {
            return Ok(());
        }
        if graph.is_active() {
            return Err("previous web processor is still active".into());
        }
        let processor = graph.activate(info).map_err(|error| error.to_string())?;
        for deck in decks.iter_mut().filter(|deck| retained.contains(&deck.id)) {
            if let Err(error) = deck.owner.resume_stream() {
                drop(processor);
                return Err(error.to_string());
            }
        }
        let output = self
            .clock
            .take()
            .ok_or("web clock receiver already transferred")?;
        let owner = HostOutput::new(processor, output, info.sample_rate);
        self.lease
            .install(owner.into())
            .map_err(|error| format!("audio installation failed: {error:?}"))?;
        if let Some(error) = self.lease.error() {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for WebOutput {
    fn drop(&mut self) {
        self.lease.close();
    }
}

#[cfg(test)]
mod tests {
    use firewheel::FirewheelConfig;
    use kithara_test_utils::bufpool::TestPools;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    fn cancellation_before_bootstrap_does_not_activate_the_framework_or_advance_its_clock() {
        let mut graph = SessionGraph::new(FirewheelConfig::default());
        let before = graph.heartbeat();
        let output = WebOutput::start::<TestPools>(&mut graph, 44_100, &mut [], None)
            .expect("real AudioContext acquisition");
        assert!(graph.negotiated_shape().is_some());
        assert!(graph.output_starting());
        assert!(
            !graph.is_active(),
            "asynchronous device creation precedes framework activation"
        );
        drop(output);
        assert_eq!(graph.heartbeat(), before);
        assert!(!graph.is_active());
    }
}
