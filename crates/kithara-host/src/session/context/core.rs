use std::ops::Deref;

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
    pub(super) inner: Option<FirewheelContext>,
    credits: Credits,
    reserved: bool,
    pub(super) stop: Option<HeapProd<()>>,
    pub(super) output_starting: bool,
    #[cfg(target_arch = "wasm32")]
    pub(super) clock: Option<crate::session::web_output::WebClock>,
}

impl SessionGraph {
    pub(crate) fn new(config: FirewheelConfig) -> Self {
        Self {
            credits: Credits::new(config.channel_capacity),
            inner: Some(FirewheelContext::new(config)),
            reserved: false,
            stop: None,
            output_starting: false,
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

    /// Compiles reserved structural edits; Host settings use their scoped command channel.
    pub(crate) fn update(&mut self) -> Result<(), GraphError> {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &mut self.clock {
            clock.publish().map_err(GraphError::Device)?;
        }
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

    /// Settles returned SDK schedule storage before activation at the admitted geometry.
    pub(crate) fn activate(&mut self, info: ActivateInfo) -> Result<SessionProcessor, GraphError> {
        if !self.is_active() {
            self.inner
                .as_mut()
                .expect("context is owned until drop")
                .update()?;
        }
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

    pub(crate) fn output_starting(&self) -> bool {
        self.output_starting
    }

    pub(crate) fn has_session_clock(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        if let Some(clock) = &self.clock {
            if self.output_starting {
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
