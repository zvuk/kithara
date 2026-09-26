//! Synthetic fragmented-mp4 bytes the walk tests read.
//!
//! Hand-built rather than muxed: the walks under test are the only thing
//! that should have to parse these bytes, and a fixture that needs an
//! encoder would drag a codec graph into a crate whose whole point is not
//! having one.

use std::{
    io,
    io::Error,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{ReadAt, consts};

/// Payload bytes of one fragment's `mdat`: every sample, back to back.
pub(crate) fn mdat_bytes() -> usize {
    usize::try_from(consts::SAMPLES_PER_FRAGMENT * consts::SAMPLE_BYTES)
        .expect("test mdat fits usize")
}

fn mp4_box(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("test box fits u32");
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(payload);
    bytes
}

fn full_box(name: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version];
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    mp4_box(name, &body)
}

fn matrix() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(36);
    for value in [0x0001_0000_i32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes
}

/// An unknown sample entry works in this fixture, since the timescale is read from `mdhd`; the
/// codec box itself never has to be decodable.
fn init_segment() -> Vec<u8> {
    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&consts::TIMESCALE.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 10]);
    mvhd.extend_from_slice(&matrix());
    mvhd.extend_from_slice(&[0u8; 24]);
    mvhd.extend_from_slice(&2u32.to_be_bytes());

    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&consts::TRACK_ID.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]);
    tkhd.extend_from_slice(&0u16.to_be_bytes());
    tkhd.extend_from_slice(&0u16.to_be_bytes());
    tkhd.extend_from_slice(&0x0100u16.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 2]);
    tkhd.extend_from_slice(&matrix());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());

    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&consts::TIMESCALE.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0x55c4u16.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 2]);

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0u8; 4]);
    hdlr.extend_from_slice(b"soun");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.push(0);

    let dref = full_box(b"dref", 0, 0, &0u32.to_be_bytes());
    let dinf = mp4_box(b"dinf", &dref);

    let mut stsd = Vec::new();
    stsd.extend_from_slice(&1u32.to_be_bytes());
    stsd.extend_from_slice(&mp4_box(b"kthx", &[]));
    let stsd = full_box(b"stsd", 0, 0, &stsd);
    let stts = full_box(b"stts", 0, 0, &0u32.to_be_bytes());
    let stsc = full_box(b"stsc", 0, 0, &0u32.to_be_bytes());
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&0u32.to_be_bytes());
    let stsz = full_box(b"stsz", 0, 0, &stsz);
    let stco = full_box(b"stco", 0, 0, &0u32.to_be_bytes());

    let mut stbl = stsd;
    stbl.extend_from_slice(&stts);
    stbl.extend_from_slice(&stsc);
    stbl.extend_from_slice(&stsz);
    stbl.extend_from_slice(&stco);
    let stbl = mp4_box(b"stbl", &stbl);

    let mut minf = dinf;
    minf.extend_from_slice(&stbl);
    let minf = mp4_box(b"minf", &minf);

    let mut mdia = full_box(b"mdhd", 0, 0, &mdhd);
    mdia.extend_from_slice(&full_box(b"hdlr", 0, 0, &hdlr));
    mdia.extend_from_slice(&minf);
    let mdia = mp4_box(b"mdia", &mdia);

    let mut trak = full_box(b"tkhd", 0, 3, &tkhd);
    trak.extend_from_slice(&mdia);
    let trak = mp4_box(b"trak", &trak);

    let mut moov = full_box(b"mvhd", 0, 0, &mvhd);
    moov.extend_from_slice(&trak);
    let moov = mp4_box(b"moov", &moov);

    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso5");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"dash");
    let mut bytes = mp4_box(b"ftyp", &ftyp);
    bytes.extend_from_slice(&moov);
    bytes
}

fn moof_box(index: u32, data_offset: i32) -> Vec<u8> {
    /// `tfhd` flags: default sample duration and size, and base-is-moof so a
    /// `trun` offset counts from the `moof` header rather than the file.
    const TFHD_FLAGS: u32 = 0x08 | 0x10 | 0x0002_0000;

    /// `trun` flags: the box carries an explicit data offset.
    const TRUN_FLAGS: u32 = 0x01;

    let mfhd = full_box(b"mfhd", 0, 0, &(index + 1).to_be_bytes());

    let mut tfhd = Vec::new();
    tfhd.extend_from_slice(&consts::TRACK_ID.to_be_bytes());
    tfhd.extend_from_slice(&consts::SAMPLE_TICKS.to_be_bytes());
    tfhd.extend_from_slice(&consts::SAMPLE_BYTES.to_be_bytes());
    let tfhd = full_box(b"tfhd", 0, TFHD_FLAGS, &tfhd);

    let decode_time = u64::from(index)
        * u64::from(consts::SAMPLES_PER_FRAGMENT)
        * u64::from(consts::SAMPLE_TICKS);
    let tfdt = full_box(b"tfdt", 1, 0, &decode_time.to_be_bytes());

    let mut trun = Vec::new();
    trun.extend_from_slice(&consts::SAMPLES_PER_FRAGMENT.to_be_bytes());
    trun.extend_from_slice(&data_offset.to_be_bytes());
    let trun = full_box(b"trun", 0, TRUN_FLAGS, &trun);

    let mut traf = tfhd;
    traf.extend_from_slice(&tfdt);
    traf.extend_from_slice(&trun);
    let traf = mp4_box(b"traf", &traf);

    let mut moof = mfhd;
    moof.extend_from_slice(&traf);
    mp4_box(b"moof", &moof)
}

/// One `moof` + `mdat` pair. The `trun` data offset is measured against the
/// finished `moof`, so the samples it addresses land on the `mdat` payload
/// the way a real muxer writes them.
pub(crate) fn media_segment(index: u32) -> Vec<u8> {
    let probe_len = moof_box(index, 0).len();
    let data_offset = i32::try_from(probe_len + 8).expect("test moof fits the trun data offset");
    let mut bytes = moof_box(index, data_offset);
    debug_assert_eq!(bytes.len(), probe_len, "data offset changed the moof size");
    bytes.extend_from_slice(&mp4_box(b"mdat", &vec![index_fill(index); mdat_bytes()]));
    bytes
}

fn index_fill(index: u32) -> u8 {
    u8::try_from(index % 251).unwrap_or(0)
}

/// Fragmented-mp4 bytes plus the offset at which the first `moof` starts.
pub(crate) fn fragmented_mp4() -> (Vec<u8>, u64) {
    let mut bytes = init_segment();
    let first_moof = u64::try_from(bytes.len()).expect("test init fits u64");
    for index in 0..consts::FRAGMENTS {
        bytes.extend_from_slice(&media_segment(index));
    }
    (bytes, first_moof)
}

/// Byte source that counts every byte a walk pulls out of it.
pub(crate) struct CountingSource {
    delivered: AtomicU64,
    bytes: Vec<u8>,
}

impl CountingSource {
    pub(crate) const fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            delivered: AtomicU64::new(0),
        }
    }

    /// Bytes handed out so far.
    pub(crate) fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }

    /// Length of the body, as the walks want it.
    pub(crate) fn total(&self) -> u64 {
        u64::try_from(self.bytes.len()).expect("test body fits u64")
    }
}

impl ReadAt for CountingSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(Error::other)?;
        let Some(tail) = self.bytes.get(start..) else {
            return Ok(0);
        };
        let n = tail.len().min(buf.len());
        buf[..n].copy_from_slice(&tail[..n]);
        self.delivered
            .fetch_add(u64::try_from(n).map_err(Error::other)?, Ordering::Relaxed);
        Ok(n)
    }
}
