use super::*;

impl SlotControl {
    pub(crate) fn bind_render(&mut self, item_id: TrackId, reader: RenderReader) {
        self.render.0.push((item_id, reader));
        kithara_test_macros::probe_event!(
            slot_render_reader_bound,
            item = item_id.as_u64(),
            binding_index =
                u64::try_from(self.render.0.len().saturating_sub(1)).unwrap_or(u64::MAX)
        );
    }

    /// Withdraws every prepared launch of `item_id` and tells the renderer.
    ///
    /// `SlotControl` is reached only while the engine holds its slots mutex,
    /// which also serializes every producer for this command ring, so the
    /// carrier is reserved before the replacement epoch begins: a decoder
    /// promise never escapes without its RT command.
    pub(crate) fn cancel_prepared_launches(&mut self, item_id: TrackId, resume: bool) -> bool {
        let Some(PreparedLaunchHandoff {
            scheduled_epoch,
            seek_epoch,
            cancel,
            ..
        }) = self
            .prepared_launch_epochs
            .iter()
            .find(|handoff| handoff.item_id == item_id)
            .cloned()
        else {
            for seek in &self.scheduled_seeks {
                if seek.item_id == item_id
                    && seek.disposition.is_prepared_launch()
                    && let Some(cancel) = &seek.cancel
                {
                    cancel.cancel();
                }
            }
            self.scheduled_seeks
                .retain(|seek| seek.item_id != item_id || !seek.disposition.is_prepared_launch());
            return true;
        };
        if self.cmd_tx.vacant_len() == 0 {
            return false;
        }
        let Some((_, handle)) = self
            .seek
            .0
            .iter()
            .find(|(bound_id, _)| *bound_id == item_id)
        else {
            return false;
        };
        let target_seconds = self.playback.position.load(Ordering::Relaxed).max(0.0);
        let target = Duration::from_secs_f64(target_seconds);
        let transport_seek_epoch = self.playback.next_seek_epoch();
        let replacement = handle.begin_prepared(target);
        let command = PlayerCmd::CancelPreparedLaunch {
            item_id,
            scheduled_epoch,
            prepared_seek_epoch: seek_epoch,
            replacement_seek_epoch: replacement.epoch,
            transport_seek_epoch,
            target,
            resume,
        };
        if self.cmd_tx.try_push(command).is_err() {
            unreachable!(
                "the slots mutex serializes the only command-ring writer after capacity reservation"
            );
        }
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.prepared_launch_epochs
            .retain(|handoff| handoff.item_id != item_id || handoff.seek_epoch != seek_epoch);
        self.scheduled_seeks
            .retain(|seek| seek.item_id != item_id || !seek.disposition.is_prepared_launch());
        true
    }

    /// Record the control half of a track's seek path.
    pub fn bind_seek(&mut self, item_id: TrackId, handle: Arc<dyn SeekBegin>) {
        self.seek.0.push((item_id, handle));
    }

    pub(crate) fn latest_render_snapshot(&self) -> Option<RenderSnapshot> {
        self.render
            .0
            .iter()
            .filter_map(|(_, reader)| reader.load())
            .max_by_key(|snapshot| {
                let context = snapshot.context();
                (
                    u64::from(context.session_epoch()),
                    i64::from(context.output_frames().end),
                )
            })
    }

    pub(crate) fn render_snapshot_for(
        &self,
        item_id: TrackId,
        warp_map: Option<WarpMapRevision>,
    ) -> Option<RenderSnapshot> {
        let selected = self
            .render
            .0
            .iter()
            .enumerate()
            .filter_map(|(binding_index, binding)| {
                Self::matching_render_snapshot(binding_index, binding, item_id, warp_map)
            })
            .max_by_key(render_snapshot_key);
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_selected,
            target_item = item_id.as_u64(),
            expected_warp_map = warp_map.map_or(0, u64::from),
            selected = if selected.is_some() { 1_u64 } else { 0 },
            selected_warp_map = selected
                .as_ref()
                .and_then(|snapshot| snapshot.frontier().warp_map())
                .map_or(0, u64::from),
            selected_output = selected
                .as_ref()
                .map_or(0, |snapshot| i64::from(snapshot.frontier().output()))
        );
        selected
    }

    fn matching_render_snapshot(
        binding_index: usize,
        binding: &RenderBinding,
        item_id: TrackId,
        warp_map: Option<WarpMapRevision>,
    ) -> Option<RenderSnapshot> {
        let (bound_item, reader) = binding;
        let snapshot = reader.load();
        let snapshot_map = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.frontier().warp_map());
        let matches_item = *bound_item == item_id;
        let matches_map = warp_map.is_none_or(|expected| snapshot_map == Some(expected));
        let binding_index = u64::try_from(binding_index).unwrap_or(u64::MAX);
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_binding,
            target_item = item_id.as_u64(),
            expected_warp_map = warp_map.map_or(0, u64::from),
            bound_item = bound_item.as_u64(),
            binding_index,
            matches_item = u64::from(matches_item)
        );
        kithara_test_macros::probe_event!(
            targeted_render_snapshot_state,
            snapshot_present = u64::from(snapshot.is_some()),
            snapshot_warp_map = snapshot_map.map_or(0, u64::from),
            snapshot_output = snapshot
                .as_ref()
                .map_or(0, |value| i64::from(value.frontier().output())),
            snapshot_epoch = snapshot
                .as_ref()
                .map_or(0, |value| u64::from(value.context().session_epoch())),
            matches_map = u64::from(matches_map)
        );
        matches_item
            .then_some(snapshot)
            .flatten()
            .filter(|_| matches_map)
    }

    pub(crate) fn unbind_render(&mut self, item_id: TrackId, reader: &RenderReader) {
        self.render
            .0
            .retain(|(bound_id, bound_reader)| *bound_id != item_id || bound_reader != reader);
        kithara_test_macros::probe_event!(slot_render_reader_unbound, item = item_id.as_u64());
    }

    /// Forget the exact resource generation returned by the processor.
    pub fn unbind_seek(&mut self, item_id: TrackId, handle: &Arc<dyn SeekBegin>) {
        self.seek.0.retain(|(bound_id, bound_handle)| {
            *bound_id != item_id || !Arc::ptr_eq(bound_handle, handle)
        });
        self.cancel_scheduled_seek(item_id);
    }
}
