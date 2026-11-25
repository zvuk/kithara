use std::{num::NonZeroU32, ops::ControlFlow};

use kithara_bufpool::{HasPool, PoolRegion};

use crate::{
    DecodeResult, GaplessInfo,
    gapless::DeclaredGapless,
    mp4::{ItunSmpb, Mp4Event, Mp4MediaTiming, TrackEdit, scan_mp4},
    traits::DecoderInput,
};

pub(crate) fn probe_mp4_gapless_dyn<S>(
    reader: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> DecodeResult<Option<DeclaredGapless>>
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
/// 2. An iTunSMPB freeform tag in the first audio track's media timescale.
///    It never overrides that track's `elst`.
#[derive(Default)]
struct GaplessProbe {
    current: Option<TrackState>,
    /// Media timing of the first audio track.
    first_track: Option<Mp4MediaTiming>,
    elst_derived: Option<DeclaredGapless>,
    itunsmpb: Option<ItunSmpb>,
    movie_timescale: Option<u32>,
}

#[derive(Default)]
struct TrackState {
    kind: Option<re_mp4::TrackKind>,
    edit: TrackEdit,
    media: Option<Mp4MediaTiming>,
    sample_rate: Option<u32>,
}

impl From<GaplessProbe> for Option<DeclaredGapless> {
    fn from(probe: GaplessProbe) -> Self {
        probe.elst_derived.or_else(|| {
            let media = probe.first_track?;
            declared(probe.itunsmpb?.gapless()?, media.timescale)
        })
    }
}

impl GaplessProbe {
    fn visit(&mut self, event: Mp4Event<'_>) -> ControlFlow<()> {
        match event {
            Mp4Event::ItunSmpb(info) => self.itunsmpb = Some(info),
            Mp4Event::MovieTimescale(timescale) => self.movie_timescale = Some(timescale),
            Mp4Event::TrackBegin => self.current = Some(TrackState::default()),
            Mp4Event::TrackKind(kind) => {
                if let Some(track) = &mut self.current {
                    track.kind = Some(kind);
                }
            }
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
        let Some(track) = self.current.take() else {
            return ControlFlow::Continue(());
        };
        if track.kind != Some(re_mp4::TrackKind::Audio) || self.first_track.is_some() {
            return ControlFlow::Continue(());
        }
        self.first_track = track.media;
        if let Some(info) = derive_from_track(&track, self.movie_timescale) {
            self.elst_derived = Some(info);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

fn derive_from_track(track: &TrackState, movie_timescale: Option<u32>) -> Option<DeclaredGapless> {
    let sample_rate = track.sample_rate?;
    declared(
        track
            .edit
            .gapless(track.media?, sample_rate, movie_timescale)?,
        sample_rate,
    )
}

fn declared(info: GaplessInfo, sample_rate: u32) -> Option<DeclaredGapless> {
    Some(DeclaredGapless {
        info,
        rate: NonZeroU32::new(sample_rate)?,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_bufpool::{HasPool, PoolRegion};
    use kithara_test_utils::kithara;

    use super::probe_mp4_gapless_dyn;
    use crate::{
        DecodeResult, DecoderInput, GaplessInfo,
        mp4::{Mp4EditListEntry, Mp4MediaTiming},
        test_pools::pools,
    };

    fn probe_mp4_gapless<R: DecoderInput, S: HasPool<u8>>(
        reader: &mut R,
        pools: &PoolRegion<S>,
    ) -> DecodeResult<Option<GaplessInfo>> {
        Ok(probe_mp4_gapless_dyn(reader, pools)?.map(|declared| declared.info))
    }

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
        track(sample_rate, media, Some(edit))
    }

    fn track(sample_rate: u32, media: Mp4MediaTiming, edit: Option<Mp4EditListEntry>) -> Vec<u8> {
        track_with_stsd(&stsd(sample_rate), media, edit, re_mp4::TrackKind::Audio)
    }

    fn track_with_stsd(
        description: &[u8],
        media: Mp4MediaTiming,
        edit: Option<Mp4EditListEntry>,
        kind: re_mp4::TrackKind,
    ) -> Vec<u8> {
        let mut stbl = Vec::new();
        stbl.extend_from_slice(description);

        let minf = atom(*b"minf", &atom(*b"stbl", &stbl));

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd(
            media.timescale,
            u32::try_from(media.duration).expect("fixture duration"),
        ));
        let mut handler = vec![0; 4];
        handler.extend_from_slice(&u32::from(re_mp4::FourCC::from(kind)).to_be_bytes());
        handler.extend_from_slice(&[0; 13]);
        mdia.extend(full_box(*b"hdlr", 0, &handler));
        mdia.extend_from_slice(&minf);

        let mut trak = Vec::new();
        trak.extend_from_slice(&atom(*b"mdia", &mdia));
        if let Some(edit) = edit {
            trak.extend_from_slice(&atom(
                *b"edts",
                &elst_v0(
                    u32::try_from(edit.segment_duration).expect("fixture duration"),
                    i32::try_from(edit.media_time).expect("fixture media time"),
                ),
            ));
        }
        atom(*b"trak", &trak)
    }

    #[kithara::test]
    #[case::audio_only(false, false)]
    #[case::video_first(true, false)]
    #[case::another_audio_edit(false, true)]
    fn itunsmpb_uses_the_audio_track_when_video_precedes_it(
        #[case] video_first: bool,
        #[case] another_audio_edit: bool,
    ) {
        let mut moov = mvhd(1_000);
        if video_first {
            let mut visual = vec![0; 78];
            visual[6..8].copy_from_slice(&1_u16.to_be_bytes());
            visual[24..26].copy_from_slice(&1920_u16.to_be_bytes());
            visual[26..28].copy_from_slice(&1080_u16.to_be_bytes());
            visual[28..32].copy_from_slice(&(72_u32 << 16).to_be_bytes());
            visual[32..36].copy_from_slice(&(72_u32 << 16).to_be_bytes());
            let mut description = 1_u32.to_be_bytes().to_vec();
            description.extend(atom(*b"avc1", &visual));
            moov.extend(track_with_stsd(
                &full_box(*b"stsd", 0, &description),
                Mp4MediaTiming {
                    timescale: 90_000,
                    duration: 0,
                },
                None,
                re_mp4::TrackKind::Video,
            ));
        }
        moov.extend(track(
            48_000,
            Mp4MediaTiming {
                timescale: 48_000,
                duration: 0,
            },
            None,
        ));
        if another_audio_edit {
            moov.extend(track_with_elst(
                32_000,
                Mp4MediaTiming {
                    timescale: 32_000,
                    duration: 0,
                },
                Mp4EditListEntry {
                    media_time: 3200,
                    segment_duration: 0,
                    media_rate: 1 << 16,
                },
            ));
        }
        let ilst = atom(
            *b"ilst",
            &freeform_itunsmpb(" 00000000 00000840 00000048 0000000000000000"),
        );
        let mut metadata = vec![0; 4];
        metadata.extend(ilst);
        moov.extend(atom(*b"udta", &atom(*b"meta", &metadata)));
        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("audio tag probe"),
            Some(GaplessInfo::new(2112, 72)),
        );
        let declared = probe_mp4_gapless_dyn(&mut reader, &pools())
            .expect("audio declaration probe")
            .expect("audio declaration");
        assert_eq!(declared.rate.get(), 48_000);
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

    /// Tag counts use the track's `mdhd` media timescale, including when the
    /// sample entry names a different PCM rate.
    #[kithara::test]
    #[case::media_timescale_is_the_sample_rate(48_000, 48_000)]
    #[case::media_timescale_is_the_core_rate(48_000, 24_000)]
    #[case::sample_entry_is_the_core_rate(22_050, 44_100)]
    fn derives_gapless_from_itunsmpb_when_elst_missing(
        #[case] sample_rate: u32,
        #[case] media_timescale: u32,
    ) {
        let ilst = atom(
            *b"ilst",
            &freeform_itunsmpb(" 00000000 00000840 00000048 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&track(
            sample_rate,
            Mp4MediaTiming {
                timescale: media_timescale,
                duration: 0,
            },
            None,
        ));
        moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

        let mut reader = Cursor::new(atom(*b"moov", &moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader, &pools()).expect("BUG: probe"),
            Some(GaplessInfo::new(2112, 72)),
        );
        let declared = probe_mp4_gapless_dyn(&mut reader, &pools())
            .expect("audio declaration probe")
            .expect("audio declaration");
        assert_eq!(declared.rate.get(), media_timescale);
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
