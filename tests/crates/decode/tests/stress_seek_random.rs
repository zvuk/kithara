use std::{fs::File as FsFile, io::Write};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    file::{File, FileConfig, FileSrc},
    platform::{time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    signal::AudioSpec,
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, Xorshift64,
    bufpool_ext::{TestPools, pools},
};
use kithara_test_fixtures::fixtures::stress_wav;
use tempfile::NamedTempFile;
use tracing::info;

#[derive(Default)]
struct SeekStats {
    successful_reads: u64,
    total_samples_read: u64,
    channel_mismatches: u64,
    zero_reads: u64,
}

fn run_seek_iterations(
    audio: &mut RegisteredAudio<Stream<File<TestPools>>, TestPools>,
    buf: &mut [f32],
    seek_positions: &[f64],
    spec: AudioSpec,
) -> SeekStats {
    let SeekStats {
        mut successful_reads,
        mut total_samples_read,
        mut channel_mismatches,
        mut zero_reads,
    } = SeekStats::default();

    for (i, &pos_secs) in seek_positions.iter().enumerate() {
        let position = Duration::from_secs_f64(pos_secs);

        audio.seek(position).unwrap_or_else(|e| {
            panic!("seek #{i} to {pos_secs:.4}s failed: {e}");
        });

        let read_count = |outcome: Result<ReadOutcome, _>| -> usize {
            match outcome {
                Ok(ReadOutcome::Frames { count, .. }) => count.get(),
                Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => 0,
                Err(e) => panic!("decode error during seek read: {e}"),
            }
        };
        let mut n = read_count(audio.read(buf));
        if n == 0 {
            audio.seek(position).unwrap_or_else(|e| {
                panic!("re-seek #{i} to {pos_secs:.4}s failed: {e}");
            });
            n = read_count(audio.read(buf));
            if n == 0 {
                zero_reads += 1;
                if zero_reads <= 3 {
                    tracing::warn!(iteration = i, pos_secs, "zero-read after retry (transient)");
                }
                continue;
            }
        }

        for (j, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "invalid sample at seek #{i} offset {j}: {sample} (pos {pos_secs:.4}s)",
            );
        }

        let channels = spec.channels as usize;
        if channels == 2 {
            let frames = n / channels;
            for f in 0..frames {
                let l = buf[f * 2];
                let r = buf[f * 2 + 1];
                if (l - r).abs() > f32::EPSILON {
                    channel_mismatches += 1;
                }
            }
        }

        successful_reads += 1;
        total_samples_read += n as u64;

        if (i + 1) % 200 == 0 {
            info!(
                iteration = i + 1,
                successful_reads, total_samples_read, channel_mismatches, "Progress"
            );
        }
    }

    SeekStats {
        successful_reads,
        total_samples_read,
        channel_mismatches,
        zero_reads,
    }
}

fn read_final_tail(
    audio: &mut RegisteredAudio<Stream<File<TestPools>>, TestPools>,
    buf: &mut [f32],
    final_seek_secs: f64,
) -> (u64, bool) {
    audio
        .seek(Duration::from_secs_f64(final_seek_secs))
        .unwrap_or_else(|e| {
            panic!("final seek to {final_seek_secs:.4}s failed: {e}");
        });

    let mut remaining_samples = 0u64;
    let mut saw_final_eof = false;
    loop {
        match audio.read(buf) {
            Ok(ReadOutcome::Pending { .. }) => break,
            Ok(ReadOutcome::Frames { count, .. }) => {
                remaining_samples += count.get() as u64;
                for &sample in &buf[..count.get()] {
                    assert!(
                        sample.is_finite() && (-1.0..=1.0).contains(&sample),
                        "invalid sample in final tail read",
                    );
                }
            }
            Ok(ReadOutcome::Eof { .. }) => {
                saw_final_eof = true;
                break;
            }
            Err(e) => panic!("decode error in final tail read: {e}"),
        }
    }

    (remaining_samples, saw_final_eof)
}

