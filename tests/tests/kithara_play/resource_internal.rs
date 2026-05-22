#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use kithara_audio::ReadOutcome;
use kithara_decode::PcmSpec;
use kithara_events::{AudioEvent, AudioFormat, Event, EventBus};
use kithara_integration_tests::audio_mock::TestPcmReader;
use kithara_platform::{time, time::Duration};
use kithara_play::Resource;
use kithara_test_utils::kithara;

fn mock_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 44100,
    }
}

fn make_resource() -> Resource {
    Resource::from_reader(TestPcmReader::new(mock_spec(), 1.0), None)
}

fn make_resource_with_bus() -> (Resource, EventBus) {
    let reader = TestPcmReader::new(mock_spec(), 1.0);
    let bus = reader.event_bus().clone();
    let resource = Resource::from_reader(reader, None);
    (resource, bus)
}

#[derive(Clone, Copy)]
enum ReadMode {
    Interleaved,
    Planar,
}

#[kithara::test(tokio)]
#[case(ReadMode::Interleaved)]
#[case(ReadMode::Planar)]
async fn test_resource_from_reader_read_variants(#[case] mode: ReadMode) {
    let mut resource = make_resource();
    match mode {
        ReadMode::Interleaved => {
            let mut buf = [0.0f32; 64];
            let outcome = resource.read(&mut buf).expect("BUG: read");
            let ReadOutcome::Frames { count, .. } = outcome else {
                panic!("expected Frames, got {outcome:?}");
            };
            assert_eq!(count.get(), 64);
            for sample in &buf[..count.get()] {
                assert!((sample - 0.5).abs() < f32::EPSILON);
            }
        }
        ReadMode::Planar => {
            let mut ch0 = [0.0f32; 32];
            let mut ch1 = [0.0f32; 32];
            let mut output: Vec<&mut [f32]> = vec![&mut ch0, &mut ch1];
            let outcome = resource.read_planar(&mut output).expect("BUG: read_planar");
            let ReadOutcome::Frames { count, .. } = outcome else {
                panic!("expected Frames, got {outcome:?}");
            };
            assert_eq!(count.get(), 32);
            for &s in &ch0[..count.get()] {
                assert!((s - 0.5).abs() < f32::EPSILON);
            }
            for &s in &ch1[..count.get()] {
                assert!((s - 0.5).abs() < f32::EPSILON);
            }
        }
    }
}

#[kithara::test(tokio)]
async fn test_resource_from_reader_spec() {
    let resource = make_resource();
    let spec = resource.spec();
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[kithara::test(tokio)]
async fn test_resource_from_reader_position_and_duration() {
    let resource = make_resource();
    assert_eq!(resource.position(), Duration::ZERO);
    let dur = resource.duration().unwrap();
    assert!((dur.as_secs_f64() - 1.0).abs() < 0.001);
}

#[kithara::test(tokio)]
async fn test_resource_from_reader_seek() {
    let mut resource = make_resource();
    assert_eq!(resource.position(), Duration::ZERO);

    let outcome = resource
        .seek(Duration::from_millis(500))
        .expect("BUG: seek");
    assert!(matches!(outcome, kithara_audio::SeekOutcome::Landed { .. }));
    let pos = resource.position();
    assert!((pos.as_secs_f64() - 0.5).abs() < 0.001);
}

#[kithara::test(tokio)]
async fn test_resource_from_reader_reads_until_eof() {
    let mut resource = make_resource();

    let mut buf = [0.0f32; 4096];
    let saw_eof = loop {
        match resource.read(&mut buf).expect("BUG: read") {
            ReadOutcome::Pending { .. } => break false,
            ReadOutcome::Frames { .. } => continue,
            ReadOutcome::Eof { .. } => break true,
        }
    };
    assert!(
        saw_eof,
        "reader must reach natural EOF after consuming all samples"
    );
}

#[kithara::test(tokio)]
async fn test_resource_subscribe_receives_events() {
    let (resource, bus) = make_resource_with_bus();
    let mut rx = resource.subscribe();

    let spec = mock_spec();
    let format = AudioFormat::new(spec.channels, spec.sample_rate);
    bus.publish(AudioEvent::FormatDetected { spec: format });

    let event = time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(event, Event::Audio(AudioEvent::FormatDetected { spec: s }) if s == format));
}

#[kithara::test(tokio)]
async fn test_resource_metadata() {
    let resource = make_resource();
    let meta = resource.metadata();
    assert_eq!(meta.title.as_deref(), Some("Mock"));
    assert!(meta.artwork.is_none());
}
