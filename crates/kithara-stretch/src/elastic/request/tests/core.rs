use kithara_test_utils::kithara;

use crate::{ElasticError, ElasticRequest};

#[kithara::test]
fn audible_advance_preserves_admission_and_rejects_an_empty_span() {
    let physical = ElasticRequest::new(32, 16).expect("physical span");
    let projected = physical
        .with_output_source_frames(8)
        .expect("audible source advance");

    assert_eq!(projected.source_frames(), 32);
    assert_eq!(projected.output_frames(), 16);
    assert_eq!(projected.output_source_frames(), 8);
    assert_eq!(physical.output_source_frames(), 32);
    assert_eq!(
        physical.with_output_source_frames(0),
        Err(ElasticError::EmptySource)
    );
}
