# Phase 5: Long-term Improvements (84.17% → 90%+)

**Start date:** 2026-01-20
**Status:** 🚀 PLANNING
**Target:** 90%+ lines coverage
**Estimated effort:** 20-30 hours

---

## Executive Summary

Phase 5 focuses on **critical coverage gaps** in core functionality modules with the biggest impact on overall coverage. After Phase 4 Quick Wins achieved **84.17% lines coverage**, Phase 5 targets the remaining low-coverage modules to reach **90%+ coverage**.

### Current State (Post Phase 4)

| Metric | Coverage | Target |
|--------|----------|--------|
| **Lines** | 84.17% | 90%+ |
| **Regions** | 81.65% | 88%+ |
| **Functions** | 78.91% | 85%+ |

### Coverage Gap Analysis

**Total lines to improve:** ~1,500 lines
**Expected coverage gain:** +6-8% lines
**Priority:** High-impact modules (largest gap × most lines)

---

## Priority Matrix

### Critical Gaps (Biggest Impact)

| Module | Lines | Current | Target | Gap | Priority | Impact |
|--------|-------|---------|--------|-----|----------|--------|
| **decoder.rs** | 430 | 44.65% | 85% | 40.35% | **P0** 🔴 | **HIGH** |
| **pipeline.rs** | 464 | 47.20% | 85% | 37.80% | **P0** 🔴 | **HIGH** |
| **adapter.rs** | 187 | 49.73% | 85% | 35.27% | **P1** 🟡 | **MED** |
| **keys.rs** | 164 | 52.44% | 85% | 32.56% | **P1** 🟡 | **MED** |
| **retry.rs** | 71 | 63.38% | 90% | 26.62% | **P2** 🟢 | **LOW** |

### Quick Wins (Small modules, high ROI)

| Module | Lines | Current | Target | Gap | Priority |
|--------|-------|---------|--------|-----|----------|
| **options.rs** (hls) | 50 | 56.00% | 90% | 34% | **P2** |
| **parsing.rs** | 229 | 72.05% | 85% | 13% | **P2** |
| **error.rs** (net) | 52 | 80.77% | 95% | 15% | **P3** |

---

## Phase 5.1: decoder.rs (44.65% → 85%) 🔴

**Priority:** P0 (Critical)
**File:** `crates/kithara-decode/src/symphonia_mod/decoder.rs`
**Lines:** 430 | **Current:** 44.65% | **Target:** 85%
**Estimated gain:** +1.2% overall coverage

### Current Coverage Stats

```
Lines:     430 total, 238 missed (44.65% coverage)
Functions:  31 total,  17 missed (45.16% coverage)
Regions:   301 total, 151 missed (49.83% coverage)
```

### Analysis

**Problem:** Core decoding functionality critically undertested.

**Uncovered areas (likely):**
- Error handling paths (Symphonia errors)
- Codec-specific branches (AAC vs MP3 vs FLAC)
- Seek operations
- EOF handling
- Metadata extraction
- Stream initialization edge cases

### Testing Strategy

#### 1. Mock-based Unit Tests

**Pattern:** Generic decoder with trait abstraction

```rust
#[cfg_attr(any(test, feature = "test-utils"), automock)]
pub trait SymphoniaDecoder {
    fn decode_next(&mut self) -> Result<AudioBuffer, SymphoniaError>;
    fn seek(&mut self, pos: u64) -> Result<(), SymphoniaError>;
    fn track_info(&self) -> TrackInfo;
}
```

**Benefits:**
- Isolated from real Symphonia complexity
- Control error conditions
- Fast, deterministic tests

#### 2. Test Categories

**A. Codec Support Tests** (rstest parametrized)
```rust
#[rstest]
#[case("mp3", Codec::Mp3)]
#[case("aac", Codec::Aac)]
#[case("flac", Codec::Flac)]
#[case("opus", Codec::Opus)]
fn test_codec_detection(#[case] ext: &str, #[case] expected: Codec)
```

**B. Error Path Tests**
- UnsupportedCodec
- NoAudioTrack
- Corrupt stream
- Unexpected EOF
- Seek beyond bounds

