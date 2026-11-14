use std::{
    io,
    path::{Path, PathBuf},
};

use kithara_platform::{
    CancelToken,
    tokio::{runtime::Handle, task},
};
use kithara_ui::render::ReadValue;

use crate::job::Job;

type Pending = Job<io::Result<bool>>;

pub(crate) struct Streaming {
    pub(crate) adaptive: bool,
    path: Option<PathBuf>,
    state: State,
    fault: String,
}

enum State {
    Loading(Pending),
    Ready,
    Saving(Pending),
}

impl Streaming {
    pub(crate) fn new(runtime: &Handle, cancel: &CancelToken, path: Option<PathBuf>) -> Self {
        let state = path.as_ref().map_or(State::Ready, |path| {
            State::Loading(access(runtime, cancel, path.clone(), None))
        });
        Self {
            adaptive: true,
            path,
            state,
            fault: String::new(),
        }
    }

    pub(crate) fn loaded(&self) -> bool {
        !matches!(self.state, State::Loading(_))
    }

    pub(crate) fn read(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        Some(match endpoint {
            "adaptive_streaming" => ReadValue::Bool(self.adaptive),
            "streaming_idle" => ReadValue::Bool(matches!(self.state, State::Ready)),
            "streaming_pending" => ReadValue::Bool(!matches!(self.state, State::Ready)),
            "streaming_fault" => ReadValue::Text(&self.fault),
            "streaming_fault_hidden" => ReadValue::Bool(self.fault.is_empty()),
            _ => return None,
        })
    }

    pub(crate) fn set(&mut self, value: bool, runtime: &Handle, cancel: &CancelToken) {
        if !matches!(self.state, State::Ready) || value == self.adaptive {
            return;
        }
        if let Some(path) = &self.path {
            self.state = State::Saving(access(runtime, cancel, path.clone(), Some(value)));
        } else {
            self.adaptive = value;
        }
    }

    pub(crate) fn tick(&mut self) {
        let job = match &mut self.state {
            State::Loading(job) | State::Saving(job) => job,
            State::Ready => return,
        };
        let Some(result) = job.finished() else {
            return;
        };
        self.state = State::Ready;
        match result {
            Ok(value) => {
                self.adaptive = value;
                self.fault.clear();
            }
            Err(error) => self.fault = format!("Streaming settings: {error}"),
        }
    }
}

fn access(runtime: &Handle, cancel: &CancelToken, path: PathBuf, value: Option<bool>) -> Pending {
    let blocking_runtime = runtime.clone();
    Job::spawn(runtime, cancel, async move {
        task::spawn_blocking_on(&blocking_runtime, move || persisted(&path, value))
            .await
            .map_err(io::Error::other)?
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn persisted(path: &Path, value: Option<bool>) -> io::Result<bool> {
    if let Some(value) = value {
        let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), &value)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(value)
    } else {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected a boolean streaming preference",
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn persisted(_: &Path, _: Option<bool>) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::{time::Duration, tokio::runtime::Builder};
    use kithara_test_utils::{TestTempDir, cancel_token, kithara, wait_until};

    use super::*;

    async fn settled(settings: &mut Streaming) {
        wait_until(
            Duration::from_secs(2),
            "streaming preference settles",
            || {
                settings.tick();
                matches!(settings.state, State::Ready)
            },
        )
        .await
        .expect("preference job must finish");
    }

    #[kithara::test(native, flash(false))]
    async fn the_preference_survives_restart_and_failed_saves_keep_accepted_state() {
        let dir = TestTempDir::new();
        let path = dir.path().join("streaming.json");
        let cancel = cancel_token();
        let runtime = Handle::current();
        let mut settings = Streaming::new(&runtime, &cancel, Some(path.clone()));
        settled(&mut settings).await;
        assert!(settings.adaptive);
        assert!(!path.exists());
        settings.set(false, &runtime, &cancel);
        assert!(
            settings.adaptive,
            "pending writes do not change the accepted value"
        );
        settled(&mut settings).await;
        assert!(!settings.adaptive);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "false");
        let mut restarted = Streaming::new(&runtime, &cancel, Some(path.clone()));
        settled(&mut restarted).await;
        assert!(!restarted.adaptive);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        restarted.set(true, &runtime, &cancel);
        settled(&mut restarted).await;
        assert!(!restarted.adaptive);
        assert!(!restarted.fault.is_empty());
        assert!(path.is_dir());
    }

    #[kithara::test(native, flash(false))]
    async fn a_malformed_preference_is_reported_without_overwriting_it() {
        let dir = TestTempDir::new();
        let path = dir.path().join("streaming.json");
        std::fs::write(&path, "not a boolean").unwrap();
        let mut settings = Streaming::new(&Handle::current(), &cancel_token(), Some(path.clone()));
        settled(&mut settings).await;
        assert!(settings.adaptive);
        assert!(!settings.fault.is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not a boolean");
    }

    #[kithara::test(native, flash(false))]
    fn a_cancelled_source_cannot_start_a_preference_write() {
        let directory = TestTempDir::new();
        let path = directory.path().join("streaming.json");
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let cancel = cancel_token();
            let mut settings = Streaming::new(runtime.handle(), &cancel, Some(path.clone()));
            settled(&mut settings).await;
            cancel.cancel();
            settings.set(false, runtime.handle(), &cancel);
        });
        drop(runtime);
        assert!(
            !path.exists(),
            "cancelled work must not create the preference file"
        );
    }
}
