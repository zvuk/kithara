use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_sync::LoadGeneration;
use ringbuf::traits::{Observer, Producer};

use super::core::EngineImpl;
use crate::{
    api::{CrossfadeSettings, SlotId},
    bridge::{PlayerCmd, TrackTransition},
    error::PlayError,
    rt::track::PlayerResource,
};

/// Capacity held for one off-RT load while its resource is moved out of the
/// queue. Other producers cannot consume these entries before the load sends.
pub(crate) struct SlotLoadReservation<'a, S> {
    engine: &'a EngineImpl<S>,
    slot: SlotId,
    transition: Option<CrossfadeSettings>,
    count: usize,
    active: bool,
}

impl<S> SlotLoadReservation<'_, S> {
    /// Publish the load and optional `FadeIn` together after preparation.
    pub(crate) fn send(
        mut self,
        item_id: TrackId,
        load: LoadGeneration,
        resource: Box<PlayerResource>,
    ) {
        let mut slots = self.engine.slots.lock();
        let Some(entry) = slots.entry_mut(self.slot) else {
            unreachable!("a slot with reserved commands was released");
        };
        let handle = &mut entry.control;
        let seek = resource.seek_handle();
        let render = resource.render_reader();
        if handle
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack { item_id, resource })
            .is_err()
        {
            unreachable!("reserved load command entry disappeared");
        }
        if let Some(settings) = self.transition
            && handle
                .cmd_tx
                .try_push(PlayerCmd::Transition(TrackTransition::FadeIn {
                    item_id,
                    settings,
                }))
                .is_err()
        {
            unreachable!("reserved FadeIn command entry disappeared");
        }
        if let Some(seek) = seek {
            handle.bind_seek(item_id, seek);
        }
        if let Some(render) = render {
            handle.bind_render(item_id, load, render);
        }
        entry.reserved_cmds -= self.count;
        self.active = false;
        drop(slots);
    }
}

impl<S> Drop for SlotLoadReservation<'_, S> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut slots = self.engine.slots.lock();
        if let Some(entry) = slots.entry_mut(self.slot) {
            entry.reserved_cmds -= self.count;
        }
    }
}

impl<S> EngineImpl<S> {
    /// Admit the seek before changing any reader. The slots lock owns the sole
    /// command producer, so a free entry cannot disappear before the push.
    pub(crate) fn send_slot_seek(
        &self,
        slot: SlotId,
        position: Duration,
        seconds: f64,
    ) -> Result<(), PlayError> {
        let mut slots = self.slots.lock();
        let entry = slots.entry_mut(slot).ok_or(PlayError::SlotNotFound(slot))?;
        if entry.closing {
            return Err(PlayError::SlotBusy { slot });
        }
        if entry.control.cmd_tx.vacant_len() <= entry.reserved_cmds {
            return Err(PlayError::SlotChannelFull { slot });
        }
        let handle = &mut entry.control;
        let seek_epoch = handle.playback.next_seek_epoch();
        handle.begin_seek(position);
        // The audio consumer only increases vacancy while this lock excludes
        // other producers, so this push cannot fail after the vacancy check.
        if handle
            .cmd_tx
            .try_push(PlayerCmd::Seek {
                seek_epoch,
                seconds,
            })
            .is_err()
        {
            unreachable!("reserved seek command entry disappeared");
        }
        drop(slots);
        Ok(())
    }

    /// Reserve command capacity before taking a resource out of the queue.
    /// The prepared resource can be moved into the audio thread without a
    /// second fallible send or an intervening producer stealing `FadeIn` space.
    pub(crate) fn reserve_slot_load(
        &self,
        slot: SlotId,
        transition: Option<CrossfadeSettings>,
    ) -> Result<SlotLoadReservation<'_, S>, PlayError> {
        let count = usize::from(transition.is_some()) + 1;
        let mut slots = self.slots.lock();
        let entry = slots.entry_mut(slot).ok_or(PlayError::SlotNotFound(slot))?;
        if entry.closing {
            return Err(PlayError::SlotBusy { slot });
        }
        if entry
            .control
            .cmd_tx
            .vacant_len()
            .saturating_sub(entry.reserved_cmds)
            < count
        {
            return Err(PlayError::SlotChannelFull { slot });
        }
        entry.reserved_cmds += count;
        drop(slots);
        Ok(SlotLoadReservation {
            engine: self,
            slot,
            transition,
            count,
            active: true,
        })
    }

    pub(crate) fn send_slot_cmd(&self, slot: SlotId, cmd: PlayerCmd) -> Result<(), PlayError> {
        if matches!(cmd, PlayerCmd::LoadTrack { .. }) {
            return Err(PlayError::Internal(
                "load requires command reservation".into(),
            ));
        }
        let mut slots = self.slots.lock();
        let result = match slots.entry_mut(slot) {
            Some(entry) => {
                if entry.closing {
                    Err(PlayError::SlotBusy { slot })
                } else if entry.control.cmd_tx.vacant_len() <= entry.reserved_cmds {
                    Err(PlayError::SlotChannelFull { slot })
                } else {
                    entry
                        .control
                        .cmd_tx
                        .try_push(cmd)
                        .map_err(|_| PlayError::SlotChannelFull { slot })
                }
            }
            None => Err(PlayError::SlotNotFound(slot)),
        };
        drop(slots);
        result
    }
}