**C. Decode Flow Tests**
- Single chunk decode
- Multi-chunk streaming
- Channel configuration (mono/stereo/5.1)
- Sample rate variations

**D. Seek Tests**
- Forward seek
- Backward seek
- Seek to 0
- Seek to EOF
- Invalid positions

#### 3. Integration with Mock Fixtures

```rust
struct MockAudioBuffer {
    spec: AudioSpec,
    samples: Vec<f32>,
}

impl MockSymphoniaDecoder {
    fn with_audio(spec: AudioSpec, frame_count: usize) -> Self {
        // Pre-configured mock for testing
    }

    fn with_error(error: SymphoniaError) -> Self {
        // Mock that returns error
    }
}
```

### Expected Tests

- **Codec tests:** 10-15 tests
- **Error paths:** 8-10 tests
- **Decode flow:** 12-15 tests
- **Seek operations:** 8-10 tests
- **Total:** ~40-50 tests

### Deliverables

1. ✅ Create `traits.rs` with `SymphoniaDecoder` trait + automock
2. ✅ Make decoder generic over trait
3. ✅ Add 40-50 comprehensive unit tests
4. ✅ Coverage: 44.65% → 85%+

---

## Phase 5.2: pipeline.rs (47.20% → 85%) 🔴

**Priority:** P0 (Critical)
**File:** `crates/kithara-decode/src/pipeline.rs`
**Lines:** 464 | **Current:** 47.20% | **Target:** 85%
**Estimated gain:** +1.3% overall coverage

### Current Coverage Stats

```
Lines:     464 total, 245 missed (47.20% coverage)
Functions:  44 total,  21 missed (52.27% coverage)
Regions:   327 total, 153 missed (53.21% coverage)
```

### Analysis

**Problem:** Pipeline orchestration (decoder → resampler → output) undertested.

**Uncovered areas (likely):**
- Error propagation through pipeline
- Buffer management edge cases
- Backpressure handling
- Cancellation scenarios
- Resampling path selection
- EOF propagation
- Multiple chunk processing

### Testing Strategy

#### 1. Integration-Style Unit Tests

**Use real components where fast:**
- Real PcmBuffer (in-memory)
- Mock decoder (Phase 5.1 trait)
- Real or mock resampler (depending on complexity)

**Pattern:**
```rust
async fn create_test_pipeline<D: Decoder>(
    decoder: D,
    target_sample_rate: u32,
) -> UnifiedPipeline {
    UnifiedPipeline::new(decoder, target_sample_rate)
}

#[tokio::test]
async fn test_pipeline_with_mock_decoder() {
    let mut mock_decoder = MockDecoder::new();
    mock_decoder
        .expect_next_chunk()
        .returning(|| Ok(PcmChunk::new(...)));

    let pipeline = create_test_pipeline(mock_decoder, 48000).await;
    // Test pipeline behavior
}
```

#### 2. Test Categories

**A. Basic Flow Tests**
- Single chunk processing
- Multi-chunk streaming
- Empty input handling
- EOF propagation

**B. Resampling Tests**
- No resampling (matching rates)
- Upsample (44100 → 48000)
- Downsample (48000 → 44100)
- Complex ratios (44100 → 96000)

**C. Error Propagation**
- Decoder error → pipeline error
- Resampler error → pipeline error
- Buffer full scenarios

**D. Cancellation Tests**
- Cancel during decode
- Cancel during resample
- Cancel during buffer write
- Graceful shutdown

**E. Buffer Management**
- Buffer capacity limits
- Backpressure handling
- Memory usage control

#### 3. Mock Sequences

```rust
#[tokio::test]
async fn test_multi_chunk_decode_sequence() {
    let mut mock_decoder = MockDecoder::new();

    let mut seq = Sequence::new();
    mock_decoder
        .expect_next_chunk()
        .times(3)
        .in_sequence(&mut seq)
        .returning(|| Ok(chunk1()));

    mock_decoder
        .expect_next_chunk()
        .times(1)
        .in_sequence(&mut seq)
        .return_once(|| Err(DecodeError::EndOfStream));

    let pipeline = create_test_pipeline(mock_decoder, 48000).await;
    // Verify 3 chunks processed, then EOF
}
```

