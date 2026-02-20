#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
use std::sync::{Arc, Mutex};

#[cfg(target_arch = "wasm32")]
use firewheel::{
    FirewheelConfig, FirewheelCtx, Volume,
    channel_config::ChannelCount,
    cpal::{CpalBackend, CpalConfig, CpalOutputConfig},
    diff::Memo,
    node::NodeID,
    nodes::beep_test::BeepTestNode,
};
#[cfg(target_arch = "wasm32")]
use tracing::info;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
type DemoCtx = FirewheelCtx<CpalBackend>;

#[cfg(target_arch = "wasm32")]
const MAX_EVENTS: usize = 256;

#[cfg(target_arch = "wasm32")]
fn js_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

#[cfg(target_arch = "wasm32")]
fn push_event(event_log: &Arc<Mutex<Vec<String>>>, line: String) {
    let Ok(mut events) = event_log.lock() else {
        return;
    };
    events.push(line);
    if events.len() > MAX_EVENTS {
        let remove = events.len() - MAX_EVENTS;
        events.drain(0..remove);
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn setup() {
    console_error_panic_hook::set_once();
    let _ = tracing_log::LogTracer::init();
    tracing_wasm::set_as_global_default();
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct BeepDemo {
    beep_memo: Option<Memo<BeepTestNode>>,
    beep_node_id: Option<NodeID>,
    ctx: Option<DemoCtx>,
    event_log: Arc<Mutex<Vec<String>>>,
    tick_count: u64,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl BeepDemo {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let event_log = Arc::new(Mutex::new(Vec::new()));
        push_event(&event_log, "demo created".to_string());
        Self {
            beep_memo: None,
            beep_node_id: None,
            ctx: None,
            event_log,
            tick_count: 0,
        }
    }

    pub fn start(&mut self) -> Result<(), JsValue> {
        if self.ctx.is_some() {
            push_event(
                &self.event_log,
                "start skipped: already running".to_string(),
            );
            return Ok(());
        }

        let mut ctx = DemoCtx::new(FirewheelConfig {
            num_graph_outputs: ChannelCount::STEREO,
            ..FirewheelConfig::default()
        });

        let config = CpalConfig {
            output: CpalOutputConfig {
                desired_sample_rate: Some(48_000),
                ..CpalOutputConfig::default()
            },
            ..CpalConfig::default()
        };

        ctx.start_stream(config)
            .map_err(|err| js_error(format!("start_stream failed: {err}")))?;

        let beep = BeepTestNode {
            freq_hz: 440.0,
            volume: Volume::Decibels(-18.0),
            enabled: true,
        };
        let beep_memo = Memo::new(beep);
        let beep_node_id = ctx.add_node(beep, None);
        let graph_out_id = ctx.graph_out_node_id();

        ctx.connect(beep_node_id, graph_out_id, &[(0, 0), (0, 1)], false)
            .map_err(|err| js_error(format!("connect failed: {err}")))?;
        ctx.update()
            .map_err(|err| js_error(format!("initial update failed: {err:?}")))?;

        let sample_rate = ctx.stream_info().map_or(0, |info| info.sample_rate.get());
        info!("beep stream started sample_rate={sample_rate}");
        push_event(
            &self.event_log,
            format!("playback started sample_rate={sample_rate} freq_hz=440"),
        );

        self.ctx = Some(ctx);
        self.beep_node_id = Some(beep_node_id);
        self.beep_memo = Some(beep_memo);
        self.tick_count = 0;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(ctx) = self.ctx.as_mut() {
            ctx.stop_stream();
        }
        self.ctx = None;
        self.beep_node_id = None;
        self.beep_memo = None;
        push_event(&self.event_log, "playback stopped".to_string());
    }

    pub fn is_running(&self) -> bool {
        self.ctx.is_some()
    }

    pub fn set_freq_hz(&mut self, freq_hz: f32) -> Result<(), JsValue> {
        let node_id = self
            .beep_node_id
            .ok_or_else(|| js_error("set_freq_hz: stream is not running"))?;
        let memo = self
            .beep_memo
            .as_mut()
            .ok_or_else(|| js_error("set_freq_hz: memo is missing"))?;
        let ctx = self
            .ctx
            .as_mut()
            .ok_or_else(|| js_error("set_freq_hz: context is missing"))?;

        let clamped = freq_hz.clamp(20.0, 20_000.0);
        memo.freq_hz = clamped;
        {
            let mut queue = ctx.event_queue(node_id);
            memo.update_memo(&mut queue);
        }
        ctx.update()
            .map_err(|err| js_error(format!("set_freq_hz update failed: {err:?}")))?;

        push_event(&self.event_log, format!("playback freq_hz={clamped:.1}"));
        Ok(())
    }

    pub fn tick(&mut self) -> Result<(), JsValue> {
        let Some(ctx) = self.ctx.as_mut() else {
            return Ok(());
        };

        ctx.update()
            .map_err(|err| js_error(format!("tick failed: {err:?}")))?;
        self.tick_count = self.tick_count.saturating_add(1);

        if self.tick_count % 20 == 0 {
            let sample_rate = ctx.stream_info().map_or(0, |info| info.sample_rate.get());
            push_event(
                &self.event_log,
                format!(
                    "playback progress ticks={} sample_rate={sample_rate}",
                    self.tick_count
                ),
            );
        }

        Ok(())
    }

    pub fn take_events(&self) -> String {
        let Ok(mut events) = self.event_log.lock() else {
            return String::new();
        };
        if events.is_empty() {
            return String::new();
        }
        let out = events.join("\n");
        events.clear();
        out
    }
}
