#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
    thread,
    time::{Instant, SystemTime},
};

use super::consts;

struct Heartbeat {
    path: PathBuf,
}

impl Heartbeat {
    fn start(lease: &Path) -> io::Result<Self> {
        let parent = lease
            .parent()
            .ok_or_else(|| io::Error::other("lease path has no build directory"))?;
        let heartbeat = Self {
            path: parent.join(consts::HEARTBEAT_FILE),
        };
        heartbeat.refresh()?;
        Ok(heartbeat)
    }

    fn refresh(&self) -> io::Result<()> {
        fs::write(&self.path, format!("pid={}\n", process::id()))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Wait for the host collector's exclusive lock without consuming command stdout.
fn hold_shared(file: &File, lease: &Path) -> io::Result<()> {
    let started = Instant::now();
    let mut announced: Option<Instant> = None;
    loop {
        match file.try_lock_shared() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
        if announced.is_none_or(|at| at.elapsed() >= consts::LOCK_ANNOUNCE) {
            eprintln!(
                "waiting for the CI build target lease {} ({} s so far): a cache cleanup holds it",
                lease.display(),
                started.elapsed().as_secs()
            );
            announced = Some(Instant::now());
        }
        thread::sleep(consts::LOCK_POLL);
    }
    if announced.is_some() {
        eprintln!(
            "took the CI build target lease {} after {} s",
            lease.display(),
            started.elapsed().as_secs()
        );
    }
    Ok(())
}

/// Lock and touch the live target, rechecking its identity after an eviction moves it.
fn take(directory: &Path, lease: &Path) -> io::Result<File> {
    loop {
        fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lease)?;
        hold_shared(&file, lease)?;
        if still_at(&file, lease)? {
            file.set_modified(SystemTime::now())?;
            return Ok(file);
        }
    }
}

/// Whether the open `file` is still the one at `path`.
#[cfg(unix)]
fn still_at(file: &File, path: &Path) -> io::Result<bool> {
    let held = file.metadata()?;
    match fs::metadata(path) {
        Ok(current) => Ok(current.dev() == held.dev() && current.ino() == held.ino()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn still_at(file: &File, path: &Path) -> io::Result<bool> {
    match File::open(path) {
        Ok(current) => Ok(windows::identity(file)? == windows::identity(&current)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, fs::File, io, mem::MaybeUninit, os::windows::io::AsRawHandle};

    use super::consts;

    #[repr(C)]
    #[derive(PartialEq, Eq)]
    pub(super) struct FileIdentity {
        volume: u64,
        id: [u8; 16],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFileInformationByHandleEx"]
        fn file_information(handle: *mut c_void, class: u32, output: *mut c_void, size: u32)
        -> i32;
    }

    pub(super) fn identity(file: &File) -> io::Result<FileIdentity> {
        let mut identity = MaybeUninit::<FileIdentity>::uninit();
        let size = u32::try_from(std::mem::size_of::<FileIdentity>()).map_err(io::Error::other)?;
        // SAFETY: FileIdInfo writes a FILE_ID_INFO into this buffer while the file holds the handle.
        let success = unsafe {
            file_information(
                file.as_raw_handle(),
                consts::FILE_ID_INFO,
                identity.as_mut_ptr().cast(),
                size,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: A successful FileIdInfo query initialized the complete FILE_ID_INFO.
        Ok(unsafe { identity.assume_init() })
    }
}

pub(super) fn run(directory: &Path, command: &mut Command) -> io::Result<ExitStatus> {
    let lease = directory.join(consts::LEASE_FILE);
    let _file = take(directory, &lease)?;
    let heartbeat = Heartbeat::start(&lease)?;
    let mut child = command.spawn()?;
    let mut refresh_at = Instant::now() + consts::HEARTBEAT_INTERVAL;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if Instant::now() >= refresh_at {
            if let Err(error) = heartbeat.refresh() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            refresh_at = Instant::now() + consts::HEARTBEAT_INTERVAL;
        }
        thread::sleep(consts::POLL_INTERVAL);
    }
}
