use std::{
    io::{Read, Seek},
    ops::ControlFlow,
};

use kithara_bufpool::{HasPool, PoolRegion};

use crate::{
    DecodeResult, GaplessInfo,
    mp4::{Mp4Event, Mp4MediaTiming, TrackEdit, scan_mp4},
    traits::DecoderInput,
};

/// Stream the MP4 boxes in `reader` and derive a [`GaplessInfo`] if any
/// recognised source is present. Returns `Ok(None)` when the file is well-formed
/// but carries no gapless metadata.
///
/// # Errors
///
/// Returns the wrapping [`crate::DecodeError`] only if reading the source
/// itself fails. Malformed MP4 data is treated as "no gapless info" rather
/// than propagated, so callers can fall back to other probes.
pub fn probe_mp4_gapless<R, S>(
    reader: &mut R,
    pools: &PoolRegion<S>,
) -> DecodeResult<Option<GaplessInfo>>
where
    R: Read + Seek + Send + Sync,
    S: HasPool<u8>,
{
    probe_mp4_gapless_dyn(reader, pools)
}

pub(crate) fn probe_mp4_gapless_dyn<S>(
    reader: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> DecodeResult<Option<GaplessInfo>>
where
    S: HasPool<u8>,
{
    let mut probe = GaplessProbe::default();
    match scan_mp4(reader, &mut |event| probe.visit(event), pools) {
        Ok(()) => Ok(probe.into()),
        Err(crate::mp4::Mp4MetadataError::Io(error)) => Err(error.into()),
        Err(crate::mp4::Mp4MetadataError::Pool(error)) => Err(error.into()),
        Err(crate::mp4::Mp4MetadataError::InvalidData(_)) => Ok(None),
    }
}

/// Streaming visitor that picks the first source of gapless info it can prove.
///
/// Priority follows the established contract:
/// 1. A track-level edit list (`elst`) that yields a positive leading/trailing
///    pair — emitted at track end. Once seen, the scan is told to stop.
/// 2. An iTunSMPB freeform tag — emitted when the tag is read. Used only as a
///    fallback because traks come before `udta` in well-formed `moov` boxes,
///    and we don't want the iTunes tag to override a real `elst`.
#[derive(Default)]
struct GaplessProbe {
    current: Option<TrackState>,
    elst_derived: Option<GaplessInfo>,
    itunsmpb: Option<GaplessInfo>,
    movie_timescale: Option<u32>,
}

#[derive(Default)]
struct TrackState {
    edit: TrackEdit,
    media: Option<Mp4MediaTiming>,
    sample_rate: Option<u32>,
}

impl From<GaplessProbe> for Option<GaplessInfo> {
    fn from(probe: GaplessProbe) -> Self {
        probe.elst_derived.or(probe.itunsmpb)
    }
}

impl GaplessProbe {
    fn visit(&mut self, event: Mp4Event<'_>) -> ControlFlow<()> {
        match event {
            Mp4Event::ItunSmpb(info) => {
                if info.leading_frames == 0 && info.trailing_frames == 0 {
                    return ControlFlow::Continue(());
                }
                self.itunsmpb = Some(GaplessInfo {
                    leading_frames: info.leading_frames,
                    trailing_frames: info.trailing_frames,
                });
                return ControlFlow::Break(());
            }
            Mp4Event::MovieTimescale(timescale) => self.movie_timescale = Some(timescale),
            Mp4Event::TrackBegin => self.current = Some(TrackState::default()),
            Mp4Event::TrackEditList(entries) => {
                if let Some(track) = &mut self.current {
                    track.edit = TrackEdit::from(entries);
                }
            }
            Mp4Event::TrackEnd => return self.finish_track(),
            Mp4Event::TrackMediaTiming(timing) => {
                if let Some(track) = &mut self.current {
                    track.media = Some(timing);
                }
            }
            Mp4Event::TrackSampleRate(sample_rate) => {
                if let Some(track) = &mut self.current {
                    track.sample_rate = Some(sample_rate);
                }
            }
            Mp4Event::TrackCodec(_) | Mp4Event::TrackId(_) => {}
        }
        ControlFlow::Continue(())
    }

    fn finish_track(&mut self) -> ControlFlow<()> {
        if let Some(track) = self.current.take()
            && let Some(info) = derive_from_track(&track, self.movie_timescale)
        {
            self.elst_derived = Some(info);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

fn derive_from_track(track: &TrackState, movie_timescale: Option<u32>) -> Option<GaplessInfo> {
    track
        .edit
        .gapless(track.media?, track.sample_rate?, movie_timescale)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_test_utils::kithara;

    use super::probe_mp4_gapless;
    use crate::{
        GaplessInfo,
        mp4::{Mp4EditListEntry, Mp4MediaTiming},
        test_pools::pools,
    };

    fn atom(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = u32::try_from(payload.len() + 8).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(payload);
        out
    }

    fn full_box(kind: [u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, 0];
        payload.extend_from_slice(body);
        atom(kind, &payload)
    }

    fn mvhd(movie_timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&movie_timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        full_box(*b"mvhd", 0, &body)
    }

    fn mdhd(media_timescale: u32, media_duration: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&media_timescale.to_be_bytes());
        body.extend_from_slice(&media_duration.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        full_box(*b"mdhd", 0, &body)
    }

    fn elst_v0(segment_duration: u32, media_time: i32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&segment_duration.to_be_bytes());
        body.extend_from_slice(&media_time.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        full_box(*b"elst", 0, &body)
    }

    fn audio_sample_entry(codec: [u8; 4], sample_rate: u32) -> Vec<u8> {
        let mut entry = vec![0; 6];
        entry.extend_from_slice(&1u16.to_be_bytes());
        entry.extend_from_slice(&[0; 8]);
        entry.extend_from_slice(&2u16.to_be_bytes());
        entry.extend_from_slice(&16u16.to_be_bytes());
        entry.extend_from_slice(&0u16.to_be_bytes());
        entry.extend_from_slice(&0u16.to_be_bytes());
        entry.extend_from_slice(&(sample_rate << 16).to_be_bytes());
        atom(codec, &entry)
    }

    fn stsd(sample_rate: u32) -> Vec<u8> {
        let entry = audio_sample_entry(*b"mp4a", sample_rate);
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&entry);
        full_box(*b"stsd", 0, &body)
    }

    fn data_box(data_type: u32, value: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&data_type.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(value);
        atom(*b"data", &body)
    }

    fn freeform_text_box(kind: [u8; 4], text: &str) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 1];
        body.extend_from_slice(text.as_bytes());
        atom(kind, &body)
    }

    fn freeform_itunsmpb(text: &str) -> Vec<u8> {
        let mut freeform = Vec::new();
        freeform.extend_from_slice(&freeform_text_box(*b"mean", "com.apple.iTunes"));
        freeform.extend_from_slice(&freeform_text_box(*b"name", "iTunSMPB"));
        freeform.extend_from_slice(&data_box(1, text.as_bytes()));
        atom(*b"----", &freeform)
    }

    fn track_with_elst(sample_rate: u32, media: Mp4MediaTiming, edit: Mp4EditListEntry) -> Vec<u8> {
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&stsd(sample_rate));

        let minf = atom(*b"minf", &atom(*b"stbl", &stbl));

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd(
            media.timescale,
            u32::try_from(media.duration).expect("fixture duration"),
        ));
        mdia.extend_from_slice(&minf);

        let edts = atom(
            *b"edts",
            &elst_v0(
                u32::try_from(edit.segment_duration).expect("fixture duration"),
                i32::try_from(edit.media_time).expect("fixture media time"),
            ),
        );

        let mut trak = Vec::new();
        trak.extend_from_slice(&atom(*b"mdia", &mdia));
        trak.extend_from_slice(&edts);
        atom(*b"trak", &trak)
    }

    #[kithara::test]
    #[case::bounded_edit(96_000, 1_916, 1_920)]
    #[case::open_ended_edit_of_a_known_track(96_000, 0, 0)]
    #[case::open_ended_edit_of_a_fragmented_track(0, 0, 0)]
    fn derives_gapless_from_edit_list(
        #[case] media_duration: u64,
        #[case] segment_duration: u64,
        #[case] trailing_frames: u64,
    ) {
        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&track_with_elst(
            48_000,
            Mp4MediaTiming {
                timescale: 48_000,
                duration: media_duration,
            },
            Mp4EditListEntry {
                segment_duration,
                media_time: 2_112,

                media_rate: 1 << 16,
            },
        ));

        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            Some(GaplessInfo {
                leading_frames: 2_112,
                trailing_frames,
            })
        );
    }

    #[kithara::test]
    fn derives_gapless_from_itunsmpb_when_elst_missing() {
        let ilst = atom(
            *b"ilst",
            &freeform_itunsmpb(" 00000000 00000840 00000048 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            Some(GaplessInfo {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            })
        );
    }

    #[kithara::test]
    fn elst_takes_priority_over_itunsmpb() {
        let ilst = atom(
            *b"ilst",
            &freeform_itunsmpb(" 00000000 00000010 00000020 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&track_with_elst(
            48_000,
            Mp4MediaTiming {
                timescale: 48_000,
                duration: 96_000,
            },
            Mp4EditListEntry {
                segment_duration: 1_916,
                media_time: 2_112,

                media_rate: 1 << 16,
            },
        ));
        moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            Some(GaplessInfo {
                leading_frames: 2_112,
                trailing_frames: 1_920,
            })
        );
    }

    #[kithara::test]
    fn returns_none_without_gapless_metadata() {
        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            None
        );
    }

    #[kithara::test]
    fn returns_none_for_zero_padding_itunsmpb() {
        let ilst = atom(
            *b"ilst",
            &freeform_itunsmpb(" 00000000 00000000 00000000 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            None
        );
    }
}
