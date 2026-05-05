use std::{
    io::{Cursor, Seek, SeekFrom},
    ops::ControlFlow,
};

use kithara_test_utils::kithara;

use super::{
    ItunSmpb, Mp4EditListEntry, Mp4MediaTiming, Mp4MetadataError, Mp4Visitor, parse_data_box,
    parse_elst, parse_itunsmpb, parse_mdhd, parse_mvhd_timescale, scan_mp4,
};

#[path = "test_cursors.rs"]
mod test_cursors;
use test_cursors::{CountingCursor, NoEndSeekCursor};

/// Fixture visitor that simply records every callback. Tests assert on the
/// recorded sequence to verify scanner behavior.
#[derive(Default, Debug, PartialEq, Eq)]
struct RecordingVisitor {
    movie_timescale: Option<u32>,
    tracks: Vec<RecordedTrack>,
    itunsmpb: Option<ItunSmpb>,
    in_track: Option<RecordedTrack>,
}

#[derive(Default, Debug, PartialEq, Eq, Clone)]
struct RecordedTrack {
    sample_rate: Option<u32>,
    timing: Option<Mp4MediaTiming>,
    edit_list: Vec<Mp4EditListEntry>,
}

impl Mp4Visitor for RecordingVisitor {
    fn on_movie_timescale(&mut self, timescale: u32) -> ControlFlow<()> {
        self.movie_timescale = Some(timescale);
        ControlFlow::Continue(())
    }
    fn on_track_begin(&mut self) -> ControlFlow<()> {
        self.in_track = Some(RecordedTrack::default());
        ControlFlow::Continue(())
    }
    fn on_track_end(&mut self) -> ControlFlow<()> {
        if let Some(track) = self.in_track.take() {
            self.tracks.push(track);
        }
        ControlFlow::Continue(())
    }
    fn on_track_sample_rate(&mut self, sample_rate: u32) -> ControlFlow<()> {
        if let Some(track) = &mut self.in_track {
            track.sample_rate = Some(sample_rate);
        }
        ControlFlow::Continue(())
    }
    fn on_track_media_timing(&mut self, timing: Mp4MediaTiming) -> ControlFlow<()> {
        if let Some(track) = &mut self.in_track {
            track.timing = Some(timing);
        }
        ControlFlow::Continue(())
    }
    fn on_track_edit_list(&mut self, entries: &[Mp4EditListEntry]) -> ControlFlow<()> {
        if let Some(track) = &mut self.in_track {
            track.edit_list = entries.to_vec();
        }
        ControlFlow::Continue(())
    }
    fn on_itunsmpb(&mut self, info: ItunSmpb) -> ControlFlow<()> {
        self.itunsmpb = Some(info);
        ControlFlow::Continue(())
    }
}

fn record(reader: &mut dyn super::DecoderInput) -> RecordingVisitor {
    let mut visitor = RecordingVisitor::default();
    scan_mp4(reader, &mut visitor).expect("scan");
    visitor
}

fn atom_parts(kind: [u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let payload_len: usize = parts.iter().map(|part| part.len()).sum();
    let size = u32::try_from(payload_len + 8).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(payload_len + 8);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&kind);
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn atom(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    atom_parts(kind, &[payload])
}

/// Builds a box with an explicit 64-bit (extended) size header.
fn extended_atom(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let total = (payload.len() + 16) as u64;
    let mut out = Vec::with_capacity(payload.len() + 16);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: [u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let header = [version, 0, 0, 0];
    atom_parts(kind, &[&header, body])
}

fn mvhd_v0(movie_timescale: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    body.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    body.extend_from_slice(&movie_timescale.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // duration
    full_box(*b"mvhd", 0, &body)
}

fn mvhd_v1(movie_timescale: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&movie_timescale.to_be_bytes());
    body.extend_from_slice(&0u64.to_be_bytes());
    full_box(*b"mvhd", 1, &body)
}

fn mdhd_v0(media_timescale: u32, media_duration: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&media_timescale.to_be_bytes());
    body.extend_from_slice(&media_duration.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // language + pre_defined
    full_box(*b"mdhd", 0, &body)
}

fn mdhd_v1(media_timescale: u32, media_duration: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&media_timescale.to_be_bytes());
    body.extend_from_slice(&media_duration.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    full_box(*b"mdhd", 1, &body)
}

fn elst_v0_one_entry(segment_duration: u32, media_time: i32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&segment_duration.to_be_bytes());
    body.extend_from_slice(&media_time.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes()); // media_rate_integer
    body.extend_from_slice(&0u16.to_be_bytes()); // media_rate_fraction
    full_box(*b"elst", 0, &body)
}

fn elst_v1_one_entry(segment_duration: u64, media_time: i64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&segment_duration.to_be_bytes());
    body.extend_from_slice(&media_time.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes());
    full_box(*b"elst", 1, &body)
}

