use std::{
    collections::{BTreeMap, HashMap},
    num::NonZeroU32,
    ops::Range,
    thread::ThreadId,
};

use kithara::{
    platform::time::Duration,
    signal::{AudioChunkInfo, AudioSpec, SourceSpan},
};
use kithara_test_utils::{
    test::usdt::{self, ProbeEvent},
    tracing,
};

use super::cases::CHANNELS;

#[derive(Clone, Copy, Debug, kithara_derive::Mirror)]
#[mirror(from = AudioChunkInfo)]
pub(super) struct SourceIdentity {
    pub(super) spec: AudioSpec,
    pub(super) source_revision: u64,
    pub(super) media_origin: Option<Duration>,
}

impl SourceIdentity {
    /// Whether `chunk` was decoded from this source identity.
    pub(super) fn describes(&self, chunk: &AudioChunkInfo) -> bool {
        chunk.source_revision == self.source_revision
            && chunk.spec == self.spec
            && chunk.media_origin == self.media_origin
    }
}

#[derive(Clone, Copy)]
pub(super) struct Interval {
    pub(super) identity: SourceIdentity,
    pub(super) source: SourceSpan,
}

pub(super) struct Capture {
    pub(super) sample_rate: u32,
    intervals: BTreeMap<i64, Interval>,
}

struct Render {
    item: u64,
    output: Range<i64>,
}

struct Pending {
    output: Range<i64>,
    revision: u64,
    source_start: u64,
    source_end: u64,
    identity: Option<(AudioSpec, u64, bool)>,
    origin: Option<Duration>,
    words: [u64; 24],
    next: usize,
}

#[derive(Default)]
struct Thread {
    render: Option<Render>,
    pending: Option<Pending>,
}

fn field(event: &ProbeEvent, name: &str) -> Result<u64, String> {
    event
        .field(name)
        .ok_or_else(|| format!("{} has no {name}", event.probe))
}

fn frame(event: &ProbeEvent, name: &str) -> Result<i64, String> {
    i64::try_from(field(event, name)?).map_err(|error| error.to_string())
}

impl Render {
    fn read(event: &ProbeEvent) -> Result<Self, String> {
        let base = field(event, "output_base")?;
        let output = if base == u64::MAX {
            0..0
        } else {
            let base = i64::try_from(base).map_err(|error| error.to_string())?;
            let start = base
                .checked_add(frame(event, "range_start")?)
                .ok_or_else(|| "render range overflows".to_owned())?;
            let end = base
                .checked_add(frame(event, "range_end")?)
                .ok_or_else(|| "render range overflows".to_owned())?;
            start..end
        };
        Ok(Self {
            item: field(event, "track_id")?,
            output,
        })
    }
}

impl Pending {
    fn read(event: &ProbeEvent, render: &Render) -> Result<Self, String> {
        let output = frame(event, "output_start")?..frame(event, "output_end")?;
        if output.start < render.output.start
            || output.end > render.output.end
            || output.end <= output.start
        {
            return Err("consumed PCM range lies outside its real render entry".to_owned());
        }
        Ok(Self {
            output,
            revision: field(event, "render_revision")?,
            source_start: field(event, "source_start")?,
            source_end: field(event, "source_end")?,
            identity: None,
            origin: None,
            words: [0; 24],
            next: 0,
        })
    }

    fn identity(&mut self, event: &ProbeEvent) -> Result<(), String> {
        if self.identity.is_some() || self.origin.is_some() || self.next != 0 {
            return Err("duplicate or misplaced consumed source identity".to_owned());
        }
        let rate = NonZeroU32::new(
            u32::try_from(field(event, "sample_rate")?).map_err(|error| error.to_string())?,
        )
        .ok_or_else(|| "consumed source has zero sample rate".to_owned())?;
        let channels =
            u16::try_from(field(event, "channels")?).map_err(|error| error.to_string())?;
        let present = field(event, "origin_present")?;
        if present > 1 {
            return Err("consumed source origin presence is malformed".to_owned());
        }
        self.identity = Some((
            AudioSpec::new(channels, rate),
            field(event, "source_revision")?,
            present == 1,
        ));
        Ok(())
    }

    fn origin(&mut self, event: &ProbeEvent) -> Result<(), String> {
        let (_, _, present) = self
            .identity
            .ok_or_else(|| "consumed origin has no preceding source identity".to_owned())?;
        if self.origin.is_some() || self.next != 0 {
            return Err("duplicate or misplaced consumed source origin".to_owned());
        }
        let nanos = u32::try_from(field(event, "nanos")?).map_err(|error| error.to_string())?;
        if nanos >= 1_000_000_000 {
            return Err("consumed source origin has invalid nanoseconds".to_owned());
        }
        let origin = Duration::new(field(event, "seconds")?, nanos);
        if !present && origin != Duration::ZERO {
            return Err("absent consumed origin has a payload".to_owned());
        }
        self.origin = Some(origin);
        Ok(())
    }

