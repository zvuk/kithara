use std::ops::Deref;

#[cfg(not(target_arch = "wasm32"))]
use firewheel::error::DeactivateError;
use firewheel::{
    ActivateInfo, FirewheelConfig, FirewheelContext,
    error::CompileGraphError,
    graph::{Edge, EdgeID, PortIdx},
    node::{AudioNode, NodeID, ProcStore},
};
use ringbuf::{
    HeapProd, HeapRb,
    traits::{Producer, Split},
};
use smallvec::SmallVec;

use super::{GraphError, credits::Credits};
use crate::session::processor::SessionProcessor;

/// Owns every writer of the Host's framework queue, reserving before graph mutation.
pub(crate) struct SessionGraph {
    inner: Option<FirewheelContext>,
    credits: Credits,
    reserved: bool,
    stop: Option<HeapProd<()>>,
    #[cfg(target_arch = "wasm32")]
    clock: Option<crate::session::web_output::WebClock>,
}

impl SessionGraph {
    pub(crate) fn new(config: FirewheelConfig) -> Self {
        Self {
            credits: Credits::new(config.channel_capacity),
            inner: Some(FirewheelContext::new(config)),
            reserved: false,
            stop: None,
            #[cfg(target_arch = "wasm32")]
            clock: None,
        }
    }

    fn observe(&mut self) {
        if self.is_active() {
            self.credits.observe(self.heartbeat());
        } else {
            self.credits.returned();
        }
    }

    pub(crate) fn quiescent(&mut self) -> bool {
        self.observe();
        !self.reserved && self.credits.empty()
    }

    fn reserve(&mut self) -> Result<(), GraphError> {
        self.observe();
        if !self.reserved && !self.credits.available() {
            return Err(GraphError::Full);
        }
        self.reserved = true;
        Ok(())
    }

    pub(crate) fn add_node<T: AudioNode + 'static>(
        &mut self,
        node: T,
        config: Option<T::Configuration>,
    ) -> Result<NodeID, GraphError> {
        self.reserve()?;
        self.inner
            .as_mut()
            .expect("context is owned until drop")
            .add_node(node, config)
            .map_err(GraphError::Node)
    }

    pub(crate) fn connect(
        &mut self,
        source: NodeID,
        destination: NodeID,
        ports: &[(PortIdx, PortIdx)],
        cycles: bool,
    ) -> Result<SmallVec<[EdgeID; 4]>, GraphError> {
        self.reserve()?;
        self.inner
            .as_mut()
            .expect("context is owned until drop")
            .connect(source, destination, ports, cycles)
            .map_err(GraphError::from)
    }

    pub(crate) fn remove_node(&mut self, node: NodeID) -> Result<SmallVec<[Edge; 4]>, GraphError> {
        self.reserve()?;
        self.inner
            .as_mut()
            .expect("context is owned until drop")
            .remove_node(node)
            .map_err(GraphError::from)
    }

    pub(crate) fn update(&mut self) -> Result<(), GraphError> {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &mut self.clock {
            clock.publish().map_err(GraphError::Device)?;
        }
        // Host nodes do not enqueue framework events; settings use their scoped command channel.
        // Only the reserved structural compile can write a framework message here.
        self.inner
            .as_mut()
            .expect("context is owned until drop")
            .update()?;
        if self.reserved && self.is_active() {
            self.credits.sent(self.heartbeat());
            self.reserved = false;
        }
        Ok(())
    }

    pub(crate) fn activate(&mut self, info: ActivateInfo) -> Result<SessionProcessor, GraphError> {
        self.reserve()?;
        let processor = self
            .inner
            .as_mut()
            .expect("context is owned until drop")
            .activate(info)?;
        self.credits.sent(self.heartbeat());
        self.reserved = false;
        let (stop, stopped) = HeapRb::new(1).split();
        self.stop = Some(stop);
        Ok(SessionProcessor::new(
            processor,
            usize::try_from(info.num_stream_out_channels).expect("output channels fit usize"),
            stopped,
        ))
    }

    pub(crate) fn request_deactivate(&mut self) {
        if let Some(stop) = &mut self.stop {
            let _ = stop.try_push(());
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn install_web_clock(&mut self, clock: crate::session::web_output::WebClock) {
        self.clock = Some(clock);
    }

    pub(crate) fn output_starting(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            return clock.starting();
        }
        false
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn initial_output(&self) -> bool {
        self.stop.is_none()
    }

    pub(crate) fn has_session_clock(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            if clock.starting() {
                return clock.initial();
            }
        }
        self.stream_info().is_some()
    }

    pub(crate) fn negotiated_shape(&self) -> Option<kithara_render::rt::StreamShape> {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            let info = clock.info();
            return Some(kithara_render::rt::StreamShape::new(
                info.max_block_frames,
                info.sample_rate,
            ));
        }
        self.stream_info().map(|info| {
            kithara_render::rt::StreamShape::new(info.max_block_frames, info.sample_rate)
        })
    }

    pub(crate) fn output_ready(&self) -> Result<bool, GraphError> {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            return clock.ready().map_err(GraphError::Device);
        }
        Ok(true)
    }

    pub(crate) fn requested_output_rate(&self) -> Option<u32> {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            return Some(clock.requested());
        }
        None
    }

    pub(crate) fn acknowledge_output(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &mut self.clock {
            clock.acknowledge();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn deactivate_blocking(
        &mut self,
        timeout: kithara_platform::time::Duration,
    ) -> Result<(), DeactivateError> {
        let until = kithara_platform::time::Instant::now() + timeout;
        self.request_deactivate();
        while self.is_active() {
            if kithara_platform::time::Instant::now() >= until {
                return Err(DeactivateError::TimedOut);
            }
            std::thread::yield_now();
        }
        Ok(())
    }

    delegate::delegate! {
        to self.inner.as_mut().expect("context is owned until drop") {
            pub(crate) fn cycle_detected(&mut self) -> Result<(), CompileGraphError>;
            pub(crate) fn proc_store_mut(&mut self) -> Option<&mut ProcStore>;
        }
    }
}

impl Deref for SessionGraph {
    type Target = FirewheelContext;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().expect("context is owned until drop")
    }
}

impl Drop for SessionGraph {
    fn drop(&mut self) {
        self.request_deactivate();
        #[cfg(target_arch = "wasm32")]
        if let Some(context) = self.inner.take() {
            wasm_bindgen_futures::spawn_local(async move {
                while context.is_active() {
                    kithara_platform::time::sleep(crate::consts::SESSION_PUMP_INTERVAL).await;
                }
                drop(context);
            });
        }
    }
}