### Expected Tests

- **Basic flow:** 10-12 tests
- **Resampling:** 8-10 tests
- **Error propagation:** 8-10 tests
- **Cancellation:** 6-8 tests
- **Buffer management:** 6-8 tests
- **Total:** ~40-50 tests

### Deliverables

1. ✅ Refactor pipeline to accept generic decoder
2. ✅ Add cancellation token tests
3. ✅ Add 40-50 comprehensive unit tests
4. ✅ Coverage: 47.20% → 85%+

---

## Phase 5.3: adapter.rs (49.73% → 85%) 🟡

**Priority:** P1 (High)
**File:** `crates/kithara-hls/src/adapter.rs`
**Lines:** 187 | **Current:** 49.73% | **Target:** 85%
**Estimated gain:** +0.5% overall coverage

### Current Coverage Stats

```
Lines:     187 total,  94 missed (49.73% coverage)
Functions:  23 total,  13 missed (43.48% coverage)
Regions:   127 total,  60 missed (52.76% coverage)
```

### Analysis

**Problem:** HLS stream adapter (byte stream → HLS segments) undertested.

**Uncovered areas (likely):**
- Segment boundary detection
- Variant switching mid-stream
- Buffer coordination with fetch manager
- Error recovery
- Seek operations
- EOF handling

### Testing Strategy

#### 1. Mock Dependencies

```rust
#[cfg_attr(test, automock)]
pub trait FetchManagerTrait {
    async fn fetch_segment(&self, idx: usize) -> Result<Bytes>;
    fn current_variant(&self) -> VariantId;
}

#[cfg_attr(test, automock)]
pub trait PlaylistManagerTrait {
    fn segment_info(&self, idx: usize) -> Option<SegmentInfo>;
    fn total_segments(&self) -> usize;
}
```

#### 2. Test Categories

**A. Segment Reading**
- Read single segment
- Read multiple segments sequentially
- Segment boundary handling
- Partial reads

**B. Variant Switching**
- Switch during segment
- Switch at segment boundary
- ABR-triggered switch
- Manual switch

**C. Seek Operations**
- Seek forward (same variant)
- Seek backward
- Seek with variant change
- Seek to invalid position

**D. Error Handling**
- Fetch error mid-segment
- Network timeout
- Invalid segment data
- Missing segments

### Expected Tests

- **Segment reading:** 10-12 tests
- **Variant switching:** 6-8 tests
- **Seek operations:** 6-8 tests
- **Error handling:** 8-10 tests
- **Total:** ~30-40 tests

### Deliverables

1. ✅ Create trait abstractions for dependencies
2. ✅ Add 30-40 unit tests with mocks
3. ✅ Coverage: 49.73% → 85%+

---

## Phase 5.4: keys.rs (52.44% → 85%) 🟡

**Priority:** P1 (High)
**File:** `crates/kithara-hls/src/keys.rs`
**Lines:** 164 | **Current:** 52.44% | **Target:** 85%
**Estimated gain:** +0.4% overall coverage

### Current Coverage Stats

```
Lines:     164 total,  78 missed (52.44% coverage)
Functions:  16 total,   8 missed (50.00% functions)
Regions:   112 total,  52 missed (53.57% coverage)
```

### Analysis

**Problem:** HLS encryption key management undertested.

**Uncovered areas (likely):**
- Key fetching from remote URLs
- Key caching logic
- AES-128 decryption
- IV (initialization vector) handling
- Multiple key URIs
- Key rotation
- Error handling (fetch failures, invalid keys)

### Testing Strategy

#### 1. Mock HTTP Client

```rust
#[cfg_attr(test, automock)]
pub trait KeyFetcher {
    async fn fetch_key(&self, uri: &str) -> Result<Bytes>;
}
```

#### 2. Test Categories

**A. Key Fetching**
- Fetch from HTTP URL
- Fetch from local file (if supported)
- Cached key reuse
- Cache invalidation
- Concurrent fetches (same key)

