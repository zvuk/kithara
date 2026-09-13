use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

const STEREO_CHANNELS: usize = 2;

pub struct MasterRing;

impl MasterRing {
    #[must_use]
    pub fn open(block_frames: u32, capacity_blocks: usize) -> (RingWriter, RingReader) {
        assert!(block_frames > 0, "invariant: ring block size is non-zero");
        assert!(capacity_blocks > 0, "invariant: ring capacity is non-zero");
        let block_samples = (block_frames as usize)
            .checked_mul(STEREO_CHANNELS)
            .unwrap_or_else(|| panic!("invariant: ring block sample count fits usize"));
        let capacity_samples = block_samples
            .checked_mul(capacity_blocks)
            .unwrap_or_else(|| panic!("invariant: ring capacity fits usize"));
        let (producer, consumer) = HeapRb::<f32>::new(capacity_samples).split();
        (
            RingWriter {
                block_frames,
                block_samples,
                producer,
                staging: vec![0.0; block_samples],
            },
            RingReader { consumer },
        )
    }
}

pub struct RingWriter {
    block_frames: u32,
    block_samples: usize,
    producer: HeapProd<f32>,
    staging: Vec<f32>,
}

impl RingWriter {
    pub(crate) const fn block_frames(&self) -> u32 {
        self.block_frames
    }

    pub fn reserve(&mut self, block_frames: u32) -> Option<ReservedBlock<'_>> {
        assert_eq!(
            block_frames, self.block_frames,
            "invariant: master ring accepts only its configured block size"
        );
        if self.producer.vacant_len() < self.block_samples {
            return None;
        }
        self.staging.fill(0.0);
        Some(ReservedBlock { writer: self })
    }
}

pub struct ReservedBlock<'a> {
    writer: &'a mut RingWriter,
}

impl ReservedBlock<'_> {
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.writer.staging
    }

    pub fn commit(self) {
        let Self { writer } = self;
        let written = writer.producer.push_slice(&writer.staging);
        assert_eq!(
            written,
            writer.staging.len(),
            "invariant: a reserved master-ring block commits in full"
        );
    }
}

pub struct RingReader {
    consumer: HeapCons<f32>,
}

impl RingReader {
    #[must_use]
    pub fn drain(&mut self, frames: usize) -> Vec<f32> {
        let requested = frames
            .checked_mul(STEREO_CHANNELS)
            .unwrap_or_else(|| panic!("invariant: drain sample count fits usize"));
        let mut samples = vec![0.0; requested.min(self.consumer.occupied_len())];
        let drained = self.consumer.pop_slice(&mut samples);
        samples.truncate(drained);
        samples
    }
}
