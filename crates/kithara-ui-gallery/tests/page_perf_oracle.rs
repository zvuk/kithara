use kithara_test_utils::kithara;
use kithara_ui_gallery::sections;

#[kithara::test]
fn measured_pages_are_gallery_pages_or_harness_owned() {
    let source = include_str!("page_perf/pages.rs");
    for tab in sections::pages().iter().chain(sections::modules()) {
        assert!(
            source.contains(&format!("name: \"{tab}\"")),
            "gallery page is not measured: {tab}"
        );
    }
}
