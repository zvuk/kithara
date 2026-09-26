use kithara_test_utils::kithara;

use super::compare::{Image, difference, ink_disagreement};

fn solid(width: u32, height: u32, color: [u8; 4]) -> Image {
    Image {
        height,
        rgba: color.repeat((width * height) as usize),
        width,
    }
}

#[kithara::test]
fn identical_images_score_zero_on_both_numbers() {
    let a = solid(4, 4, [200, 200, 200, 255]);
    let b = solid(4, 4, [200, 200, 200, 255]);

    let (share, _) = difference(&a, &b);
    let ink = ink_disagreement(&a, &b);

    assert_eq!(share, 0.0);
    assert_eq!(ink, 0.0);
}

#[kithara::test]
fn ink_present_on_one_side_only_scores_high_even_when_pixel_diff_is_low() {
    // 226 sits within noise of right's 205 background, so `difference` never
    // flags it, but it sits outside noise of left's own 200 background: ink
    // on the left, plain background on the right — a control missing on one host.
    let mut left = solid(10, 1, [200, 200, 200, 255]);
    left.rgba[0..4].copy_from_slice(&[226, 226, 226, 255]);
    let right = solid(10, 1, [205, 205, 205, 255]);

    let (share, _) = difference(&left, &right);
    let ink = ink_disagreement(&left, &right);

    assert_eq!(share, 0.0);
    assert_eq!(ink, 0.1);
}