    fn geometry(&mut self, event: &ProbeEvent) -> Result<bool, String> {
        if self.identity.is_none() || self.origin.is_none() {
            return Err("consumed geometry lost its source identity/origin".to_owned());
        }
        let part = usize::try_from(field(event, "part")?).map_err(|error| error.to_string())?;
        if part != self.next || part >= 6 {
            return Err(format!(
                "missing, duplicate or reordered consumed geometry fragment {part}; expected {}",
                self.next
            ));
        }
        for (offset, name) in ["first", "second", "third", "fourth"]
            .into_iter()
            .enumerate()
        {
            self.words[part * 4 + offset] = field(event, name)?;
        }
        self.next += 1;
        Ok(self.next == 6)
    }
}

impl Capture {
    pub(super) fn for_track(item: u64, sample_rate: u32) -> Result<Self, String> {
        let events = usdt::events();
        let capture = Self::from_events(item, sample_rate, &events)?;
        let geometry_events = events
            .iter()
            .filter(|event| {
                event.target == "kithara_render_probe" && event.probe == "pcm_consumed_geometry"
            })
            .count();
        let consumed_slices = events
            .iter()
            .filter(|event| event.target == "kithara_render_probe" && event.probe == "pcm_consumed")
            .count();
        tracing::info!(
            item,
            history_events = events.len(),
            consumed_slices,
            geometry_events,
            selected_intervals = capture.intervals.len(),
            per_probe_capacity = usdt::MAX_EVENTS,
            "actual consumed geometry capture census"
        );
        Ok(capture)
    }

    fn from_events(item: u64, sample_rate: u32, events: &[ProbeEvent]) -> Result<Self, String> {
        let mut threads: HashMap<ThreadId, Thread> = HashMap::new();
        let mut capture = Self {
            sample_rate,
            intervals: BTreeMap::new(),
        };
        for event in events {
            if event.target != "kithara_render_probe" {
                continue;
            }
            let thread = threads.entry(event.thread).or_default();
            if event.probe == "render" {
                if thread.pending.is_some() {
                    return Err("new render interrupted consumed source geometry".to_owned());
                }
                thread.render = Some(Render::read(event)?);
                continue;
            }
            let Some(render) = thread.render.as_ref().filter(|render| render.item == item) else {
                continue;
            };
            if event.probe == "pcm_consumed" {
                if thread.pending.is_some() {
                    return Err("next PCM slice interrupted consumed source geometry".to_owned());
                }
                thread.pending = Some(Pending::read(event, render)?);
            } else if matches!(
                event.probe,
                "pcm_consumed_identity" | "pcm_consumed_origin" | "pcm_consumed_geometry"
            ) {
                let pending = thread
                    .pending
                    .as_mut()
                    .ok_or_else(|| "orphan or duplicate consumed source metadata".to_owned())?;
                let complete = match event.probe {
                    "pcm_consumed_identity" => {
                        pending.identity(event)?;
                        false
                    }
                    "pcm_consumed_origin" => {
                        pending.origin(event)?;
                        false
                    }
                    _ => pending.geometry(event)?,
                };
                if complete {
                    let pending = thread
                        .pending
                        .take()
                        .ok_or_else(|| "consumed geometry disappeared".to_owned())?;
                    capture.finish(&pending)?;
                }
            }
        }
        if threads.values().any(|thread| thread.pending.is_some()) {
            return Err("capture ends with missing consumed source geometry".to_owned());
        }
        Ok(capture)
    }

    fn finish(&mut self, pending: &Pending) -> Result<(), String> {
        let source = SourceSpan::try_from(pending.words)
            .map_err(|()| "consumed native geometry is malformed".to_owned())?;
        let (spec, source_revision, present) = pending
            .identity
            .ok_or_else(|| "consumed source identity disappeared".to_owned())?;
        let origin = pending
            .origin
            .ok_or_else(|| "consumed source origin disappeared".to_owned())?;
        if source.output_frames()
            != u64::try_from(pending.output.end - pending.output.start)
                .map_err(|error| error.to_string())?
            || spec.sample_rate.get() != self.sample_rate
            || source.sample_rate().get() != self.sample_rate
            || spec.channels != CHANNELS
            || !present
            || source.render_revision() != pending.revision
            || source.start() != pending.source_start
            || source.end() != pending.source_end
        {
            return Err(
                "consumed source lost output, revision, spec, origin or geometry custody"
                    .to_owned(),
            );
        }
        let identity = SourceIdentity {
            spec,
            source_revision,
            media_origin: Some(origin),
        };
        if self
            .intervals
            .insert(pending.output.start, Interval { identity, source })
            .is_some()
        {
            return Err("duplicate consumed output interval for one track".to_owned());
        }
        Ok(())
    }

    pub(super) fn command_boundary(
        &self,
        retained: Range<i64>,
        previous: u64,
        revision: u64,
        expected: i64,
    ) -> Result<(), String> {
        let issued = retained.start;
        let mut first_changed = None;
        for (start, interval) in self.covered(retained)? {
            let start = start.max(issued);
            let actual = interval.source.render_revision();
            if start < expected && actual != previous {
                return Err("consumed source law changed before the admitted lane frame".to_owned());
            }
            if actual == revision {
                first_changed = Some(first_changed.map_or(start, |first: i64| first.min(start)));
            }
        }
        if first_changed != Some(expected) {
            return Err(format!(
                "new source law first consumed at {first_changed:?}, expected {expected}"
            ));
        }
        Ok(())
    }

