use std::{num::NonZeroUsize, ops::Range};

use kithara_blob::{BlobError, Reader};
use kithara_key::{KeyEstimator, KeyResume, Mode, MusicalKey, PitchClass};
use kithara_test_utils::{
    bufpool::{Pools, pools},
    kithara,
};

use crate::recipe::{estimate_whole, estimator, frames, goldens, rendered};

#[kithara::test]
fn a_minor_progression_is_a_minor() {
    let estimate = estimate_whole(&rendered("a_minor"), goldens().sample_rate)
        .expect("a tonal track states a key");
    assert_eq!(
        estimate.key(),
        MusicalKey {
            tonic: PitchClass::A,
            mode: Mode::Minor,
        }
    );
}

#[kithara::test]
fn c_major_progression_is_c_major() {
    let estimate = estimate_whole(&rendered("c_major"), goldens().sample_rate)
        .expect("a tonal track states a key");
    assert_eq!(
        estimate.key(),
        MusicalKey {
            tonic: PitchClass::C,
            mode: Mode::Major,
        }
    );
}

#[kithara::test]
fn silence_states_no_key() {
    let silence = rendered("silence");
    assert!(silence.iter().all(|&sample| sample == 0.0));
    assert_eq!(estimate_whole(&silence, goldens().sample_rate), None);
}

const FRAME: usize = 4096;
const FRAME_LEAD: usize = 2048;
fn chunks(len: usize) -> Vec<Range<usize>> {
    const CHUNK_SIZES: [usize; 5] = [1000, 4096, 777, 10_000, 3333];

    let mut chunks = Vec::new();
    let mut start = 0;
    for size in CHUNK_SIZES.iter().cycle() {
        if start >= len {
            break;
        }
        let end = (start + size).min(len);
        chunks.push(start..end);
        start = end;
    }
    chunks
}

fn push(pools: &Pools, estimator: &mut KeyEstimator, pcm: &[f32], range: Range<usize>) {
    let at = u64::try_from(range.start).expect("the position fits u64");
    estimator
        .push(pools, &pcm[range], NonZeroUsize::MIN, at)
        .expect("the chunk fits the pools");
}

#[kithara::test]
fn shuffled_gapped_delivery_matches_in_order_delivery() {
    let pcm = rendered("e_minor");
    let rate = goldens().sample_rate;
    let kept: Vec<Range<usize>> = chunks(pcm.len())
        .into_iter()
        .enumerate()
        .filter(|(index, _)| index % 7 != 3)
        .map(|(_, range)| range)
        .collect();
    let pools = pools();

    let mut in_order = estimator(&pools, rate);
    for range in &kept {
        push(&pools, &mut in_order, &pcm, range.clone());
    }
    in_order.set_end(frames(&pcm)).expect("the tail closes");

    let mut shuffled = estimator(&pools, rate);
    let mut order: Vec<usize> = (0..kept.len()).collect();
    order.sort_by_key(|&index| (index * 7919) % kept.len());
    for (step, &index) in order.iter().enumerate() {
        if step == order.len() / 2 {
            shuffled.set_end(frames(&pcm)).expect("the tail closes");
        }
        push(&pools, &mut shuffled, &pcm, kept[index].clone());
    }

    let in_order = in_order.estimate().expect("a tonal track states a key");
    let shuffled = shuffled.estimate().expect("a tonal track states a key");
    assert_eq!(shuffled.key(), in_order.key());
    assert!((shuffled.strength() - in_order.strength()).abs() <= 1e-9);
}

#[kithara::test]
fn a_range_delivered_twice_changes_nothing() {
    let pcm = rendered("g_major");
    let rate = goldens().sample_rate;
    let pools = pools();

    let mut once = estimator(&pools, rate);
    let mut twice = estimator(&pools, rate);
    for range in chunks(pcm.len()) {
        push(&pools, &mut once, &pcm, range.clone());
        push(&pools, &mut twice, &pcm, range.clone());
        push(&pools, &mut twice, &pcm, range);
    }
    once.set_end(frames(&pcm)).expect("the tail closes");
    twice.set_end(frames(&pcm)).expect("the tail closes");
    push(&pools, &mut twice, &pcm, 50_000..70_000);

    assert!(once.estimate().is_some());
    assert_eq!(twice.estimate(), once.estimate());
}

#[kithara::test]
fn tail_frames_count_once_the_end_is_stated() {
    let pcm = rendered("f_minor");
    let rate = goldens().sample_rate;
    let last_start = (pcm.len() - 1 + FRAME_LEAD) / FRAME * FRAME - FRAME_LEAD;
    assert!(
        last_start + FRAME > pcm.len(),
        "the last frame reaches past the end"
    );
    let pools = pools();

    let mut open = estimator(&pools, rate);
    push(&pools, &mut open, &pcm, 0..pcm.len());
    let mut head = estimator(&pools, rate);
    push(&pools, &mut head, &pcm, 0..last_start);
    assert_eq!(
        open.estimate(),
        head.estimate(),
        "the last frame waits for the end"
    );

    open.set_end(frames(&pcm)).expect("the tail closes");
    let mut padded_pcm = pcm.clone();
    padded_pcm.resize(last_start + FRAME, 0.0);
    let mut padded = estimator(&pools, rate);
    push(&pools, &mut padded, &padded_pcm, 0..padded_pcm.len());
    padded
        .set_end(frames(&padded_pcm))
        .expect("the tail closes");

    assert_eq!(
        open.estimate(),
        padded.estimate(),
        "the last frame is zero-padded"
    );
    assert_ne!(open.estimate(), head.estimate());
}

#[kithara::test]
fn a_restored_estimator_continues_where_it_stopped() {
    let pcm = rendered("c_sharp_minor");
    let rate = goldens().sample_rate;
    let stop = 100_000;
    assert_ne!(
        (stop + FRAME_LEAD) % FRAME,
        0,
        "the store falls inside a frame"
    );
    let pools = pools();

    let mut whole = estimator(&pools, rate);
    whole.set_end(frames(&pcm)).expect("no frame is open yet");
    push(&pools, &mut whole, &pcm, 0..stop);
    push(&pools, &mut whole, &pcm, stop..pcm.len());

    let mut first = estimator(&pools, rate);
    first.set_end(frames(&pcm)).expect("no frame is open yet");
    push(&pools, &mut first, &pcm, 0..stop);
    let mut bytes = Vec::new();
    first.write_state(&mut bytes);
    drop(first);

    let mut resumed = estimator(&pools, rate);
    resumed
        .restore_state(&pools, decode(&bytes).expect("the state decodes"))
        .expect("the state fits the pools");
    push(&pools, &mut resumed, &pcm, stop..pcm.len());
    push(&pools, &mut resumed, &pcm, 20_000..60_000);

    assert!(whole.estimate().is_some());
    assert_eq!(resumed.estimate(), whole.estimate());
}

fn decode(bytes: &[u8]) -> Result<KeyResume, BlobError> {
    let mut reader = Reader::new(bytes);
    let resume = KeyResume::decode(&mut reader)?;
    reader.finish()?;
    Ok(resume)
}

#[kithara::test]
fn state_of_another_version_is_refused() {
    let pcm = rendered("c_sharp_minor");
    let rate = goldens().sample_rate;
    let pools = pools();
    let mut estimator = estimator(&pools, rate);
    push(&pools, &mut estimator, &pcm, 0..10_000);
    let mut bytes = Vec::new();
    estimator.write_state(&mut bytes);
    assert!(bytes.len() > 4, "the state follows its version");
    bytes[0] ^= 1;

    assert!(matches!(decode(&bytes), Err(BlobError::Version { .. })));
}
