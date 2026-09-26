//! Byte oracle for an HLS fixture whose media bytes the test chose: the
//! test pattern, shared custom bytes or per-variant custom bytes, each after
//! the variant's optional init bytes.

use std::iter;

use super::hls::{CreatedHls, HlsFixtureData, HlsFixtureInit};
use crate::fixture_protocol::{DataMode, InitMode};

/// Where the media bytes of one variant come from.
enum Media<'a> {
    Bytes(&'a [u8]),
    Pattern { variant: usize, segment_size: usize },
}

impl CreatedHls {
    /// Init-segment bytes of `variant`, empty when the fixture has none.
    ///
    /// # Panics
    ///
    /// Panics when the fixture's init segments are not plain bytes.
    #[must_use]
    pub fn init_bytes(&self, variant: usize) -> &[u8] {
        match &self.fixture.init {
            HlsFixtureInit::Spec(InitMode::None) => &[],
            HlsFixtureInit::PerVariantBytes(data) => {
                data.get(variant).map_or(&[], |d| d.as_slice())
            }
            HlsFixtureInit::Spec(InitMode::Custom(data)) => {
                data.get(variant).map_or(&[], Vec::as_slice)
            }
            HlsFixtureInit::Spec(other) => {
                panic!("HLS byte oracle: init mode {other:?} has no byte contract")
            }
        }
    }

    /// Length of variant 0's init segment.
    #[must_use]
    pub fn init_len(&self) -> u64 {
        self.init_bytes(0).len() as u64
    }

    /// Bytes of variant 0's full stream: init plus every media segment.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        let spec = self.spec();
        self.init_len() + spec.segments_per_variant as u64 * spec.segment_size as u64
    }

    /// Nominal duration of one variant: segments times segment duration.
    #[must_use]
    pub fn total_duration_secs(&self) -> f64 {
        let spec = self.spec();
        spec.segments_per_variant as f64 * spec.segment_duration_secs
    }

    fn media(&self, variant: usize) -> Media<'_> {
        match &self.fixture.data {
            HlsFixtureData::Spec(DataMode::TestPattern) => Media::Pattern {
                variant,
                segment_size: self.spec().segment_size,
            },
            HlsFixtureData::SharedBytes(data) => Media::Bytes(data),
            HlsFixtureData::Spec(DataMode::CustomData(data)) => Media::Bytes(data),
            HlsFixtureData::PerVariantBytes(data) => {
                Media::Bytes(data.get(variant).map_or(&[], |d| d.as_slice()))
            }
            HlsFixtureData::Spec(DataMode::CustomDataPerVariant(data)) => {
                Media::Bytes(data.get(variant).map_or(&[], Vec::as_slice))
            }
            HlsFixtureData::Spec(other) => {
                panic!("HLS byte oracle: data mode {other:?} has no byte contract")
            }
        }
    }

    /// The byte the fixture serves at `offset` of `variant`'s byte stream
    /// (`[init][segment 0][segment 1]…`).
    #[must_use]
    pub fn expected_byte_at(&self, variant: usize, offset: u64) -> u8 {
        let init = self.init_bytes(variant);
        let init_len = init.len() as u64;
        if offset < init_len {
            return init[offset as usize];
        }
        let media_offset = offset - init_len;
        match self.media(variant) {
            Media::Bytes(data) => usize::try_from(media_offset)
                .ok()
                .and_then(|offset| data.get(offset))
                .copied()
                .unwrap_or(0),
            Media::Pattern {
                variant,
                segment_size,
            } => {
                let segment = (media_offset / segment_size as u64) as usize;
                let in_segment = (media_offset % segment_size as u64) as usize;
                format!("V{variant}-SEG-{segment}:TEST_SEGMENT_DATA")
                    .as_bytes()
                    .get(in_segment)
                    .copied()
                    .unwrap_or(0xFF)
            }
        }
    }

    /// Calls `mismatch(index, expected, actual)` for every byte of `actual`
    /// that differs from what the fixture serves starting at `offset`.
    pub fn for_each_expected_byte_mismatch(
        &self,
        variant: usize,
        offset: u64,
        actual: &[u8],
        mut mismatch: impl FnMut(usize, u8, u8),
    ) {
        let init = self.init_bytes(variant);
        let init_len = init.len() as u64;

        let mut checked = 0usize;
        if offset < init_len {
            let init_count = usize::try_from(init_len - offset)
                .map_or(actual.len(), |remaining| actual.len().min(remaining));
            scan_data(init, offset, &actual[..init_count], 0, &mut mismatch);
            checked = init_count;
        }
        if checked == actual.len() {
            return;
        }

        let media_offset = offset.saturating_sub(init_len);
        let media_actual = &actual[checked..];
        match self.media(variant) {
            Media::Bytes(data) => {
                scan_data(data, media_offset, media_actual, checked, &mut mismatch);
            }
            Media::Pattern {
                variant,
                segment_size,
            } => scan_pattern(
                variant,
                segment_size,
                media_offset,
                media_actual,
                checked,
                &mut mismatch,
            ),
        }
    }
}

fn scan_data(
    data: &[u8],
    data_offset: u64,
    actual: &[u8],
    base_index: usize,
    mismatch: &mut impl FnMut(usize, u8, u8),
) {
    let start = usize::try_from(data_offset).map_or(data.len(), |offset| offset.min(data.len()));
    let data_len = actual.len().min(data.len() - start);
    scan(
        data[start..start + data_len].iter().copied(),
        &actual[..data_len],
        base_index,
        mismatch,
    );
    scan(
        iter::repeat(0),
        &actual[data_len..],
        base_index + data_len,
        mismatch,
    );
}

/// The test pattern: each segment opens with `V{v}-SEG-{s}:TEST_SEGMENT_DATA`
/// and is padded with `0xFF`.
fn scan_pattern(
    variant: usize,
    segment_size: usize,
    mut media_offset: u64,
    actual: &[u8],
    base_index: usize,
    mismatch: &mut impl FnMut(usize, u8, u8),
) {
    let mut checked = 0usize;
    while checked < actual.len() {
        let segment = (media_offset / segment_size as u64) as usize;
        let in_segment = (media_offset % segment_size as u64) as usize;
        let n = (segment_size - in_segment).min(actual.len() - checked);
        let prefix = format!("V{variant}-SEG-{segment}:TEST_SEGMENT_DATA");
        let prefix = prefix.as_bytes().get(in_segment..).unwrap_or_default();
        let prefix_len = n.min(prefix.len());
        scan(
            prefix[..prefix_len]
                .iter()
                .copied()
                .chain(iter::repeat(0xFF)),
            &actual[checked..checked + n],
            base_index + checked,
            mismatch,
        );
        checked += n;
        media_offset += n as u64;
    }
}

fn scan(
    expected: impl IntoIterator<Item = u8>,
    actual: &[u8],
    base_index: usize,
    mismatch: &mut impl FnMut(usize, u8, u8),
) {
    for (index, (expected, &actual)) in expected.into_iter().zip(actual).enumerate() {
        if actual != expected {
            mismatch(base_index + index, expected, actual);
        }
    }
}
