use std::num::{NonZeroU32, NonZeroU64};

use kithara_analysis::{
    AnalysisFile, AnalysisFileSpec, AnalysisFileUpdate, AnalysisFingerprint, AnalysisProgress,
    AnalysisToken, BeatArtifact, BeatSnapshot, BeatState, Coverage, FrameRange, TrackAnalysis,
};
use kithara_test_macros as kithara;

use super::score::{self, ChannelLayout, Control, Origin, Style};

struct Consts;

impl Consts {
    const RHYTHM_FRAMES: u64 = 48_000 * 12;
    const RHYTHM_LONG_FRAMES: u64 = 48_000 * 15;
    const RHYTHM_LISTENING_FRAMES: u64 = 48_000 * 45;
    const RHYTHM_LISTENING_LONG_FRAMES: u64 = 48_000 * 55;
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::ambient_dub_62_aligned(Style::AmbientDub, Control::Aligned)]
#[case::ambient_dub_62_one_frame_late(Style::AmbientDub, Control::OneFrameLate)]
#[case::ambient_dub_62_one_beat_bar_late(Style::AmbientDub, Control::OneBeatBarLate)]
#[case::ambient_dub_62_missing_beat(Style::AmbientDub, Control::MissingBeat)]
#[case::trip_hop_74_aligned(Style::TripHop, Control::Aligned)]
#[case::trip_hop_74_one_frame_late(Style::TripHop, Control::OneFrameLate)]
#[case::trip_hop_74_one_beat_bar_late(Style::TripHop, Control::OneBeatBarLate)]
#[case::trip_hop_74_missing_beat(Style::TripHop, Control::MissingBeat)]
#[case::downtempo_96_aligned(Style::Downtempo, Control::Aligned)]
#[case::downtempo_96_one_frame_late(Style::Downtempo, Control::OneFrameLate)]
#[case::downtempo_96_one_beat_bar_late(Style::Downtempo, Control::OneBeatBarLate)]
#[case::downtempo_96_missing_beat(Style::Downtempo, Control::MissingBeat)]
#[case::house_124_aligned(Style::House, Control::Aligned)]
#[case::house_124_one_frame_late(Style::House, Control::OneFrameLate)]
#[case::house_124_one_beat_bar_late(Style::House, Control::OneBeatBarLate)]
#[case::house_124_missing_beat(Style::House, Control::MissingBeat)]
#[case::techno_132_aligned(Style::Techno, Control::Aligned)]
#[case::techno_132_one_frame_late(Style::Techno, Control::OneFrameLate)]
#[case::techno_132_one_beat_bar_late(Style::Techno, Control::OneBeatBarLate)]
#[case::techno_132_missing_beat(Style::Techno, Control::MissingBeat)]
#[case::breakbeat_140_aligned(Style::Breakbeat, Control::Aligned)]
#[case::breakbeat_140_one_frame_late(Style::Breakbeat, Control::OneFrameLate)]
#[case::breakbeat_140_one_beat_bar_late(Style::Breakbeat, Control::OneBeatBarLate)]
#[case::breakbeat_140_missing_beat(Style::Breakbeat, Control::MissingBeat)]
fn rhythm_wav(style: Style, control: Control) -> Vec<u8> {
    score::wav(style, control)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::downtempo_96_left_only(Style::Downtempo, ChannelLayout::LeftOnly)]
#[case::house_124_left_only(Style::House, ChannelLayout::LeftOnly)]
#[case::house_124_right_only(Style::House, ChannelLayout::RightOnly)]
fn rhythm_wav_scenario_1(style: Style, layout: ChannelLayout) -> Vec<u8> {
    score::wav_with_layout(style, Control::Aligned, layout)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::house_124_right_only_pickup(Style::House, ChannelLayout::RightOnly)]
