use kithara_test_utils::kithara;

use crate::{
    ElasticConfig, ElasticCursor, ElasticError, ElasticSpan, ElasticSpanPlan, SignalsmithBackend,
};

const CONTINUITY_EPSILON: f64 = 1.0e-6;
const MAX_OUTPUT_FRAMES: usize = 480;

fn capabilities() -> super::ElasticCapabilities {
    let config =
        ElasticConfig::new(44_100, 2, 960, MAX_OUTPUT_FRAMES).expect("static exact-span config");
    SignalsmithBackend::prepare(config)
        .expect("signalsmith exact-span backend")
        .capabilities()
}

fn span(source_start: f64, source_end: f64, output_frames: usize) -> ElasticSpan {
    ElasticSpan::try_from((source_start..source_end, output_frames)).expect("valid continuous span")
}

fn plan(
    spans: &[ElasticSpan],
    cursor: Option<ElasticCursor>,
) -> Result<ElasticSpanPlan, ElasticError> {
    ElasticSpanPlan::new(spans.iter().copied(), cursor, capabilities())
}

fn source_cursor(source: f64) -> ElasticCursor {
    ElasticCursor::try_from(source).expect("representable source cursor")
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= CONTINUITY_EPSILON,
        "expected {expected}, received {actual}"
    );
}

fn phase_residuals(
    mut cursor: ElasticCursor,
    starts: [f64; 3],
    source_delta: f64,
) -> (Vec<f64>, ElasticCursor) {
    let mut residuals = vec![starts[0] - cursor.continuous()];
    for start in starts {
        let desired_end = start + source_delta;
        let plan = plan(&[span(start, desired_end, 120)], Some(cursor))
            .expect("phase error is correctable");
        assert_eq!(plan.segments()[0].source_start(), cursor.integer());
        let next = plan.cursor();
        residuals.push(desired_end - next.continuous());
        cursor = next;
    }
    (residuals, cursor)
}

#[kithara::test]
fn reverse_quantization_keeps_a_descending_cursor_inside_the_rate_envelope() {
    let plan = plan(&[span(360.0, 240.0, 120), span(240.0, 120.0, 120)], None)
        .expect("reverse source path is quantized");
    let segments = plan.segments();

    assert_eq!(segments[0].source_start(), 360);
    assert_eq!(segments[0].source_end(), 240);
    assert_eq!(segments[1].source_start(), 240);
    assert_eq!(segments[1].source_end(), 120);
    assert_eq!(plan.cursor().integer(), 120);
    assert_close(plan.cursor().continuous(), 120.0);
}

#[kithara::test]
fn reverse_phase_error_converges_without_a_source_jump() {
    let (residuals, _) = phase_residuals(source_cursor(360.0), [359.25, 239.25, 119.25], -120.0);

    assert_eq!(residuals, vec![-0.75, -0.5, -0.25, 0.0]);
}

#[kithara::test]
fn small_phase_error_converges_without_a_source_jump_and_is_partition_independent() {
    let (residuals, cursor) = phase_residuals(source_cursor(0.0), [0.75, 120.75, 240.75], 120.0);

    assert_eq!(residuals, vec![0.75, 0.5, 0.25, 0.0]);

    let whole = plan(&[span(0.75, 360.75, 360)], Some(source_cursor(0.0)))
        .expect("combined subrange is correctable");
    assert_eq!(whole.segments()[0].source_start(), 0);
    assert_close(whole.cursor().continuous(), cursor.continuous());
    assert_eq!(whole.cursor().integer(), cursor.integer());
}

#[kithara::test]
fn negative_phase_error_converges_without_overshoot() {
    let (residuals, _) = phase_residuals(source_cursor(0.75), [0.0, 120.0, 240.0], 120.0);

    assert_eq!(residuals, vec![-0.75, -0.5, -0.25, 0.0]);
}

#[kithara::test]
fn one_frame_error_is_continuous_but_larger_error_requires_relocation() {
    let cursor = Some(source_cursor(0.0));
    plan(&[span(1.0, 121.0, 120)], cursor)
        .expect("one source frame is inside the continuous envelope");

    let above_one = f64::from_bits(1.0_f64.to_bits() + 1);
    let error = plan(&[span(above_one, 120.0 + above_one, 120)], cursor)
        .expect_err("larger error requires prepared relocation");
    assert!(matches!(
        error,
        ElasticError::PhaseDiscontinuity { error, limit }
            if error > limit && limit == 1.0
    ));
}

#[kithara::test]
fn correction_respects_backend_rate_headroom() {
    let cursor = Some(source_cursor(0.0));
    let error = plan(&[span(0.75, 160.75, 120)], cursor)
        .expect_err("maximum nominal rate has no positive headroom");
    assert!(matches!(
        error,
        ElasticError::PhaseCorrectionUnavailable { error }
            if (error - 0.75).abs() <= CONTINUITY_EPSILON
    ));

    let corrected = plan(&[span(0.75, 160.65, 120)], cursor)
        .expect("partial headroom permits partial correction");
    let segment = corrected.segments()[0];
    assert_eq!(segment.source_end() - segment.source_start(), 160);
    assert_close(corrected.cursor().continuous(), 160.0);
    assert_close(160.65 - corrected.cursor().continuous(), 0.65);
}

#[kithara::test]
fn planned_segment_gap_is_not_treated_as_phase_error() {
    let error = plan(&[span(0.0, 120.0, 120), span(120.5, 240.5, 120)], None)
        .expect_err("planner segments must remain strictly adjacent");
    assert!(matches!(
        error,
        ElasticError::DiscontinuousSource {
            expected: 120.0,
            actual: 120.5
        }
    ));
}

#[kithara::test]
fn a_fifth_span_is_rejected_before_inline_storage_can_spill() {
    let spans = [
        span(0.0, 10.0, 10),
        span(10.0, 20.0, 10),
        span(20.0, 30.0, 10),
        span(30.0, 40.0, 10),
        span(40.0, 50.0, 10),
    ];

    assert!(matches!(
        plan(&spans, None),
        Err(ElasticError::SpanLimit { limit }) if limit == ElasticSpanPlan::MAX_SPANS
    ));
}

#[kithara::test]
fn one_plan_cannot_change_source_direction() {
    let error = plan(&[span(0.0, 10.0, 10), span(10.0, 0.0, 10)], None)
        .expect_err("one backend block has one source direction");

    assert_eq!(error, ElasticError::SourceDirectionChange);
}

#[kithara::test]
fn plan_limits_apply_to_the_complete_backend_block() {
    let error = plan(&[span(0.0, 481.0, 481)], None)
        .expect_err("the complete block must fit its prepared output limit");

    assert_eq!(
        error,
        ElasticError::OutputFrameLimit {
            frames: 481,
            limit: MAX_OUTPUT_FRAMES,
        }
    );
}
