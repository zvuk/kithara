use std::ops::ControlFlow;

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::time::Duration;

use super::{Mp4EditListEntry, Mp4Event, Mp4MediaTiming, scan_mp4};
use crate::{DecodeResult, DecoderInput, GaplessInfo, gapless::derive_from_edit};

pub(crate) struct Presentation {
    pub(crate) track_id: u32,
    pub(crate) media_origin: Option<Duration>,
    pub(crate) media_end: Option<Duration>,
    pub(crate) gapless: Option<GaplessInfo>,
}

#[derive(Clone, Copy, Default)]
enum Edit {
    #[default]
    Identity,
    Offset(Mp4EditListEntry),
    Unsupported,
}

#[derive(Default)]
struct Track {
    id: Option<u32>,
    media: Option<Mp4MediaTiming>,
    sample_rate: Option<u32>,
    edit: Edit,
}

impl Track {
    fn presentation(self, movie_timescale: Option<u32>) -> Option<Presentation> {
        let track_id = self.id?;
        let media = self.media?;
        let sample_rate = self.sample_rate?;
        let gapless = match self.edit {
            Edit::Offset(edit) => {
                movie_timescale.and_then(|scale| derive_from_edit(edit, media, sample_rate, scale))
            }
            Edit::Identity | Edit::Unsupported => None,
        };
        let (media_origin, media_end) = match self.edit {
            Edit::Identity => (
                Some(Duration::ZERO),
                duration(media.duration, media.timescale).filter(|end| !end.is_zero()),
            ),
            Edit::Offset(edit) => (
                u64::try_from(edit.media_time)
                    .ok()
                    .and_then(|time| duration(time, media.timescale)),
                movie_timescale
                    .and_then(|scale| duration(edit.segment_duration, scale))
                    .filter(|end| !end.is_zero()),
            ),
            Edit::Unsupported => (None, None),
        };
        Some(Presentation {
            track_id,
            media_origin,
            media_end,
            gapless,
        })
    }
}

pub(crate) fn probe_presentations<S>(
    reader: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> DecodeResult<Vec<Presentation>>
where
    S: HasPool<u8>,
{
    let mut movie_timescale = None;
    let mut track = None;
    let mut result: Vec<Presentation> = Vec::new();
    let mut itunsmpb = None;
    scan_mp4(
        reader,
        &mut |event| {
            match event {
                Mp4Event::MovieTimescale(scale) => movie_timescale = Some(scale),
                Mp4Event::TrackBegin => track = Some(Track::default()),
                Mp4Event::TrackId(id) => {
                    if let Some(track) = &mut track {
                        track.id = Some(id);
                    }
                }
                Mp4Event::TrackMediaTiming(media) => {
                    if let Some(track) = &mut track {
                        track.media = Some(media);
                    }
                }
                Mp4Event::TrackSampleRate(rate) => {
                    if let Some(track) = &mut track {
                        track.sample_rate = Some(rate);
                    }
                }
                Mp4Event::ItunSmpb(info) => {
                    if info.leading_frames != 0 || info.trailing_frames != 0 {
                        itunsmpb = Some(GaplessInfo {
                            leading_frames: info.leading_frames,
                            trailing_frames: info.trailing_frames,
                        });
                    }
                }
                Mp4Event::TrackEditList(edits) => {
                    if let Some(track) = &mut track {
                        track.edit = match edits {
                            [] => Edit::Identity,
                            [edit] if edit.media_time >= 0 && edit.media_rate == 1 << 16 => {
                                Edit::Offset(*edit)
                            }
                            _ => Edit::Unsupported,
                        };
                    }
                }
                Mp4Event::TrackEnd => {
                    if let Some(track) = track.take()
                        && let Some(presentation) = track.presentation(movie_timescale)
                    {
                        result.push(presentation);
                    }
                }
                Mp4Event::TrackCodec(_) => {}
            }
            ControlFlow::Continue(())
        },
        pools,
    )
    .map_err(|error| crate::DecodeError::parse("mp4 presentation", error))?;
    if let [presentation] = result.as_mut_slice() {
        presentation.gapless = presentation.gapless.or(itunsmpb);
    }
    Ok(result)
}

fn duration(ticks: u64, timescale: u32) -> Option<Duration> {
    let nanos = u128::from(ticks)
        .checked_mul(1_000_000_000)?
        .checked_div(u128::from(timescale))?;
    Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
}
