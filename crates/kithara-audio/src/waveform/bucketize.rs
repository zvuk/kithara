/// Fold a fixed-rate raw series into exactly `buckets` values by normalized
/// position, combining each bucket's range with `combine`. Bucket `b` folds the
/// raw range `[b*R/N, (b+1)*R/N)`; an empty range yields `empty` (very short
/// tracks). This is THE `[0, 1]` index boundary - it lives in exactly one place.
pub(crate) fn bucketize<T: Copy>(
    raw: &[T],
    buckets: usize,
    empty: T,
    combine: impl Fn(T, T) -> T,
) -> Vec<T> {
    if buckets == 0 {
        return Vec::new();
    }

    let len = raw.len();
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let start = b * len / buckets;
        let end = (b + 1) * len / buckets;
        let value = match raw[start..end].split_first() {
            Some((first, rest)) => rest.iter().fold(*first, |acc, &v| combine(acc, v)),
            None => empty,
        };
        out.push(value);
    }
    out
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::bucketize;

    #[kithara::test]
    fn zero_buckets_is_empty() {
        let out = bucketize(&[1.0_f32, 2.0, 3.0], 0, 0.0, f32::max);
        assert!(out.is_empty());
    }

    #[kithara::test]
    fn folds_each_range_with_combine() {
        // 6 values, 3 buckets -> ranges [0,2), [2,4), [4,6); max per range.
        let raw = [0.1_f32, 0.9, 0.2, 0.3, 0.8, 0.4];
        let out = bucketize(&raw, 3, 0.0, f32::max);
        assert_eq!(out, vec![0.9, 0.3, 0.8]);
    }

    #[kithara::test]
    fn component_add_combine() {
        let raw = [[1.0_f32, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]];
        let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        // 3 values into 1 bucket -> the whole range folds together.
        let out = bucketize(&raw, 1, [0.0; 3], add);
        assert_eq!(out, vec![[1.0, 2.0, 3.0]]);
    }

    #[kithara::test]
    fn normalized_mapping_splits_evenly() {
        // 4 values, 2 buckets -> [0,2) and [2,4): first half vs second half.
        let raw = [1.0_f32, 2.0, 3.0, 4.0];
        let out = bucketize(&raw, 2, 0.0, f32::max);
        assert_eq!(out, vec![2.0, 4.0]);
    }

    #[kithara::test]
    fn short_track_empty_ranges_fill_empty() {
        // 3 values, 5 buckets: some bucket ranges are empty and must use `empty`.
        let raw = [0.5_f32, 0.25, 0.75];
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
    fn deterministic_for_same_input() {
        let raw: Vec<f32> = (0..1000_u16).map(|i| (f32::from(i) * 0.01).sin()).collect();
        assert_eq!(
            bucketize(&raw, 64, 0.0, f32::max),
            bucketize(&raw, 64, 0.0, f32::max),
        );
    }
}
