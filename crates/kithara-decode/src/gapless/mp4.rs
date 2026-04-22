use std::{
    io::{Read, Seek},
    ops::ControlFlow,
};

use crate::{
    DecodeResult, GaplessInfo,
    mp4::{ItunSmpb, Mp4EditListEntry, Mp4MediaTiming, Mp4Visitor, scan_mp4},
};

/// Stream the MP4 boxes in `reader` and derive a [`GaplessInfo`] if any
/// recognised source is present. Returns `Ok(None)` when the file is well
/// formed but carries no gapless metadata.
///
/// # Errors
///
/// Returns the wrapping [`crate::DecodeError`] only if reading the source
/// itself fails. Malformed MP4 data is treated as "no gapless info" rather
/// than propagated, so callers can fall back to other probes.
pub fn probe_mp4_gapless<R: Read + Seek + Send + Sync>(
    reader: &mut R,
) -> DecodeResult<Option<GaplessInfo>> {
    let mut probe = GaplessProbe::default();
    match scan_mp4(reader, &mut probe) {
        Ok(()) => Ok(probe.into_info()),
        Err(crate::mp4::Mp4MetadataError::Io(error)) => Err(error.into()),
        Err(crate::mp4::Mp4MetadataError::InvalidData(_)) => Ok(None),
    }
}

/// Streaming visitor that picks the first source of gapless info it can prove.
///
/// Priority follows the established contract:
/// 1. A track-level edit list (`elst`) that yields a positive leading/trailing
///    pair — emitted at `on_track_end`. Once seen, the scan is told to stop.
/// 2. An iTunSMPB freeform tag — emitted at `on_itunsmpb`. Used only as a
///    fallback because traks come before `udta` in well-formed `moov` boxes,
///    and we don't want the iTunes tag to override a real `elst`.
#[derive(Default)]
struct GaplessProbe {
    movie_timescale: Option<u32>,
    current: Option<TrackState>,
    elst_derived: Option<GaplessInfo>,
    itunsmpb: Option<GaplessInfo>,
}

#[derive(Default)]
struct TrackState {
    sample_rate: Option<u32>,
    media: Option<Mp4MediaTiming>,
    first_edit: Option<Mp4EditListEntry>,
}

impl GaplessProbe {
    fn into_info(self) -> Option<GaplessInfo> {
        self.elst_derived.or(self.itunsmpb)
    }
}

impl Mp4Visitor for GaplessProbe {
    fn on_movie_timescale(&mut self, timescale: u32) -> ControlFlow<()> {
        self.movie_timescale = Some(timescale);
        ControlFlow::Continue(())
    }

    fn on_track_begin(&mut self) -> ControlFlow<()> {
        self.current = Some(TrackState::default());
        ControlFlow::Continue(())
    }

