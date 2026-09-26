use std::{
    env,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

mod consts {
    use super::Duration;

    pub(super) const HEARTBEAT_FILE: &str = ".kithara-job-heartbeat";
    // Refresh far faster than the five-minute host cleanup interval. Polling the
    // child at 100 ms keeps the wrapper's exit latency below a measurable CI phase.
    pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
    pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(100);
}

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

fn main() {
    match run() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("failed to hold the CI build target: {error}");
            process::exit(1);
        }
    }
}

fn run() -> io::Result<ExitStatus> {
    let mut args = env::args_os().skip(1);
    let lease = PathBuf::from(
        args.next()
            .ok_or_else(|| io::Error::other("missing lease path"))?,
    );
    let command = args
        .next()
        .ok_or_else(|| io::Error::other("missing command"))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lease)?;
    file.lock_shared()?;
    let heartbeat = Heartbeat::start(&lease)?;
    let _ = env::current_exe().and_then(fs::remove_file);
    let mut child = Command::new(command).args(args).spawn()?;
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