**B. Decryption**
- AES-128-CBC with IV
- Different IV formats
- Segment decryption
- Invalid key handling

**C. Key Management**
- Multiple keys per playlist
- Key rotation
- URI parsing
- Error recovery

**D. Cache Management**
- Cache hit
- Cache miss
- Cache eviction
- Cache size limits

### Expected Tests

- **Key fetching:** 10-12 tests
- **Decryption:** 8-10 tests
- **Key management:** 6-8 tests
- **Cache:** 6-8 tests
- **Total:** ~30-35 tests

### Deliverables

1. ✅ Mock HTTP client for key fetching
2. ✅ Add 30-35 unit tests
3. ✅ Coverage: 52.44% → 85%+

---

## Phase 5.5: retry.rs (63.38% → 90%) 🟢

**Priority:** P2 (Medium)
**File:** `crates/kithara-net/src/retry.rs`
**Lines:** 71 | **Current:** 63.38% | **Target:** 90%
**Estimated gain:** +0.15% overall coverage

### Current Coverage Stats

```
Lines:      71 total,  26 missed (63.38% coverage)
Functions:  18 total,   6 missed (66.67% functions)
Regions:    59 total,  18 missed (69.49% coverage)
```

### Analysis

**Problem:** Retry logic with exponential backoff undertested.

**Uncovered areas (likely):**
- Backoff calculation edge cases
- Jitter application
- Max attempts enforcement
- Specific error conditions
- Timeout interaction

### Testing Strategy

#### 1. Mock Time Control

```rust
use tokio::time::{pause, advance, Duration};

#[tokio::test]
async fn test_exponential_backoff() {
    pause(); // Control time in test

    let mut attempt = 0;
    let result = retry_with_backoff(|| async {
        attempt += 1;
        if attempt < 3 {
            Err(RetryableError)
        } else {
            Ok(())
        }
    }).await;

    // Verify backoff delays
}
```

#### 2. Test Categories

**A. Retry Logic**
- Immediate success (no retry)
- Single retry success
- Multiple retries
- Max attempts exceeded
- Non-retryable error

**B. Backoff Timing**
- Initial delay
- Exponential increase
- Jitter variation
- Max delay cap

**C. Error Classification**
- Retryable errors (500, timeout)
- Non-retryable errors (404, 401)
- Network errors
- Custom error handling

### Expected Tests

- **Retry logic:** 8-10 tests
- **Backoff timing:** 6-8 tests
- **Error classification:** 6-8 tests
- **Total:** ~20-25 tests

### Deliverables

1. ✅ Add time-controlled retry tests
2. ✅ Add error classification tests
3. ✅ Add 20-25 unit tests
4. ✅ Coverage: 63.38% → 90%+

---

## Technical Patterns (Phase 5)

### 1. Generic Trait Abstractions ✅

**All complex dependencies made generic:**

```rust
// Before (tightly coupled)
pub struct Pipeline {
    decoder: SymphoniaDecoder,
    resampler: Resampler,
}

// After (generic, mockable)
pub struct Pipeline<D, R> {
    decoder: D,
    resampler: R,
}

impl<D: Decoder, R: Resampler> Pipeline<D, R> {
    // Implementation
}

// Production type alias
pub type DefaultPipeline = Pipeline<SymphoniaDecoder, DefaultResampler>;
```

### 2. Mock Sequences for Flow Testing ✅

```rust
use mockall::Sequence;

#[test]
fn test_ordered_operations() {
    let mut mock = MockTrait::new();
    let mut seq = Sequence::new();

    mock.expect_init()
        .times(1)
        .in_sequence(&mut seq)
        .return_const(Ok(()));

    mock.expect_process()
        .times(3)
        .in_sequence(&mut seq)
        .returning(|_| Ok(()));

    mock.expect_finalize()
        .times(1)
        .in_sequence(&mut seq)
        .return_const(Ok(()));

    // Test ensures operations happen in order
}
```

### 3. Time-Controlled Async Tests ✅

