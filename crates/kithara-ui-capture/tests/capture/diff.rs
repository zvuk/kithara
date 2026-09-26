use std::{fs, iter::once, path::Path};

use kithara_test_utils::kithara;
use kithara_ui_capture::{Geometry, diff::compare, write_geometry, write_png};
use tempfile::tempdir;

/// Two capture sets of one row each, photographed on the same geometry.
fn sets(root: &Path, width: u32) -> (std::path::PathBuf, std::path::PathBuf) {
    let left = root.join("left");
    let right = root.join("right");
    let frame = Geometry {
        height: 1,
        scale: 1.0,
        width,
    };
    for dir in [&left, &right] {
        fs::create_dir_all(dir).expect("a capture set directory");
        write_geometry(dir, frame).expect("the set's geometry");
    }
    (left, right)
}

fn row(width: usize, color: [u8; 4]) -> Vec<u8> {
    color.repeat(width)
}

#[kithara::test]
fn gate_fails_on_the_larger_of_the_two_numbers() {
    let root = tempdir().expect("a private capture directory");
    let (left_dir, right_dir) = sets(root.path(), 10);

    // Diff share is 0%, ink share is 10%: the one lighter pixel on the left is
    // within noise of the right's background but outside noise of its own. A
    // budget of 5% sits strictly between the two, so the gate only fails if it
    // is judged by the larger number.
    let mut left = row(10, [200, 200, 200, 255]);
    left[0..4].copy_from_slice(&[226, 226, 226, 255]);
    let right = row(10, [205, 205, 205, 255]);
    write_png(&left_dir.join("page.png"), 10, 1, once(left.as_slice())).expect("left page");
    write_png(&right_dir.join("page.png"), 10, 1, once(right.as_slice())).expect("right page");

    let budget_path = root.path().join("budget.txt");
    fs::write(&budget_path, "page.png 5\n").expect("a budget file");

    let report = compare(
        &left_dir,
        &right_dir,
        &root.path().join("out"),
        Some(&budget_path),
    )
    .expect("two complete capture sets");
    let message = report.to_string();

    assert!(!report.passed());
    assert!(message.contains("over budget: page.png differs by 10.0%, allowed 5.0%"));
    assert!(message.contains("1 page(s) compared, 1 over budget"));
}

#[kithara::test]
fn an_unjudged_comparison_reports_a_missing_page_without_failing() {
    let root = tempdir().expect("a private capture directory");
    let (left_dir, right_dir) = sets(root.path(), 1);
    write_png(
        &left_dir.join("missing.png"),
        1,
        1,
        once([0, 0, 0, 255].as_slice()),
    )
    .expect("left page");

    let report = compare(&left_dir, &right_dir, &root.path().join("out"), None)
        .expect("matching geometry is enough for an unjudged comparison");
    let message = report.to_string();

    assert!(report.passed());
    assert!(message.contains("missing.png"));
    assert!(message.contains("missing"));
    assert!(message.contains("0 page(s) compared; no budget given, so nothing was decided"));
}
