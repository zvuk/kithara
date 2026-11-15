use num_traits::ToPrimitive;

/// Folds ordered covered columns on one fixed grid without allocating uncovered columns.
pub(crate) fn bucketize<T: Copy>(
    raw: impl IntoIterator<Item = (usize, T)>,
    total: usize,
    buckets: usize,
    empty: T,
    combine: impl Fn(T, T) -> T,
) -> Vec<T> {
    let mut out = vec![empty; buckets];
    if total == 0 || buckets == 0 {
        return out;
    }
    let width = u128::try_from(buckets).expect("usize fits u128");
    let total_wide = u128::try_from(total).expect("usize fits u128");
    let mut previous = None;
    for (index, value) in raw.into_iter().filter(|(index, _)| *index < total) {
        let bucket =
            ((u128::try_from(index).expect("usize fits u128") + 1) * width - 1) / total_wide;
        let bucket = bucket
            .to_usize()
            .expect("bucket index is bounded by output length");
        out[bucket] = if previous == Some(bucket) {
            combine(out[bucket], value)
        } else {
            value
        };
        previous = Some(bucket);
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_fixtures::{
        analysis_fixtures::{bucket_components, bucket_ranges, bucket_short, bucket_sine},
        fixtures::pcm_ramp,
    };
    use kithara_test_utils::kithara;

    use super::bucketize as sparse;

    fn bucketize<T: Copy>(
        raw: &[T],
        buckets: usize,
        empty: T,
        combine: impl Fn(T, T) -> T,
    ) -> Vec<T> {
        sparse(
            raw.iter().copied().enumerate(),
            raw.len(),
            buckets,
            empty,
            combine,
        )
    }

    #[kithara::test]
    fn zero_buckets_is_empty(pcm_ramp: Vec<f32>) {
        let out = bucketize(&pcm_ramp[..3], 0, 0.0, f32::max);
        assert!(out.is_empty());
    }

    #[kithara::test]
    fn folds_each_range_with_combine(bucket_ranges: Vec<f32>) {
        // 6 values, 3 buckets -> ranges [0,2), [2,4), [4,6); max per range.
        let raw = bucket_ranges;
        let out = bucketize(&raw, 3, 0.0, f32::max);
        assert_eq!(out, vec![0.9, 0.3, 0.8]);
    }

    #[kithara::test]
    fn component_add_combine(bucket_components: Vec<f32>) {
        let (raw, remainder) = bucket_components.as_chunks::<3>();
        assert!(remainder.is_empty());
        let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        // 3 values into 1 bucket -> the whole range folds together.
        let out = bucketize(raw, 1, [0.0; 3], add);
        assert_eq!(out, vec![[1.0, 2.0, 3.0]]);
    }

    #[kithara::test]
    fn normalized_mapping_splits_evenly(pcm_ramp: Vec<f32>) {
        // 4 values, 2 buckets -> [0,2) and [2,4): first half vs second half.
        let raw = &pcm_ramp[..4];
        let out = bucketize(raw, 2, 0.0, f32::max);
        assert_eq!(out, vec![2.0, 4.0]);
    }

    #[kithara::test]
    fn short_track_empty_ranges_fill_empty(bucket_short: Vec<f32>) {
        // 3 values, 5 buckets: some bucket ranges are empty and must use `empty`.
        let raw = bucket_short;
        let out = bucketize(&raw, 5, -1.0, f32::max);
        assert_eq!(out.len(), 5);
        // Ranges: [0,0) [0,1) [1,1) [1,2) [2,3) -> empty,0.5,empty,0.25,0.75
        assert_eq!(out, vec![-1.0, 0.5, -1.0, 0.25, 0.75]);
    }

    #[kithara::test]
    fn empty_input_all_empty_fill() {
        let out = bucketize::<f32>(&[], 4, 0.0, f32::max);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0]);
    }

    #[kithara::test]
    fn deterministic_for_same_input(bucket_sine: Vec<f32>) {
        let raw = bucket_sine;
        assert_eq!(
            bucketize(&raw, 64, 0.0, f32::max),
            bucketize(&raw, 64, 0.0, f32::max),
        );
    }
}
