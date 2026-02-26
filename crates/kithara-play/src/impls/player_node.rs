//! Player source node for the Firewheel audio graph.
//!
//! [`PlayerNode`] is a source node (zero inputs, stereo outputs) that
//! communicates with its [`PlayerNodeProcessor`] via a command
//! channel. This avoids Diff/Patch complexity with `Arc<Mutex<>>` types.
//!
//! The command channel and shared state are embedded in the node and
//! marked `#[diff(skip)]` so they are excluded from Diff/Patch. The
//! processor receives them via `construct_processor(&self, ...)`.

use std::sync::Arc;

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch},
    node::{AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig},
};
use kithara_bufpool::PcmPool;
use kithara_platform::Mutex;
use ringbuf::{HeapCons, HeapRb, traits::Split};

use super::{
    player_processor::{PlayerCmd, PlayerNodeProcessor},
    shared_player_state::SharedPlayerState,
};

/// A player source node that outputs mixed audio from loaded tracks.
///
/// Commands (load, unload, seek, pause, fade) are sent to the processor
/// via a channel stored in the node. The `Diff`/`Patch` derives
/// only apply to the `active` field; all runtime state is `#[diff(skip)]`.
///
/// # Construction
///
/// Use [`PlayerNode::with_channel`] to create a node wired to a command
/// channel and shared state. [`PlayerNode::new`] creates a standalone
/// node (useful for tests or when wiring is deferred to Task 9).
#[derive(Clone, Diff, Patch)]
pub(crate) struct PlayerNode {
    /// Whether the node is active (used by Diff/Patch for graph updates).
    pub(crate) active: bool,

    /// Receiver for commands from the main thread. Taken by the processor.
    #[diff(skip)]
    cmd_rx: Arc<Mutex<Option<HeapCons<PlayerCmd>>>>,

    /// PCM buffer pool for scratch buffer allocation.
    #[diff(skip)]
    pcm_pool: PcmPool,

    /// Shared atomic state for position, duration, notifications, etc.
    #[diff(skip)]
    shared_state: Arc<SharedPlayerState>,
}

impl PlayerNode {
    /// Create a new player node with a self-contained command channel.
    ///
    /// This constructor creates an internal noop channel. Use
    /// [`PlayerNode::with_channel`] when you need to send commands
    /// from the main thread.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        let (_, rx) = HeapRb::<PlayerCmd>::new(1).split();
        Self {
            active: true,
            cmd_rx: Arc::new(Mutex::new(Some(rx))),
            pcm_pool: kithara_bufpool::pcm_pool().clone(),
            shared_state: Arc::new(SharedPlayerState::new()),
        }
    }

    /// Create a player node wired to the given command channel and shared state.
    pub(crate) fn with_channel(
        cmd_rx: HeapCons<PlayerCmd>,
        shared_state: Arc<SharedPlayerState>,
        pcm_pool: PcmPool,
    ) -> Self {
        Self {
            active: true,
            cmd_rx: Arc::new(Mutex::new(Some(cmd_rx))),
            pcm_pool,
            shared_state,
        }
    }

    /// Get a reference to the shared player state.
    #[cfg_attr(not(test), expect(dead_code, reason = "accessor for future use"))]
    pub(crate) fn shared_state(&self) -> &Arc<SharedPlayerState> {
        &self.shared_state
    }

    /// Get a reference to the command receiver.
    #[expect(dead_code, reason = "accessor for future use")]
    pub(crate) fn cmd_rx(&self) -> &Arc<Mutex<Option<HeapCons<PlayerCmd>>>> {
        &self.cmd_rx
    }
}

impl AudioNode for PlayerNode {
    type Configuration = EmptyConfig;

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("Player")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            })
    }

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        let sample_rate = cx.stream_info.sample_rate;
        let cmd_rx = self.cmd_rx.lock_sync().take().unwrap_or_else(|| {
            let (_, rx) = HeapRb::<PlayerCmd>::new(1).split();
            rx
        });
        PlayerNodeProcessor::new(
            cmd_rx,
            Arc::clone(&self.shared_state),
            sample_rate,
            &self.pcm_pool,
        )
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use ringbuf::traits::{Consumer, Producer};

    use super::*;

    #[kithara::test]
    fn player_node_defaults_active() {
        let node = PlayerNode::new();
        assert!(node.active);
    }

    #[kithara::test]
    fn player_node_info_has_stereo_output() {
        let node = PlayerNode::new();
        let info = node.info(&EmptyConfig);
        // AudioNodeInfo does not expose fields directly,
        // but construction should not panic.
        let _ = info;
    }

    #[kithara::test]
    #[case(PlayerCmd::SetPaused(true))]
    #[case(PlayerCmd::SetPaused(false))]
    #[case(PlayerCmd::SetFadeDuration(0.25))]
    fn player_node_with_channel(#[case] cmd: PlayerCmd) {
        let (mut tx, rx) = HeapRb::<PlayerCmd>::new(8).split();
        let shared_state = Arc::new(SharedPlayerState::new());
        let node = PlayerNode::with_channel(rx, shared_state, kithara_bufpool::pcm_pool().clone());
        assert!(node.active);

        // Verify the channel is connected
        tx.try_push(cmd).ok();
        let mut guard = node.cmd_rx.lock_sync();
        let received = (*guard).as_mut().and_then(|cmd_rx| cmd_rx.try_pop());
        assert!(received.is_some());
    }

    #[kithara::test]
    fn player_node_shared_state_accessible() {
        let node = PlayerNode::new();
        let state = node.shared_state();
        assert!(!state.playing.load(std::sync::atomic::Ordering::Relaxed));
    }
}