fn rhythm_wav_scenario_2(style: Style, layout: ChannelLayout) -> Vec<u8> {
    score::wav_with_layout(style, Control::OneBeatBarLate, layout)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin(
        style,
        Control::Aligned,
        ChannelLayout::LeftOnly,
        Origin::Zero,
    )
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero_long(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin_for_frames(
        style,
        Control::Aligned,
        ChannelLayout::LeftOnly,
        Origin::Zero,
        Consts::RHYTHM_LONG_FRAMES,
    )
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::downtempo_96_stereo_45s(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero_listening(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin_for_frames(
        style,
        Control::Aligned,
        ChannelLayout::Stereo,
        Origin::Zero,
        Consts::RHYTHM_LISTENING_FRAMES,
    )
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::downtempo_96_stereo_55s(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero_listening_long(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin_for_frames(
        style,
        Control::Aligned,
        ChannelLayout::Stereo,
        Origin::Zero,
        Consts::RHYTHM_LISTENING_LONG_FRAMES,
    )
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::downtempo_96_stereo_45s(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero_pickup_listening(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin_for_frames(
        style,
        Control::OneBeatBarLate,
        ChannelLayout::Stereo,
        Origin::Zero,
        Consts::RHYTHM_LISTENING_FRAMES,
    )
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_wav_scenario_1_origin_zero_pickup_long(style: Style) -> Vec<u8> {
    score::wav_with_layout_at_origin_for_frames(
        style,
        Control::OneBeatBarLate,
        ChannelLayout::LeftOnly,
        Origin::Zero,
        Consts::RHYTHM_LONG_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_{case}"]
)]
#[case::ambient_dub_62_aligned(Style::AmbientDub, Control::Aligned)]
#[case::ambient_dub_62_one_frame_late(Style::AmbientDub, Control::OneFrameLate)]
#[case::ambient_dub_62_one_beat_bar_late(Style::AmbientDub, Control::OneBeatBarLate)]
#[case::ambient_dub_62_missing_beat(Style::AmbientDub, Control::MissingBeat)]
#[case::trip_hop_74_aligned(Style::TripHop, Control::Aligned)]
#[case::trip_hop_74_one_frame_late(Style::TripHop, Control::OneFrameLate)]
#[case::trip_hop_74_one_beat_bar_late(Style::TripHop, Control::OneBeatBarLate)]
#[case::trip_hop_74_missing_beat(Style::TripHop, Control::MissingBeat)]
#[case::downtempo_96_aligned(Style::Downtempo, Control::Aligned)]
#[case::downtempo_96_one_frame_late(Style::Downtempo, Control::OneFrameLate)]
#[case::downtempo_96_one_beat_bar_late(Style::Downtempo, Control::OneBeatBarLate)]
#[case::downtempo_96_missing_beat(Style::Downtempo, Control::MissingBeat)]
#[case::house_124_aligned(Style::House, Control::Aligned)]
#[case::house_124_one_frame_late(Style::House, Control::OneFrameLate)]
#[case::house_124_one_beat_bar_late(Style::House, Control::OneBeatBarLate)]
#[case::house_124_missing_beat(Style::House, Control::MissingBeat)]
#[case::techno_132_aligned(Style::Techno, Control::Aligned)]
#[case::techno_132_one_frame_late(Style::Techno, Control::OneFrameLate)]
#[case::techno_132_one_beat_bar_late(Style::Techno, Control::OneBeatBarLate)]
#[case::techno_132_missing_beat(Style::Techno, Control::MissingBeat)]
#[case::breakbeat_140_aligned(Style::Breakbeat, Control::Aligned)]
#[case::breakbeat_140_one_frame_late(Style::Breakbeat, Control::OneFrameLate)]
#[case::breakbeat_140_one_beat_bar_late(Style::Breakbeat, Control::OneBeatBarLate)]
#[case::breakbeat_140_missing_beat(Style::Breakbeat, Control::MissingBeat)]
fn rhythm_expected_analysis(_inputs: &[&[u8]], style: Style, control: Control) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth(style, control)),
        Consts::RHYTHM_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_{case}"]
)]
#[case::downtempo_96_left_only(Style::Downtempo)]
#[case::house_124_left_only(Style::House)]
#[case::house_124_right_only(Style::House)]
fn rhythm_expected_analysis_scenario_1(_inputs: &[&[u8]], style: Style) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth(style, Control::Aligned)),
        Consts::RHYTHM_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_2_{case}"]
)]
#[case::house_124_right_only_pickup(Style::House)]
fn rhythm_expected_analysis_scenario_2(_inputs: &[&[u8]], style: Style) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth(style, Control::OneBeatBarLate)),
        Consts::RHYTHM_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_{case}"]
)]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero(_inputs: &[&[u8]], style: Style) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin(
            style,
            Control::Aligned,
            Origin::Zero,
        )),
        Consts::RHYTHM_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_long_{case}"]
)]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero_long(
    _inputs: &[&[u8]],
    style: Style,
) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin_for_frames(
            style,
            Control::Aligned,
            Origin::Zero,
            Consts::RHYTHM_LONG_FRAMES,
        )),
        Consts::RHYTHM_LONG_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_listening_{case}"]
)]
#[case::downtempo_96_stereo_45s(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero_listening(
    _inputs: &[&[u8]],
    style: Style,
) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin_for_frames(
            style,
            Control::Aligned,
            Origin::Zero,
            Consts::RHYTHM_LISTENING_FRAMES,
        )),
        Consts::RHYTHM_LISTENING_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_listening_long_{case}"]
)]
#[case::downtempo_96_stereo_55s(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero_listening_long(
    _inputs: &[&[u8]],
    style: Style,
) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin_for_frames(
            style,
            Control::Aligned,
            Origin::Zero,
            Consts::RHYTHM_LISTENING_LONG_FRAMES,
        )),
        Consts::RHYTHM_LISTENING_LONG_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_pickup_listening_{case}"]
)]
#[case::downtempo_96_stereo_45s(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero_pickup_listening(
    _inputs: &[&[u8]],
    style: Style,
) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin_for_frames(
            style,
            Control::OneBeatBarLate,
            Origin::Zero,
            Consts::RHYTHM_LISTENING_FRAMES,
        )),
        Consts::RHYTHM_LISTENING_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_scenario_1_origin_zero_pickup_long_{case}"]
)]
#[case::house_124_left_only(Style::House)]
#[case::downtempo_96_left_only(Style::Downtempo)]
fn rhythm_expected_analysis_scenario_1_origin_zero_pickup_long(
    _inputs: &[&[u8]],
    style: Style,
) -> Vec<u8> {
    analysis_file(
        BeatArtifact::from(score::truth_at_origin_for_frames(
            style,
            Control::OneBeatBarLate,
            Origin::Zero,
            Consts::RHYTHM_LONG_FRAMES,
        )),
        Consts::RHYTHM_LONG_FRAMES,
    )
}

