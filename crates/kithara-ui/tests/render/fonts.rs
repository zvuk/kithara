use kithara_test_utils::kithara;
use kithara_ui::render::fonts::FONT_BYTES;

#[kithara::test]
fn font_catalog_contains_embedded_bytes() {
    assert!(FONT_BYTES.iter().all(|bytes| !bytes.is_empty()));
}