    pub(super) fn covered(&self, retained: Range<i64>) -> Result<BTreeMap<i64, Interval>, String> {
        let mut selected = BTreeMap::new();
        let mut cursor = retained.start;
        for (&start, &interval) in self.intervals.range(..retained.end) {
            let end = start
                .checked_add(
                    i64::try_from(interval.source.output_frames())
                        .map_err(|error| error.to_string())?,
                )
                .ok_or_else(|| "consumed output endpoint overflows".to_owned())?;
            if end <= retained.start {
                continue;
            }
            if start > cursor || (start < cursor && cursor != retained.start) {
                return Err(format!(
                    "consumed PCM coverage has a gap or overlap at {cursor}; next={start}..{end}"
                ));
            }
            cursor = end;
            selected.insert(start, interval);
        }
        if cursor < retained.end || selected.is_empty() {
            return Err(format!(
                "consumed PCM witness ends at {cursor}, before retained endpoint {}",
                retained.end
            ));
        }
        Ok(selected)
    }
}

pub(super) fn check_trace_negatives(
    item: u64,
    sample_rate: u32,
    retained: Range<i64>,
) -> Result<(), String> {
    let mut events = usdt::events();
    let mut owners = HashMap::new();
    let mut missing = None;
    for (index, event) in events.iter().enumerate() {
        if event.target != "kithara_render_probe" {
            continue;
        }
        if event.probe == "render" {
            owners.insert(event.thread, field(event, "track_id")?);
        } else if event.probe == "pcm_consumed_geometry" && owners.get(&event.thread) == Some(&item)
        {
            missing = Some(index);
            break;
        }
    }
    let missing =
        missing.ok_or_else(|| "trace negatives have no actual consumed geometry".to_owned())?;
    let removed = events.remove(missing);
    if Capture::from_events(item, sample_rate, &events)
        .and_then(|capture| capture.covered(retained.clone()))
        .is_ok()
    {
        return Err("physical oracle accepted an omitted native geometry fragment".to_owned());
    }
    events.insert(missing, removed);
    events.insert(missing, events[missing]);
    if Capture::from_events(item, sample_rate, &events)
        .and_then(|capture| capture.covered(retained.clone()))
        .is_ok()
    {
        return Err("physical oracle accepted a duplicate native geometry fragment".to_owned());
    }
    events.remove(missing);
    events.retain(|event| {
        event.target != "kithara_render_probe"
            || event.probe != "render"
            || event.field("track_id") != Some(item)
    });
    if Capture::from_events(item, sample_rate, &events)
        .and_then(|capture| capture.covered(retained))
        .is_ok()
    {
        return Err(
            "physical oracle accepted PCM without its real TrackId render owner".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn revision_capture(first_new: u64) -> Capture {
        let rate = NonZeroU32::new(48_000).expect("test sample rate");
        let identity = SourceIdentity {
            spec: AudioSpec::new(CHANNELS, rate),
            source_revision: 7,
            media_origin: Some(Duration::ZERO),
        };
        Capture {
            sample_rate: rate.get(),
            intervals: [(0, first_new, 1), (first_new, 130, 2)]
                .into_iter()
                .map(|(start, end, revision)| {
                    (
                        i64::try_from(start).expect("test frame"),
                        Interval {
                            identity,
                            source: SourceSpan::new(start, end, rate, end - start)
                                .expect("test source span")
                                .with_render_revision(revision),
                        },
                    )
                })
                .collect(),
        }
    }

    #[kithara::test(native)]
    #[case::early(126, false)]
    #[case::exact(127, true)]
    #[case::late(128, false)]
    fn command_revision_uses_the_consumed_frame_boundary(
        #[case] first_new: u64,
        #[case] accepted: bool,
    ) {
        let capture = revision_capture(first_new);
        let result = capture.command_boundary(125..129, 1, 2, 127);
        assert_eq!(result.is_ok(), accepted, "{result:?}");
        if first_new < 127 {
            assert!(
                result
                    .unwrap_err()
                    .contains("before the admitted lane frame")
            );
        } else if first_new > 127 {
            assert!(result.unwrap_err().contains("first consumed at Some(128)"));
        }
    }

    #[kithara::test(native)]
    fn command_boundary_requires_complete_native_coverage() {
        let mut capture = revision_capture(127);
        capture.intervals.remove(&0);
        assert!(capture.command_boundary(0..129, 1, 2, 127).is_err());
        let mut capture = revision_capture(127);
        let previous = capture.intervals.get_mut(&0).expect("previous interval");
        previous.source = SourceSpan::new(0, 128, previous.source.sample_rate(), 128)
            .expect("overlapping source span")
            .with_render_revision(1);
        assert!(capture.command_boundary(0..129, 1, 2, 127).is_err());
    }
}
