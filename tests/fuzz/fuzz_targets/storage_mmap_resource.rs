#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use kithara_storage::{MmapOptions, MmapResource, OpenMode, ResourceExt};
use libfuzzer_sys::fuzz_target;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
enum Op {
    Write { offset: u16, data: Vec<u8> },
    Read { offset: u16, len: u8 },
    Commit,
}

#[derive(Debug)]
struct Input {
    initial_len: u16,
    ops: Vec<Op>,
}

impl<'a> Arbitrary<'a> for Op {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        match u.int_in_range::<u8>(0..=2)? {
            0 => {
                let offset = u.int_in_range::<u16>(0..=4096)?;
                let mut data = u.arbitrary::<Vec<u8>>()?;
                data.truncate(256);
                Ok(Self::Write { offset, data })
            }
            1 => Ok(Self::Read {
                offset: u.int_in_range::<u16>(0..=4096)?,
                len: u.int_in_range::<u8>(0..=128)?,
            }),
            _ => Ok(Self::Commit),
        }
    }
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let initial_len = u.int_in_range::<u16>(0..=2048)?;
        let mut ops = u.arbitrary::<Vec<Op>>()?;
        ops.truncate(64);
        Ok(Self { initial_len, ops })
    }
}

fuzz_target!(|input: Input| {
    let dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(_) => return,
    };

    let path = dir.path().join("mmap_fuzz.bin");
    let res = match MmapResource::open(
        CancellationToken::new(),
        MmapOptions {
            path,
            initial_len: Some(u64::from(input.initial_len)),
            mode: OpenMode::Auto,
        },
    ) {
        Ok(res) => res,
        Err(_) => return,
    };

    for op in input.ops {
        match op {
            Op::Write { offset, data } => {
                let _ = res.write_at(u64::from(offset), &data);
            }
            Op::Read { offset, len } => {
                let mut buf = vec![0u8; usize::from(len)];
                let _ = res.read_at(u64::from(offset), &mut buf);
            }
            Op::Commit => {
                let _ = res.commit(None);
            }
        }
    }
});