fn audio_sample_entry(codec: [u8; 4], sample_rate: u32) -> Vec<u8> {
    let mut sample_entry = vec![0; 6]; // reserved
    sample_entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    sample_entry.extend_from_slice(&[0; 8]); // reserved
    sample_entry.extend_from_slice(&2u16.to_be_bytes()); // channelcount
    sample_entry.extend_from_slice(&16u16.to_be_bytes()); // samplesize
    sample_entry.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    sample_entry.extend_from_slice(&0u16.to_be_bytes()); // reserved
    sample_entry.extend_from_slice(&(sample_rate << 16).to_be_bytes());
    atom(codec, &sample_entry)
}

fn stsd(sample_rate: u32, codec: [u8; 4]) -> Vec<u8> {
    let entry = audio_sample_entry(codec, sample_rate);
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&entry);
    full_box(*b"stsd", 0, &body)
}

fn data_box(data_type: u32, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&data_type.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // locale
    body.extend_from_slice(value);
    atom(*b"data", &body)
}

fn freeform_text_box(kind: [u8; 4], text: &str) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 1]; // version + flags
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

fn make_track_mp4() -> Vec<u8> {
    let mut stbl = Vec::new();
    stbl.extend_from_slice(&stsd(48_000, *b"mp4a"));

    let mut minf = Vec::new();
    minf.extend_from_slice(&atom(*b"stbl", &stbl));

    let mut mdia = Vec::new();
    mdia.extend_from_slice(&mdhd_v0(48_000, 96_000));
    mdia.extend_from_slice(&atom(*b"minf", &minf));

    let mut edts = Vec::new();
    edts.extend_from_slice(&elst_v0_one_entry(1_916, 2_112));

    let mut trak = Vec::new();
    trak.extend_from_slice(&atom(*b"mdia", &mdia));
    trak.extend_from_slice(&atom(*b"edts", &edts));

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));
    moov.extend_from_slice(&atom(*b"trak", &trak));

    atom(*b"moov", &moov)
}

fn make_itunsmpb_mp4(text: &str) -> Vec<u8> {
    let ilst = atom(*b"ilst", &freeform_itunsmpb(text));

    let mut meta_payload = vec![0, 0, 0, 0];
    meta_payload.extend_from_slice(&ilst);

    let udta = atom(*b"meta", &meta_payload);

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));
    moov.extend_from_slice(&atom(*b"udta", &udta));
    atom(*b"moov", &moov)
}

/// `moov` containing a `QuickTime`-style `meta` box (no `FullBox` header)
/// embedded directly under `moov` rather than under `udta`, with an
/// iTunSMPB freeform tag inside.
fn make_quicktime_meta_mp4(text: &str) -> Vec<u8> {
    let ilst = atom(*b"ilst", &freeform_itunsmpb(text));
    // QuickTime meta has no FullBox prefix.
    let meta = atom(*b"meta", &ilst);

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(600));
    moov.extend_from_slice(&meta);
    atom(*b"moov", &moov)
}

#[kithara::test]
fn emits_track_callbacks_in_order() {
    let mut reader = Cursor::new(make_track_mp4());
    let recorded = record(&mut reader);

    assert_eq!(recorded.movie_timescale, Some(1_000));
    assert_eq!(
        recorded.tracks,
        vec![RecordedTrack {
            sample_rate: Some(48_000),
            timing: Some(Mp4MediaTiming {
                timescale: 48_000,
                duration: 96_000,
            }),
            edit_list: vec![Mp4EditListEntry {
                segment_duration: 1_916,
                media_time: 2_112,
            }],
        }]
    );
}

#[kithara::test]
fn emits_v1_mvhd_and_mdhd_with_64bit_durations() {
    let mut stbl = Vec::new();
    stbl.extend_from_slice(&stsd(48_000, *b"alac"));

    let mut minf = atom(*b"stbl", &stbl);
    minf = atom(*b"minf", &minf);

    let mut mdia = Vec::new();
    mdia.extend_from_slice(&mdhd_v1(96_000, 1u64 << 33));
    mdia.extend_from_slice(&minf);

    let trak = atom(*b"trak", &atom(*b"mdia", &mdia));

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v1(48_000));
    moov.extend_from_slice(&trak);

    let mut reader = Cursor::new(atom(*b"moov", &moov));
    let recorded = record(&mut reader);

    assert_eq!(recorded.movie_timescale, Some(48_000));
    assert_eq!(
        recorded.tracks,
        vec![RecordedTrack {
            sample_rate: Some(48_000),
            timing: Some(Mp4MediaTiming {
                timescale: 96_000,
                duration: 1u64 << 33,
            }),
            edit_list: vec![],
        }]
    );
}

