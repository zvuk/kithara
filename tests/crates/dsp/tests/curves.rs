use kithara_dsp::fade::FadeCurve;
use kithara_host::{CrossfaderBus, crossfader_gain};
use kithara_play::{CrossfadeCurve, CrossfadeSettings};
use kithara_test_utils::kithara;

const POSITIONS: [f32; 7] = [0.0, 5.0e-6, 0.1, 0.5, 0.9, 0.999_995, 1.0];

#[kithara::test(native, flash(false))]
fn host_crossfader_speaks_the_equal_power_curve() {
    for position in POSITIONS {
        let (a, b) = FadeCurve::EqualPower3dB.compute_gains_0_to_1(position);
        let gain = |bus| crossfader_gain(bus, position).expect("position lies in 0..=1");
        assert_eq!(
            gain(CrossfaderBus::A).to_bits(),
            a.to_bits(),
            "bus A at {position}"
        );
        assert_eq!(
            gain(CrossfaderBus::B).to_bits(),
            b.to_bits(),
            "bus B at {position}"
        );
    }
}

#[kithara::test(native, flash(false))]
#[case::linear(CrossfadeCurve::Linear, FadeCurve::Linear)]
#[case::equal_power(CrossfadeCurve::EqualPower, FadeCurve::EqualPower3dB)]
fn play_crossfade_at_the_centre_pivot_speaks_the_same_curve(
    #[case] curve: CrossfadeCurve,
    #[case] fade: FadeCurve,
) {
    let settings = CrossfadeSettings::new(1.0, curve, 1.0, 0.5).expect("settings are valid");
    for position in POSITIONS {
        let (outgoing, incoming) = settings.gains(position);
        let (a, b) = fade.compute_gains_0_to_1(position);
        assert_eq!(outgoing.to_bits(), a.to_bits(), "outgoing at {position}");
        assert_eq!(incoming.to_bits(), b.to_bits(), "incoming at {position}");
    }
}