#[kithara::asset(
    ext = "analysis",
    content_type = "application/x-kithara-analysis",
    depends_on = ["rhythm_wav_{case}"]
)]
#[case::ambient_dub_62_aligned()]
#[case::ambient_dub_62_one_frame_late()]
#[case::ambient_dub_62_one_beat_bar_late()]
#[case::ambient_dub_62_missing_beat()]
#[case::trip_hop_74_aligned()]
#[case::trip_hop_74_one_frame_late()]
#[case::trip_hop_74_one_beat_bar_late()]
#[case::trip_hop_74_missing_beat()]
#[case::downtempo_96_aligned()]
#[case::downtempo_96_one_frame_late()]
#[case::downtempo_96_one_beat_bar_late()]
#[case::downtempo_96_missing_beat()]
#[case::house_124_aligned()]
#[case::house_124_one_frame_late()]
#[case::house_124_one_beat_bar_late()]
#[case::house_124_missing_beat()]
#[case::techno_132_aligned()]
#[case::techno_132_one_frame_late()]
#[case::techno_132_one_beat_bar_late()]
#[case::techno_132_missing_beat()]
#[case::breakbeat_140_aligned()]
#[case::breakbeat_140_one_frame_late()]
#[case::breakbeat_140_one_beat_bar_late()]
#[case::breakbeat_140_missing_beat()]
fn rhythm_analyzed_analysis(inputs: &[&[u8]]) -> Vec<u8> {
    analysis_file(
        super::analyze::beat(
            inputs
                .first()
                .expect("invariant: the declared rhythm WAV dependency is present"),
        ),
        Consts::RHYTHM_FRAMES,
    )
}

pub(in crate::defs) fn analysis_file(artifact: BeatArtifact, frames: u64) -> Vec<u8> {
    const CHUNK_SECONDS: u64 = 16;
    const FINGERPRINT: &str = "rhythm-fixture:v1";

    let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate");
    let mut coverage = Coverage::default();
    coverage.insert(FrameRange::new(0, frames));
    let analysis = TrackAnalysis::builder()
        .token(AnalysisToken::from("rhythm-fixture"))
        .revision(1)
        .source_sample_rate(sample_rate)
        .extent(frames)
        .coverage(coverage)
        .fingerprint(AnalysisFingerprint::new(Some(FINGERPRINT), None))
        .settled(true)
        .maybe_waveform(None)
        .beat(BeatSnapshot::new(artifact, BeatState::Final, Vec::new()))
        .build();
    let progress = AnalysisProgress::try_from(analysis)
        .expect("settled fixture analysis forms final progress");
    let chunk_frames = NonZeroU64::new(u64::from(sample_rate.get()) * CHUNK_SECONDS)
        .expect("fixture analysis chunk is non-zero");
    let spec = AnalysisFileSpec::for_analysis(progress.analysis(), chunk_frames)
        .expect("fixture analysis has a stable extent");
    let update = AnalysisFile::create(&spec, &progress).expect("fixture analysis file encodes");
    let bytes = materialize(&update);
    AnalysisFile::parse(&bytes, progress.analysis().fingerprint())
        .expect("fixture analysis file restores");
    bytes
}

fn materialize(update: &AnalysisFileUpdate) -> Vec<u8> {
    let len = usize::try_from(update.final_len()).expect("fixture analysis length fits usize");
    let mut bytes = vec![0; len];
    let initial = update
        .initial_bytes()
        .expect("new fixture analysis supplies its fixed prefix");
    bytes[..initial.len()].copy_from_slice(initial);
    write_at(
        &mut bytes,
        update.payload().offset(),
        update.payload().bytes(),
    );
    for patch in update.patches() {
        write_at(&mut bytes, patch.offset(), patch.bytes());
    }
    bytes
}

fn write_at(destination: &mut [u8], offset: u64, source: &[u8]) {
    let start = usize::try_from(offset).expect("fixture analysis offset fits usize");
    let end = start
        .checked_add(source.len())
        .expect("fixture analysis write end fits usize");
    destination
        .get_mut(start..end)
        .expect("fixture analysis write stays in committed length")
        .copy_from_slice(source);
}
