#![cfg(not(target_arch = "wasm32"))]

//! Gapless pipeline integration contracts that need public test helpers.

use kithara_decode::{
    DecoderTrackInfo, GaplessInfo, PcmSpec, mock::scripted_inner_decoder_with_track_info_loose,
};
use kithara_test_utils::kithara;

#[kithara::test]
fn scripted_decoder_exposes_gapless_track_info() {
    let mut gapless = GaplessInfo::default();
    gapless.leading_frames = 2_112;
    gapless.trailing_frames = 960;
    let mut track_info = DecoderTrackInfo::default();
    track_info.gapless = Some(gapless);

    let spec = PcmSpec {
        channels: 2,
        sample_rate: 44_100,
    };
    let (decoder, _) = scripted_inner_decoder_with_track_info_loose(
        spec,
        Vec::new(),
        Vec::new(),
        None,
        track_info.clone(),
    );

    assert_eq!(decoder.track_info(), track_info);
}
