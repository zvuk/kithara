use kithara_command::{LevelInbox, ScopeId};
use kithara_output::LiveOutput;
use kithara_platform::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use kithara_signal::AudioSpec;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use triple_buffer::{Input, Output, triple_buffer};

use super::{DeckEvent, DeckProtocol, DeckSnapshot};
use crate::rt::DeckMixerConfig;

/// Events a deck's mixer can hold for its owner per slot before it counts an overflow.
/// The scope identity and observation ends of one deck's mixer.
#[non_exhaustive]
pub struct DeckEnds {
    pub scope: ScopeId,
    pub events: DeckEvents,
    pub snapshot: Output<DeckSnapshot>,
}

/// The mixer's ends of the same channels, taken by the mixer when it is built.
#[non_exhaustive]
pub struct MixerInputs {
    pub(crate) scope: ScopeId,
    pub(crate) events: HeapProd<DeckEvent>,
    pub(crate) snapshot: Input<DeckSnapshot>,
    pub(crate) config: DeckMixerConfig,
}

/// Borrows a deck's command level from the session's processor store.
pub trait SessionInbox: Send + 'static {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>>;
}

/// Events a deck's mixer reported, in the order it reported them.
pub struct DeckEvents(HeapCons<DeckEvent>);

impl DeckEvents {
    /// Every event reported since the last call.
    pub fn drain(&mut self) -> impl Iterator<Item = DeckEvent> + '_ {
        std::iter::from_fn(|| self.0.try_pop())
    }
}

/// The channels between a deck's owner and the mixer `config` builds.
#[must_use]
pub fn scope_channels(scope: ScopeId, config: DeckMixerConfig) -> (DeckEnds, MixerInputs) {
    let slots = config.slots().get();
    let (events_tx, events_rx) =
        HeapRb::<DeckEvent>::new(slots * crate::consts::EVENTS_PER_SLOT).split();
    let initial = DeckSnapshot::new(config);
    let (snapshot_in, snapshot_out) = triple_buffer(&initial);
    (
        DeckEnds {
            scope,
            events: DeckEvents(events_rx),
            snapshot: snapshot_out,
        },
        MixerInputs {
            scope,
            config,
            events: events_tx,
            snapshot: snapshot_in,
        },
    )
}

/// Producer for interleaved stereo mix samples and their drop count.
#[non_exhaustive]
pub struct MixTapWriter {
    pub(crate) drops: Arc<AtomicU64>,
    pub(crate) samples: HeapProd<f32>,
}

impl MixTapWriter {
    #[must_use]
    pub fn new(samples: HeapProd<f32>, drops: Arc<AtomicU64>) -> Self {
        Self { drops, samples }
    }
}

impl From<MixTapWriter> for (HeapProd<f32>, Arc<AtomicU64>) {
    fn from(writer: MixTapWriter) -> Self {
        (writer.samples, writer.drops)
    }
}

impl LiveOutput for MixTapWriter {
    fn reconfigure(&mut self, _spec: AudioSpec) {}

    fn write_stereo(&mut self, frames: usize, left: &[f32], right: &[f32]) {
        let stereo = 2;
        let writable = frames
            .min(left.len())
            .min(right.len())
            .min(self.samples.vacant_len() / stereo);
        let pushed = self.samples.push_iter(
            left[..writable]
                .iter()
                .zip(&right[..writable])
                .flat_map(|(&left, &right)| [left, right]),
        );
        let dropped = frames.saturating_mul(stereo).saturating_sub(pushed);
        if dropped > 0 {
            self.drops.fetch_add(
                u64::try_from(dropped).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
    }
}