```rust
use tokio::time::{pause, advance, Duration};

#[tokio::test]
async fn test_timeout_behavior() {
    pause(); // Freeze time

    let task = timeout(Duration::from_secs(5), long_operation());

    advance(Duration::from_secs(3)).await;
    assert!(!task.is_finished());

    advance(Duration::from_secs(3)).await;
    assert!(task.is_finished());
}
```

### 4. Parametrized Error Tests ✅

```rust
#[rstest]
#[case(StatusCode::INTERNAL_SERVER_ERROR, true, "500 is retryable")]
#[case(StatusCode::NOT_FOUND, false, "404 is not retryable")]
#[case(StatusCode::UNAUTHORIZED, false, "401 is not retryable")]
#[case(StatusCode::REQUEST_TIMEOUT, true, "408 is retryable")]
fn test_error_retryability(
    #[case] status: StatusCode,
    #[case] should_retry: bool,
    #[case] _desc: &str,
) {
    let error = HttpError::from(status);
    assert_eq!(error.is_retryable(), should_retry);
}
```

---

## Coverage Goals

### Target Metrics (Post Phase 5)

| Metric | Current | Target | Gain |
|--------|---------|--------|------|
| **Lines** | 84.17% | **90%+** | +6%+ |
| **Regions** | 81.65% | **88%+** | +6%+ |
| **Functions** | 78.91% | **85%+** | +6%+ |

### By Module

| Module | Current | Phase 5 Target |
|--------|---------|----------------|
| kithara-decode | ~65% | **85%+** |
| kithara-hls | ~78% | **88%+** |
| kithara-net | ~85% | **92%+** |
| kithara-storage | ~88% | **92%+** |
| kithara-stream | ~92% | **95%+** |

---

## Success Criteria

### Must Have ✅

1. **Coverage:** 90%+ lines overall
2. **Tests:** 150-200 new comprehensive tests
3. **Quality:** All tests use rstest, mocks, proper isolation
4. **Patterns:** Generic abstractions established
5. **Documentation:** Each phase documented in git commits

### Nice to Have 🎯

1. **95%+ coverage** on critical modules (decoder, pipeline)
2. **Error path coverage** comprehensive
3. **Cancellation tests** for all async operations
4. **Performance benchmarks** (baseline for future)

---

## Timeline

### Estimated Effort

| Phase | Module | Tests | Hours | Complexity |
|-------|--------|-------|-------|------------|
| 5.1 | decoder.rs | 40-50 | 8-10h | **High** |
| 5.2 | pipeline.rs | 40-50 | 8-10h | **High** |
| 5.3 | adapter.rs | 30-40 | 5-7h | **Medium** |
| 5.4 | keys.rs | 30-35 | 5-7h | **Medium** |
| 5.5 | retry.rs | 20-25 | 3-4h | **Low** |

**Total:** 150-200 tests, 29-38 hours

### Milestones

- **Week 1:** Phase 5.1 + 5.2 (decoder, pipeline) → 87% coverage
- **Week 2:** Phase 5.3 + 5.4 (adapter, keys) → 89% coverage
- **Week 3:** Phase 5.5 + polish → 90%+ coverage

---

## Risk Assessment

### High Risk 🔴

**decoder.rs complexity:**
- Symphonia API complex to mock
- Many codec-specific branches
- **Mitigation:** Start with trait abstraction, incremental testing

### Medium Risk 🟡

**pipeline.rs integration:**
- Multiple components interact
- Async coordination complexity
- **Mitigation:** Integration-style tests with real components where possible

### Low Risk 🟢

**retry.rs, keys.rs:**
- Well-defined interfaces
- Isolated functionality
- **Mitigation:** Standard mock patterns

---

## Next Steps

1. **Review this plan** ✅
2. **Start Phase 5.1** (decoder.rs)
3. **Create trait abstraction**
4. **Add first 10 tests**
5. **Iterate until 85%+ coverage**
6. **Move to Phase 5.2**

---

## Notes

- **Incremental commits:** One commit per phase completion
- **Coverage measurement:** After each phase
- **Test quality:** rstest + mocks + timeout mandatory
- **Documentation:** Update PHASE5_COMPLETE.md at the end

**Ready to start Phase 5.1: decoder.rs**
