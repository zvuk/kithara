#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use kithara_storage::{MemResource, ResourceExt};
use libfuzzer_sys::fuzz_target;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
enum Op {
    Write { offset: u16, data: Vec<u8> },
    Read { offset: u16, len: u8 },
    Commit,
}

#[derive(Debug)]
struct Input {
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
        let mut ops = u.arbitrary::<Vec<Op>>()?;
        ops.truncate(64);
        Ok(Self { ops })
    }
}

fuzz_target!(|input: Input| {
    let res = MemResource::new(CancellationToken::new());
    let mut oracle = vec![0u8; 8192];
    let mut written_len = 0usize;
    let mut committed = false;

    for op in input.ops {
        match op {
            Op::Write { offset, data } => {
                let offset = usize::from(offset);
                let _ = res.write_at(offset as u64, &data);
                if !committed {
                    let end = offset.saturating_add(data.len()).min(oracle.len());
                    if end > offset {
                        let copied = end - offset;
                        oracle[offset..end].copy_from_slice(&data[..copied]);
                        written_len = written_len.max(end);
                    }
                }
            }
            Op::Read { offset, len } => {
                let offset = usize::from(offset);
                let mut buf = vec![0u8; usize::from(len)];
                let read = res.read_at(offset as u64, &mut buf).unwrap_or(0);
                let expected = written_len.saturating_sub(offset).min(buf.len());
                assert!(read <= expected);
                if read > 0 {
                    let end = offset + read;
                    assert_eq!(&buf[..read], &oracle[offset..end]);
                }
            }
            Op::Commit => {
                let _ = res.commit(Some(written_len as u64));
                committed = true;
            }
        }
    }
});