#[kithara::test(
    native,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_stream=debug")
)]
async fn stress_random_seek_read_synthetic_wav(#[future(awt)] wav_file: NamedTempFile) {
    const DURATION_SECS_INT: u32 = 10;
    const DURATION_SECS: f64 = DURATION_SECS_INT as f64;
    const SEEK_ITERATIONS: usize = 1000;

    let cache = TestTempDir::new();
    let pools = pools();
    let file_config = FileConfig::for_src(FileSrc::Local(wav_file.path().to_path_buf()))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: cache.path().into(),
                })
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint("wav".to_string())
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker.open(config).await.expect("create audio pipeline");

    let total_duration = audio.duration().expect("WAV should report known duration");
    let total_secs = total_duration.as_secs_f64();
    info!(total_secs, "Stream duration");

    assert!(
        (total_secs - DURATION_SECS).abs() < 0.1,
        "duration mismatch: expected ~{DURATION_SECS}, got {total_secs}",
    );

    let spec = audio.spec();
    info!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        "Audio spec"
    );

    let chunk_duration_secs = (total_secs * 0.005).clamp(0.05, 0.5);
    let chunk_samples = num_traits::cast::<f64, usize>(
        chunk_duration_secs * f64::from(spec.sample_rate.get()) * f64::from(spec.channels),
    )
    .unwrap_or(usize::MAX);
    info!(chunk_duration_secs, chunk_samples, "Read chunk size");

    let result = spawn_blocking(move || {
        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0.0f32; chunk_samples];

        let max_seek_secs = total_secs - chunk_duration_secs;
        assert!(max_seek_secs > 0.0, "stream too short for chunk size");

        let seek_positions: Vec<f64> = (0..SEEK_ITERATIONS)
            .map(|_| rng.range_f64(0.001, max_seek_secs))
            .collect();

        info!(
            count = seek_positions.len(),
            max_seek_secs, "Generated seek positions"
        );

        let SeekStats {
            successful_reads,
            total_samples_read,
            channel_mismatches,
            zero_reads,
        } = run_seek_iterations(&mut audio, &mut buf, &seek_positions, spec);

        info!(
            successful_reads,
            total_samples_read,
            channel_mismatches,
            zero_reads,
            "All {} seek+read iterations done",
            SEEK_ITERATIONS
        );

        if zero_reads > 0 {
            tracing::warn!(zero_reads, "zero-reads detected (within tolerance of 3)");
        }
        assert!(
            zero_reads <= 3,
            "{} zero-reads out of {} (>3 tolerance) — decoder EOF race",
            zero_reads,
            SEEK_ITERATIONS
        );
        assert!(
            successful_reads >= SEEK_ITERATIONS as u64 - 3,
            "only {} successful reads out of {}",
            successful_reads,
            SEEK_ITERATIONS
        );
        assert_eq!(
            channel_mismatches, 0,
            "L/R channel data diverged {channel_mismatches} times — data corruption"
        );

        let final_seek_secs = total_secs - chunk_duration_secs;
        info!(final_seek_secs, "Final seek near end");

        let (remaining_samples, saw_final_eof) =
            read_final_tail(&mut audio, &mut buf, final_seek_secs);

        assert!(
            saw_final_eof,
            "expected EOF after reading all remaining data from {final_seek_secs:.4}s"
        );

        info!(remaining_samples, "Final read done — EOF confirmed");
    })
    .await;

    result.expect("spawn_blocking failed");
    info!("Stress test passed");
}

#[kithara::fixture]
async fn wav_file() -> NamedTempFile {
    spawn_blocking(|| {
        let file = NamedTempFile::new().expect("create temp file");
        Write::write_all(
            &mut FsFile::create(file.path()).expect("open temp file"),
            stress_wav(),
        )
        .expect("write prepared WAV data");
        file
    })
    .await
    .expect("prepare WAV file")
}
