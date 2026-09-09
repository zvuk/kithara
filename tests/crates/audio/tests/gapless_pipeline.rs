#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    self,
    decode::{DecoderTrackInfo, GaplessInfo},
    signal::AudioSpec,
};
use kithara_integration_tests::decode_mock::{ScriptedOptions, scripted_decoder};

#[kithara::test]
fn scripted_decoder_exposes_gapless_track_info() {
    let mut gapless = GaplessInfo::default();
    gapless.leading_frames = 2_112;
    gapless.trailing_frames = 960;
    let mut track_info = DecoderTrackInfo::default();
    track_info.gapless = Some(gapless);

    let spec = AudioSpec::new(2, NonZeroU32::new(44100).expect("test rate"));
    let (decoder, _) = scripted_decoder(
        spec,
        Vec::new(),
        Vec::new(),
        None,
        ScriptedOptions {
            track_info: track_info.clone(),
            verify_in_drop: false,
        },
    );

    assert_eq!(decoder.track_info(), track_info);
}