#[kithara::test]
fn emits_v1_elst_with_signed_media_time() {
    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));

    let edts = atom(*b"edts", &elst_v1_one_entry(2_000, -1));
    moov.extend_from_slice(&atom(*b"trak", &edts));

    let mut reader = Cursor::new(atom(*b"moov", &moov));
    let recorded = record(&mut reader);

    assert_eq!(
        recorded.tracks[0].edit_list,
        vec![Mp4EditListEntry {
            segment_duration: 2_000,
            media_time: -1,
        }]
    );
}

#[kithara::test]
fn parses_extended_size_box() {
    let mut file = atom(*b"ftyp", b"isom");
    file.extend_from_slice(&extended_atom(*b"mdat", &[0; 32]));
    file.extend_from_slice(&make_track_mp4());

    let mut reader = Cursor::new(file);
    let recorded = record(&mut reader);

    assert_eq!(recorded.movie_timescale, Some(1_000));
    assert_eq!(recorded.tracks.len(), 1);
}

#[kithara::test]
fn parses_quicktime_style_meta_without_fullbox_header() {
    let mut reader = Cursor::new(make_quicktime_meta_mp4(
        " 00000000 00000840 00000048 0000000000000000",
    ));
    let recorded = record(&mut reader);

    assert_eq!(
        recorded.itunsmpb,
        Some(ItunSmpb {
            leading_frames: 0x840,
            trailing_frames: 0x48,
        })
    );
}

#[kithara::test]
fn emits_typed_itunsmpb_from_freeform_tag() {
    let mut reader = Cursor::new(make_itunsmpb_mp4(
        " 00000000 00000840 00000048 0000000000000000",
    ));
    let recorded = record(&mut reader);

    assert_eq!(
        recorded.itunsmpb,
        Some(ItunSmpb {
            leading_frames: 0x840,
            trailing_frames: 0x48,
        })
    );
}

#[kithara::test]
fn skips_non_itunsmpb_freeform_without_reading_data() {
    // A `----` whose name is not "iTunSMPB" must not call on_itunsmpb,
    // and must not consume the data-box payload (we just sanity-check the
    // visitor here; payload-skip is enforced by the parser's contract).
    let mut freeform = Vec::new();
    freeform.extend_from_slice(&freeform_text_box(*b"mean", "com.apple.iTunes"));
    freeform.extend_from_slice(&freeform_text_box(*b"name", "OTHER"));
    freeform.extend_from_slice(&data_box(1, b"value"));
    let ilst = atom(*b"ilst", &atom(*b"----", &freeform));

    let mut meta_payload = vec![0, 0, 0, 0];
    meta_payload.extend_from_slice(&ilst);

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));
    moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

    let mut reader = Cursor::new(atom(*b"moov", &moov));
    let recorded = record(&mut reader);

    assert_eq!(recorded.itunsmpb, None);
}

#[kithara::test]
fn ignores_standard_ilst_items_including_cover_art() {
    // Build an `ilst` containing a fake covr (with a chunky payload) plus
    // a freeform iTunSMPB. Only the freeform tag should reach the visitor.
    let cover_payload = vec![0xFFu8; 1024];
    let cover = atom(*b"covr", &data_box(13, &cover_payload));
    let title = atom([0xa9, b'n', b'a', b'm'], &data_box(1, b"Title"));

    let mut ilst = Vec::new();
    ilst.extend_from_slice(&cover);
    ilst.extend_from_slice(&title);
    ilst.extend_from_slice(&freeform_itunsmpb(
        " 00000000 00000840 00000048 0000000000000000",
    ));

    let mut meta_payload = vec![0, 0, 0, 0];
    meta_payload.extend_from_slice(&atom(*b"ilst", &ilst));

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));
    moov.extend_from_slice(&atom(*b"udta", &atom(*b"meta", &meta_payload)));

    let mut reader = Cursor::new(atom(*b"moov", &moov));
    let recorded = record(&mut reader);

    assert_eq!(
        recorded.itunsmpb,
        Some(ItunSmpb {
            leading_frames: 0x840,
            trailing_frames: 0x48,
        })
    );
}

