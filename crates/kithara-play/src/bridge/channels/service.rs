use super::*;

impl SlotControl {
    /// Begins and transfers scheduled track seeks.
    ///
    /// A seek on an audible track waits until its activation is within `lead`
    /// output frames of presentation: beginning it discards the decoder's old
    /// position, so the old stream must keep playing until the new epoch only
    /// has the response budget left to prepare. The presentation advance seen
    /// since the previous call is spent ahead of time, so a call cadence
    /// coarser than `lead` still begins the seek before the window closes. A
    /// track with no presented render snapshot is not audible and begins at
    /// once.
    pub(crate) fn service_scheduled_seeks(&mut self, lead: NonZeroUsize) {
        self.service_scheduled_seeks_for(lead, None);
    }

    pub(crate) fn service_scheduled_seek(&mut self, item_id: TrackId, lead: NonZeroUsize) {
        self.service_scheduled_seeks_for(lead, Some(item_id));
    }

    #[kithara::hang_watchdog]
    fn service_scheduled_seeks_for(&mut self, lead: NonZeroUsize, only: Option<TrackId>) {
        let lead = i64::try_from(lead.get()).unwrap_or(i64::MAX);
        let mut index = 0;
        while index < self.scheduled_seeks.len() {
            hang_reset!();
            let request = self.scheduled_seeks[index].clone();
            if only.is_some_and(|item_id| request.item_id != item_id) {
                index += 1;
                continue;
            }
            if request
                .cancel
                .as_ref()
                .is_some_and(CancelToken::is_cancelled)
            {
                self.scheduled_seeks.remove(index);
                continue;
            }
            if let ScheduledSeekDisposition::SeekOnly { activation } = request.disposition
                && matches!(request.state, ScheduledTrackSeekState::AwaitingStart)
                && let Some(snapshot) = self.render_snapshot_for(request.item_id, None)
            {
                let presented = snapshot.frontier().output();
                let advance = request.observed_output.map_or(0, |observed| {
                    (i64::from(presented) - i64::from(observed)).max(0)
                });
                self.scheduled_seeks[index].observed_output = Some(presented);
                if i64::from(activation) - i64::from(presented) - advance > lead {
                    index += 1;
                    continue;
                }
            }
            if request.disposition.is_prepared_launch()
                && !request.armed
                && matches!(request.state, ScheduledTrackSeekState::AwaitingStart)
            {
                index += 1;
                continue;
            }
            let seek_epoch = match request.state {
                ScheduledTrackSeekState::AwaitingStart => {
                    let Some(seek) = self.begin_track_seek(
                        request.item_id,
                        request.position,
                        request.disposition,
                    ) else {
                        if request.disposition.is_prepared_launch() {
                            index += 1;
                        } else {
                            self.scheduled_seeks.remove(index);
                        }
                        continue;
                    };
                    if !matches!(seek.outcome, kithara_audio::SeekOutcome::Landed { .. }) {
                        self.scheduled_seeks.remove(index);
                        continue;
                    }
                    self.scheduled_seeks[index].state = ScheduledTrackSeekState::AwaitingCommand {
                        seek_epoch: seek.epoch,
                    };
                    seek.epoch
                }
                ScheduledTrackSeekState::AwaitingCommand { seek_epoch } => seek_epoch,
            };
            let needs_arm = request.disposition.is_prepared_launch() && request.armed;
            if request
                .cancel
                .as_ref()
                .is_some_and(CancelToken::is_cancelled)
            {
                self.scheduled_seeks.remove(index);
                continue;
            }
            if needs_arm && self.cmd_tx.vacant_len() < 2
                || !needs_arm && self.cmd_tx.vacant_len() < 1
            {
                index += 1;
                continue;
            }
            if self
                .cmd_tx
                .try_push(PlayerCmd::ScheduleSeek {
                    item_id: request.item_id,
                    scheduled_epoch: request.scheduled_epoch,
                    seek_epoch,
                    disposition: request.disposition,
                    armed: false,
                })
                .is_ok()
            {
                if needs_arm
                    && self
                        .cmd_tx
                        .try_push(PlayerCmd::ArmPreparedLaunch {
                            item_id: request.item_id,
                            scheduled_epoch: request.scheduled_epoch,
                        })
                        .is_err()
                {
                    unreachable!("reserved command ring capacity must accept the arm command");
                }
                self.prepared_launch_epochs
                    .retain(|handoff| handoff.item_id != request.item_id);
                if let ScheduledSeekDisposition::PreparedLaunch(identity) = request.disposition {
                    self.prepared_launch_epochs.push(PreparedLaunchHandoff {
                        item_id: request.item_id,
                        scheduled_epoch: request.scheduled_epoch,
                        seek_epoch,
                        identity,
                        armed: request.armed,
                        cancel: request.cancel,
                    });
                }
                self.scheduled_seeks.remove(index);
            } else {
                index += 1;
            }
        }
    }
}
