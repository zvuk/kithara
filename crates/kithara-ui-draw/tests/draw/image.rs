use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use kithara_ui_draw::{Image, ImageId};

#[kithara::test]
fn a_picture_whose_pixels_fill_its_size_is_drawable() {
    let image = Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 8]));

    assert!(image.is_some());
}

#[kithara::test]
fn a_picture_shorter_than_its_size_is_not_a_picture() {
    assert_eq!(
        Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 7])),
        None
    );
}

#[kithara::test]
fn a_picture_longer_than_its_size_is_not_a_picture() {
    assert_eq!(
        Image::pixels(ImageId::new("sheet"), 2, 1, Arc::from(vec![0_u8; 9])),
        None
    );
}

#[kithara::test]
fn a_picture_with_no_area_is_not_a_picture() {
    assert_eq!(
        Image::pixels(ImageId::new("sheet"), 0, 4, Arc::from(Vec::new())),
        None
    );
}

#[kithara::test]
fn a_picture_on_the_device_carries_no_pixels() {
    assert_eq!(
        Image::external(ImageId::new("shader/field"), 8, 8).rgba(),
        None
    );
}
