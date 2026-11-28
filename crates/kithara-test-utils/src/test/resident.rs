//! Residency of the test binary's code before a time budget starts.

use std::{fs, io, path::Path, sync::Once};

/// Pages every executable mapping of the running test binary into memory,
/// once per process, and returns when they are resident.
///
/// A time budget measures the work of a test body. Loading the binary's code
/// from disk belongs to starting the process, like its spawn; a cold page
/// cache would otherwise charge it to whichever tests run first in the binary.
///
/// # Panics
/// Panics when the binary's mappings cannot be read or populated: the budget
/// would then measure the disk again, so the harness refuses to go on.
pub fn make_binary_resident() {
    static RESIDENT: Once = Once::new();
    RESIDENT.call_once(|| {
        populate_executable_mappings()
            .unwrap_or_else(|error| panic!("test binary code could not be made resident: {error}"));
    });
}

fn populate_executable_mappings() -> io::Result<()> {
    let binary = fs::read_link("/proc/self/exe")?;
    let maps = fs::read_to_string("/proc/self/maps")?;
    for (start, end) in executable_ranges(&maps, &binary)? {
        // SAFETY: the range is a live mapping of this process taken from
        // `/proc/self/maps`; `MADV_POPULATE_READ` only faults its pages in and
        // never changes their contents or protection.
        let status = unsafe {
            libc::madvise(
                start as *mut libc::c_void,
                end - start,
                libc::MADV_POPULATE_READ,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Address ranges of the executable mappings backed by `binary`.
fn executable_ranges(maps: &str, binary: &Path) -> io::Result<Vec<(usize, usize)>> {
    let binary = binary.to_string_lossy();
    let mut ranges = Vec::new();
    for line in maps.lines() {
        let mut fields = line.splitn(6, ' ');
        let (Some(range), Some(perms)) = (fields.next(), fields.next()) else {
            return Err(malformed(line));
        };
        let path = fields.nth(3).map_or("", str::trim_start);
        if !perms.contains('x') || path != binary {
            continue;
        }
        let (start, end) = range.split_once('-').ok_or_else(|| malformed(line))?;
        let parse = |hex| usize::from_str_radix(hex, 16).map_err(|_| malformed(line));
        ranges.push((parse(start)?, parse(end)?));
    }
    if ranges.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no executable mapping of {binary} in /proc/self/maps"),
        ));
    }
    Ok(ranges)
}

fn malformed(line: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unreadable /proc/self/maps line: {line}"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::executable_ranges;
    use crate::kithara;

    const MAPS: &str = "\
55d0a0000000-55d0a0100000 r--p 00000000 103:02 42  /work/target/deps/suite-1
55d0a0100000-55d0a0900000 r-xp 00100000 103:02 42  /work/target/deps/suite-1
7f0000000000-7f0000100000 r-xp 00000000 103:02 7   /usr/lib/libc.so.6
7ffc00000000-7ffc00021000 rw-p 00000000 00:00 0    [stack]
";

    #[kithara::test]
    fn only_the_binary_s_executable_mappings_are_selected() {
        let ranges = executable_ranges(MAPS, Path::new("/work/target/deps/suite-1"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(ranges, [(0x55d0_a010_0000, 0x55d0_a090_0000)]);
    }

    #[kithara::test]
    fn a_binary_without_an_executable_mapping_is_an_error() {
        assert!(executable_ranges(MAPS, Path::new("/elsewhere")).is_err());
    }
}