#[kithara::test]
fn visitor_break_stops_scan_at_track_end() {
    // Two trak boxes; the visitor signals Break after the first one.
    let trak1 = atom(*b"trak", &atom(*b"mdia", &mdhd_v0(48_000, 96_000)));
    let trak2 = atom(*b"trak", &atom(*b"mdia", &mdhd_v0(44_100, 132_300)));

    let mut moov = Vec::new();
    moov.extend_from_slice(&mvhd_v0(1_000));
    moov.extend_from_slice(&trak1);
    moov.extend_from_slice(&trak2);
    let bytes = atom(*b"moov", &moov);

    struct BreakAfterFirstTrack {
        tracks_seen: u32,
    }
    impl Mp4Visitor for BreakAfterFirstTrack {
        fn on_track_end(&mut self) -> ControlFlow<()> {
            self.tracks_seen += 1;
            ControlFlow::Break(())
        }
    }

    let mut visitor = BreakAfterFirstTrack { tracks_seen: 0 };
    let mut reader = Cursor::new(bytes);
    scan_mp4(&mut reader, &mut visitor).expect("scan");
    assert_eq!(visitor.tracks_seen, 1);
}

#[kithara::test]
fn scan_seeks_past_large_boxes_to_find_moov() {
    let mut file = atom(*b"ftyp", b"isom");
    file.extend_from_slice(&atom(*b"mdat", &vec![0; (4 * 1024 * 1024) + 128]));
    file.extend_from_slice(&make_track_mp4());

    let mut reader = CountingCursor::new(file);
    let recorded = record(&mut reader);

    assert_eq!(recorded.movie_timescale, Some(1_000));
    assert_eq!(recorded.tracks.len(), 1);
    assert!(reader.bytes_read < 4_096, "unexpected read amplification");
}

#[kithara::test]
fn scan_restores_reader_position() {
    let mut reader = Cursor::new(make_itunsmpb_mp4(
        " 00000000 00000840 00000048 0000000000000000",
    ));
    reader.seek(SeekFrom::Start(3)).expect("seek inside mp4");

    let mut visitor = RecordingVisitor::default();
    scan_mp4(&mut reader, &mut visitor).expect("scan");

    // Position 3 is mid-`ftyp`-style header — the scan must not have moved
    // the cursor for the caller, regardless of what it read internally.
    assert_eq!(reader.stream_position().expect("position"), 3);
}

#[kithara::test]
fn scan_uses_current_reader_position() {
    // Place a junk prefix in front of a real moov, then position the
    // reader past the prefix. The scan must work from that point and not
    // rewind the reader behind the caller's back.
    let mut bytes = b"junkjunk".to_vec();
    bytes.extend_from_slice(&make_track_mp4());

    let mut reader = Cursor::new(bytes);
    reader.seek(SeekFrom::Start(8)).expect("skip prefix");

    let recorded = record(&mut reader);
    assert_eq!(recorded.movie_timescale, Some(1_000));
    assert_eq!(recorded.tracks.len(), 1);
}

#[kithara::test]
fn scan_does_not_seek_to_end() {
    let mut reader = NoEndSeekCursor::new(make_track_mp4());
    let recorded = record(&mut reader);

    assert_eq!(recorded.movie_timescale, Some(1_000));
    assert_eq!(recorded.tracks.len(), 1);
}

#[kithara::test]
fn top_level_size_zero_terminates_scan_without_error() {
    let mut file = atom(*b"ftyp", b"isom");
    // Header that says "size = 0", meaning extends to end of file. With
    // moov before this marker, scanning should still succeed.
    file.extend_from_slice(&make_track_mp4());
    file.extend_from_slice(&[0, 0, 0, 0, b'm', b'd', b'a', b't']);

    let mut reader = Cursor::new(file);
    let recorded = record(&mut reader);
    assert_eq!(recorded.tracks.len(), 1);
}

#[kithara::test]
fn rejects_child_box_extending_past_parent() {
    let mut file = atom(*b"ftyp", b"isom");
    // moov size 24 (8 header + 16 payload). Inside, declare an mvhd of
    // size 128 — well past the parent's end. The scanner must reject this.
    let mut moov_payload = Vec::new();
    moov_payload.extend_from_slice(&128u32.to_be_bytes());
    moov_payload.extend_from_slice(b"mvhd");
    moov_payload.extend_from_slice(&[0; 8]);
    file.extend_from_slice(&atom(*b"moov", &moov_payload));

    let mut reader = Cursor::new(file);
    let mut visitor = RecordingVisitor::default();
    let error = scan_mp4(&mut reader, &mut visitor).expect_err("must fail");
    assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
}

