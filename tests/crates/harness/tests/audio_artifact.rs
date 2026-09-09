use std::mem::size_of;

use kithara_integration_tests::{
    audio_artifact::{AssetReader, AudioArtifactSet, ReadSide, audio_artifact_path},
    bufpool_ext::TestPools,
};
use kithara_test_utils::kithara;
use tempfile::tempdir;

fn read(reader: &AssetReader<TestPools>) -> Vec<u8> {
    let len = reader.len().expect("committed artifact length");
    let mut bytes = vec![0_u8; usize::try_from(len).expect("artifact length fits usize")];
    let read = reader.read_at(0, &mut bytes).expect("read audio artifact");
    assert_eq!(read, bytes.len());
    bytes
}

#[kithara::test(native, flash(false))]
fn recording_core_writes_float_wav_through_disk_assets() {
    let temp = tempdir().expect("temporary artifact directory");
    let set = AudioArtifactSet::new(temp.path(), "header", 48_000, 2).expect("audio artifact set");
    let mut recording = set.recording("capture", Some(2)).expect("recording");
    recording
        .push(&[0.25, -0.25, 0.5, -0.5])
        .expect("record samples");
    let reader = AudioArtifactSet::finish(recording).expect("finish recording");
    let bytes = read(&reader);

    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3);
    assert_eq!(bytes.len(), 44 + 4 * size_of::<f32>());
    assert!(
        audio_artifact_path(&reader)
            .expect("artifact path")
            .is_absolute()
    );
}

#[kithara::test(native, flash(false))]
fn recording_core_preserves_payload_across_packet_boundary() {
    let temp = tempdir().expect("temporary artifact directory");
    let set = AudioArtifactSet::new(temp.path(), "packets", 48_000, 1).expect("audio artifact set");
    let samples = (0..1_026)
        .map(|index| {
            f32::from_bits(0x3e80_0000 + u32::try_from(index).expect("sample index fits u32"))
        })
        .collect::<Vec<_>>();
    let mut recording = set.recording("batched", Some(1_026)).expect("recording");
    recording.push(&samples).expect("record samples");
    let bytes = read(&AudioArtifactSet::finish(recording).expect("finish recording"));
    let expected = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect::<Vec<_>>();

    assert_eq!(&bytes[44..], expected);
}
