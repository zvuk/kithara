use std::ops::ControlFlow;

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::time::Duration;

use super::{Mp4Event, Mp4MediaTiming, TrackEdit, scan_mp4};
use crate::{DecodeResult, DecoderInput, GaplessInfo};

pub(crate) struct Presentation {
    pub(crate) track_id: u32,
    pub(crate) media_origin: Option<Duration>,
    pub(crate) media_end: Option<Duration>,
    pub(crate) gapless: Option<GaplessInfo>,
    media: Mp4MediaTiming,
    sample_rate: u32,
}

#[derive(Default)]
struct Track {
    id: Option<u32>,
    media: Option<Mp4MediaTiming>,
    sample_rate: Option<u32>,
    edit: TrackEdit,
}

impl Track {
    fn presentation(self, movie_timescale: Option<u32>) -> Option<Presentation> {
        let track_id = self.id?;
        let media = self.media?;
        let sample_rate = self.sample_rate?;
        let gapless = self.edit.gapless(media, sample_rate, movie_timescale);
        let (media_origin, media_end) = match self.edit {
            TrackEdit::Identity => (
                Some(Duration::ZERO),
                duration(media.duration, media.timescale).filter(|end| !end.is_zero()),
            ),
            TrackEdit::Offset {
                media_time,
                segment,
            } => (
                duration(media_time, media.timescale),
                segment
                    .zip(movie_timescale)
                    .and_then(|(segment, scale)| duration(segment.get(), scale)),
            ),
            TrackEdit::Unsupported => (None, None),
        };
        Some(Presentation {
            track_id,
            media_origin,
            media_end,
            gapless,
            media,
            sample_rate,
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
                Mp4Event::ItunSmpb(info) => itunsmpb = Some(info),
                Mp4Event::TrackEditList(edits) => {
                    if let Some(track) = &mut track {
                        track.edit = TrackEdit::from(edits);
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
    if let [presentation] = result.as_mut_slice()
        && presentation.gapless.is_none()
    {
        presentation.gapless =
            itunsmpb.and_then(|info| info.gapless(presentation.media, presentation.sample_rate));
    }
    Ok(result)
}

fn duration(ticks: u64, timescale: u32) -> Option<Duration> {
    let nanos = u128::from(ticks)
        .checked_mul(1_000_000_000)?
        .checked_div(u128::from(timescale))?;
    Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
}