#[kithara::test]
fn rejects_box_smaller_than_header() {
    let mut file = atom(*b"ftyp", b"isom");
    file.extend_from_slice(&4u32.to_be_bytes()); // size=4, smaller than 8-byte header
    file.extend_from_slice(b"junk");

    let mut reader = Cursor::new(file);
    let mut visitor = RecordingVisitor::default();
    let error = scan_mp4(&mut reader, &mut visitor).expect_err("must fail");
    assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
}

#[kithara::test]
fn parse_mvhd_timescale_handles_versions_and_truncation() {
    let mut v0 = vec![0, 0, 0, 0]; // version + flags
    v0.extend_from_slice(&[0; 8]); // creation + modification
    v0.extend_from_slice(&44_100u32.to_be_bytes());
    assert_eq!(parse_mvhd_timescale(&v0), Some(44_100));

    let mut v1 = vec![1, 0, 0, 0];
    v1.extend_from_slice(&[0; 16]);
    v1.extend_from_slice(&96_000u32.to_be_bytes());
    assert_eq!(parse_mvhd_timescale(&v1), Some(96_000));

    assert_eq!(parse_mvhd_timescale(&[]), None);
    assert_eq!(parse_mvhd_timescale(&[0, 0, 0, 0]), None);
    assert_eq!(parse_mvhd_timescale(&[2, 0, 0, 0]), None);
}

#[kithara::test]
fn parse_mdhd_handles_versions_and_truncation() {
    let mut v0 = vec![0, 0, 0, 0];
    v0.extend_from_slice(&[0; 8]);
    v0.extend_from_slice(&48_000u32.to_be_bytes());
    v0.extend_from_slice(&100u32.to_be_bytes());
    assert_eq!(
        parse_mdhd(&v0),
        Some(Mp4MediaTiming {
            timescale: 48_000,
            duration: 100,
        })
    );

    let mut v1 = vec![1, 0, 0, 0];
    v1.extend_from_slice(&[0; 16]);
    v1.extend_from_slice(&44_100u32.to_be_bytes());
    v1.extend_from_slice(&(u64::MAX - 1).to_be_bytes());
    assert_eq!(
        parse_mdhd(&v1),
        Some(Mp4MediaTiming {
            timescale: 44_100,
            duration: u64::MAX - 1,
        })
    );

    assert_eq!(parse_mdhd(&[]), None);
    assert_eq!(parse_mdhd(&[0, 0, 0, 0, 1, 2]), None);
}

#[kithara::test]
fn parse_data_box_strips_version_byte_from_type_code() {
    let mut payload = vec![0x01, 0x00, 0x00, 0x0d]; // version=1, type=13 (JPEG)
    payload.extend_from_slice(&0u32.to_be_bytes()); // locale
    payload.extend_from_slice(b"\xff\xd8\xff");

    let (data_type, value) = parse_data_box(&payload).expect("data box");
    assert_eq!(data_type, 13);
    assert_eq!(value, b"\xff\xd8\xff");
}

#[kithara::test]
fn parse_data_box_returns_none_for_short_payload() {
    assert!(parse_data_box(&[]).is_none());
    assert!(parse_data_box(&[0; 3]).is_none());
    assert!(parse_data_box(&[0; 7]).is_none());
}

#[kithara::test]
fn rejects_unsupported_elst_version() {
    let mut payload = vec![3, 0, 0, 0]; // version 3
    payload.extend_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&[0; 12]);

    let error = parse_elst(&payload).expect_err("must fail");
    assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
}

#[kithara::test]
fn elst_caps_entry_count() {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&u32::MAX.to_be_bytes());

    let error = parse_elst(&payload).expect_err("must fail");
    assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
}

#[kithara::test]
fn parse_itunsmpb_extracts_leading_and_trailing_frames() {
    let info =
        parse_itunsmpb(b" 00000000 00000840 00000048 0000000000000000").expect("itunsmpb parses");
    assert_eq!(
        info,
        ItunSmpb {
            leading_frames: 0x840,
            trailing_frames: 0x48,
        }
    );
}

#[kithara::test]
fn parse_itunsmpb_rejects_short_or_malformed_input() {
    assert_eq!(parse_itunsmpb(b""), None);
    assert_eq!(parse_itunsmpb(b"only one"), None);
    assert_eq!(parse_itunsmpb(b" 00000000 ZZZZZZZZ 00000048 0"), None);
}