    fn on_track_end(&mut self) -> ControlFlow<()> {
        let Some(track) = self.current.take() else {
            return ControlFlow::Continue(());
        };
        if let Some(info) = derive_from_track(&track, self.movie_timescale) {
            self.elst_derived = Some(info);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn on_track_sample_rate(&mut self, sample_rate: u32) -> ControlFlow<()> {
        if let Some(track) = &mut self.current {
            track.sample_rate = Some(sample_rate);
        }
        ControlFlow::Continue(())
    }

    fn on_track_media_timing(&mut self, timing: Mp4MediaTiming) -> ControlFlow<()> {
        if let Some(track) = &mut self.current {
            track.media = Some(timing);
        }
        ControlFlow::Continue(())
    }

    fn on_track_edit_list(&mut self, entries: &[Mp4EditListEntry]) -> ControlFlow<()> {
        if let Some(track) = &mut self.current {
            track.first_edit = entries.first().copied();
        }
        ControlFlow::Continue(())
    }

    fn on_itunsmpb(&mut self, info: ItunSmpb) -> ControlFlow<()> {
        if info.leading_frames == 0 && info.trailing_frames == 0 {
            return ControlFlow::Continue(());
        }
        self.itunsmpb = Some(GaplessInfo {
            leading_frames: info.leading_frames,
            trailing_frames: info.trailing_frames,
        });
        // We only get here if no `elst` produced gapless yet — the iTunes
        // freeform tag lives in `udta/meta`, which sits after `trak` boxes in
        // a normal `moov`. Stop the scan now so we don't keep walking.
        ControlFlow::Break(())
    }
}

fn derive_from_track(track: &TrackState, movie_timescale: Option<u32>) -> Option<GaplessInfo> {
    let movie_timescale = movie_timescale?;
    let sample_rate = track.sample_rate?;
    let media = track.media?;
    let edit = track.first_edit?;
    let media_time = u64::try_from(edit.media_time).ok()?;

    let leading = scale_frames(media_time, sample_rate, media.timescale)?;
    let valid = scale_frames(edit.segment_duration, sample_rate, movie_timescale)?;
    let total = scale_frames(media.duration, sample_rate, media.timescale)?;
    let trailing = total.checked_sub(leading.saturating_add(valid))?;

    if leading == 0 && trailing == 0 {
        return None;
    }

    Some(GaplessInfo {
        leading_frames: leading,
        trailing_frames: trailing,
    })
}

fn scale_frames(value: u64, numerator: u32, denominator: u32) -> Option<u64> {
    if denominator == 0 {
        return None;
    }

    let scaled = u128::from(value)
        .saturating_mul(u128::from(numerator))
        .saturating_add(u128::from(denominator / 2))
        / u128::from(denominator);
    u64::try_from(scaled).ok()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_test_utils::kithara;

    use crate::GaplessInfo;

    use super::probe_mp4_gapless;

    fn atom(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let size = u32::try_from(payload.len() + 8).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&payload);
        out
    }

    fn full_box(kind: &[u8; 4], version: u8, body: Vec<u8>) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, 0];
        payload.extend_from_slice(&body);
        atom(kind, payload)
    }

    fn mvhd(movie_timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&movie_timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        full_box(b"mvhd", 0, body)
    }

    fn mdhd(media_timescale: u32, media_duration: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&media_timescale.to_be_bytes());
        body.extend_from_slice(&media_duration.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        full_box(b"mdhd", 0, body)
    }

    fn elst_v0(segment_duration: u32, media_time: i32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&segment_duration.to_be_bytes());
        body.extend_from_slice(&media_time.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        full_box(b"elst", 0, body)
    }

    fn audio_sample_entry(codec: &[u8; 4], sample_rate: u32) -> Vec<u8> {
        let mut entry = vec![0; 6];
        entry.extend_from_slice(&1u16.to_be_bytes());
        entry.extend_from_slice(&[0; 8]);
        entry.extend_from_slice(&2u16.to_be_bytes());
        entry.extend_from_slice(&16u16.to_be_bytes());
        entry.extend_from_slice(&0u16.to_be_bytes());
        entry.extend_from_slice(&0u16.to_be_bytes());
        entry.extend_from_slice(&(sample_rate << 16).to_be_bytes());
        atom(codec, entry)
    }

    fn stsd(sample_rate: u32) -> Vec<u8> {
        let entry = audio_sample_entry(b"mp4a", sample_rate);
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&entry);
        full_box(b"stsd", 0, body)
    }

    fn data_box(data_type: u32, value: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&data_type.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(value);
        atom(b"data", body)
    }

    fn freeform_text_box(kind: &[u8; 4], text: &str) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 1];
        body.extend_from_slice(text.as_bytes());
        atom(kind, body)
    }

    fn freeform_itunsmpb(text: &str) -> Vec<u8> {
        let mut freeform = Vec::new();
        freeform.extend_from_slice(&freeform_text_box(b"mean", "com.apple.iTunes"));
        freeform.extend_from_slice(&freeform_text_box(b"name", "iTunSMPB"));
        freeform.extend_from_slice(&data_box(1, text.as_bytes()));
        atom(b"----", freeform)
    }

    fn track_with_elst(
        sample_rate: u32,
        media_timescale: u32,
        media_duration: u32,
        segment_duration: u32,
        media_time: i32,
    ) -> Vec<u8> {
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&stsd(sample_rate));

        let minf = atom(b"minf", atom(b"stbl", stbl));

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd(media_timescale, media_duration));
        mdia.extend_from_slice(&minf);

        let edts = atom(b"edts", elst_v0(segment_duration, media_time));

        let mut trak = Vec::new();
        trak.extend_from_slice(&atom(b"mdia", mdia));
        trak.extend_from_slice(&edts);
        atom(b"trak", trak)
    }

    #[kithara::test]
    fn derives_gapless_from_edit_list() {
        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&track_with_elst(48_000, 48_000, 96_000, 1_916, 2_112));

        let mut reader = Cursor::new(atom(b"moov", moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader).expect("probe"),
            Some(GaplessInfo {
                leading_frames: 2_112,
                trailing_frames: 1_920,
            })
        );
    }

    #[kithara::test]
    fn derives_gapless_from_itunsmpb_when_elst_missing() {
        let ilst = atom(
            b"ilst",
            freeform_itunsmpb(" 00000000 00000840 00000048 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&atom(b"udta", atom(b"meta", meta_payload)));

        let mut reader = Cursor::new(atom(b"moov", moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader).expect("probe"),
            Some(GaplessInfo {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            })
        );
    }

    #[kithara::test]
    fn elst_takes_priority_over_itunsmpb() {
        // elst yields leading=2112 trailing=1920; iTunSMPB encodes a different
        // pair. The probe must commit to the elst-derived value.
        let ilst = atom(
            b"ilst",
            freeform_itunsmpb(" 00000000 00000010 00000020 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&track_with_elst(48_000, 48_000, 96_000, 1_916, 2_112));
        moov.extend_from_slice(&atom(b"udta", atom(b"meta", meta_payload)));

        let mut reader = Cursor::new(atom(b"moov", moov));
        assert_eq!(
            probe_mp4_gapless(&mut reader).expect("probe"),
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
        let mut reader = Cursor::new(atom(b"moov", moov));
        assert_eq!(probe_mp4_gapless(&mut reader).expect("probe"), None);
    }

    #[kithara::test]
    fn returns_none_for_zero_padding_itunsmpb() {
        let ilst = atom(
            b"ilst",
            freeform_itunsmpb(" 00000000 00000000 00000000 0000000000000000"),
        );
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd(1_000));
        moov.extend_from_slice(&atom(b"udta", atom(b"meta", meta_payload)));

        let mut reader = Cursor::new(atom(b"moov", moov));
        assert_eq!(probe_mp4_gapless(&mut reader).expect("probe"), None);
    }
}
