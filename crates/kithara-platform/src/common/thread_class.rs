use std::io;

/// How the OS schedules a thread against everything else on the machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThreadClass {
    /// The class the OS gives every new thread.
    #[default]
    Normal,
    /// A thread that keeps an audio output buffer filled ahead of the device.
    ///
    /// The buffer drains while the thread waits for a CPU, and an ordinary
    /// thread can wait longer than the buffer lasts once the machine is
    /// oversubscribed. The class puts it ahead of ordinary work, at the
    /// priority each platform gives its own media playback threads.
    AudioFeed,
}

/// Ask the OS to schedule the calling thread in `class`.
///
/// A thread calls this for itself as it starts. `Normal` requests nothing, so
/// it does not undo an earlier request.
///
/// `AudioFeed` is nice -16 on Linux and Android (Android's
/// `THREAD_PRIORITY_AUDIO`), the user-interactive quality-of-service class on
/// Apple platforms and the MMCSS "Audio" task on Windows. Browsers and the Miri
/// interpreter expose no thread scheduling, so on wasm and under Miri it requests
/// nothing.
///
/// # Errors
///
/// Returns the OS refusal. Linux lowers a thread's nice value only within the
/// process's `RLIMIT_NICE` or with `CAP_SYS_NICE`; a refused thread keeps the
/// class it had.
pub fn set_current_class(class: ThreadClass) -> io::Result<()> {
    match class {
        ThreadClass::Normal => Ok(()),
        ThreadClass::AudioFeed => os::audio_feed(),
    }
}

#[cfg(all(not(miri), any(target_os = "linux", target_os = "android")))]
mod os {
    use std::io;

    use crate::consts::AUDIO_FEED_NICE;

    pub(super) fn audio_feed() -> io::Result<()> {
        // SAFETY: `gettid` has no preconditions and cannot fail.
        let thread = unsafe { libc::gettid() };
        // SAFETY: on Linux a nice value belongs to one thread, so naming the
        // calling thread's id changes that thread alone.
        let status = unsafe {
            libc::setpriority(libc::PRIO_PROCESS, thread.cast_unsigned(), AUDIO_FEED_NICE)
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(all(not(miri), target_vendor = "apple"))]
mod os {
    use std::io;

    pub(super) fn audio_feed() -> io::Result<()> {
        // SAFETY: the call changes only the calling thread's own QoS class.
        let status = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }
}

#[cfg(all(not(miri), windows))]
mod os {
    use std::io;

    use windows_sys::{Win32::System::Threading::AvSetMmThreadCharacteristicsW, w};

    /// The registration lasts as long as the thread: MMCSS drops it when the
    /// thread exits, so the handle is not kept.
    pub(super) fn audio_feed() -> io::Result<()> {
        let mut task_index = 0;
        // SAFETY: the task name is a static NUL-terminated wide string and
        // the index a live local the call writes once.
        let registration =
            unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &raw mut task_index) };
        if registration.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(any(miri, target_arch = "wasm32"))]
mod os {
    use std::io;

    pub(super) fn audio_feed() -> io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, not(miri), not(target_arch = "wasm32")))]
mod tests {
    use std::io;

    use kithara_test_utils::kithara;

    use super::{ThreadClass, set_current_class};

    /// The class a fresh thread reports after asking for `class`, with the
    /// answer it got.
    fn on_fresh_thread<R: Send + 'static>(
        class: ThreadClass,
        reported: fn() -> R,
    ) -> (Result<(), io::ErrorKind>, R) {
        crate::thread::spawn(move || {
            let answer = set_current_class(class).map_err(|error| error.kind());
            (answer, reported())
        })
        .join()
        .expect("the probe thread does not panic")
    }

    /// The calling thread's nice value, from its own `stat` line.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn nice() -> i32 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").expect("a readable stat line");
        let (_, fields) = stat
            .rsplit_once(')')
            .expect("a stat line names its command");
        fields
            .split_whitespace()
            .nth(16)
            .and_then(|nice| nice.parse().ok())
            .expect("a stat line carries the nice value")
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[kithara::test(native)]
    fn an_audio_feed_thread_runs_at_the_nice_value_the_os_granted() {
        let (answer, nice) = on_fresh_thread(ThreadClass::AudioFeed, nice);

        match answer {
            Ok(()) => assert_eq!(nice, crate::consts::AUDIO_FEED_NICE),
            Err(kind) => {
                assert_eq!(kind, io::ErrorKind::PermissionDenied);
                assert_eq!(nice, 0, "a refused thread keeps the class it had");
            }
        }
    }

    /// The calling thread's quality-of-service class.
    #[cfg(target_vendor = "apple")]
    fn qos() -> libc::qos_class_t {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative = 0;
        // SAFETY: both out-pointers are live locals the call writes once.
        let status = unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &raw mut class, &raw mut relative)
        };
        assert_eq!(status, 0, "the calling thread's QoS class is readable");
        class
    }

    #[kithara::test(native, platform = apple)]
    fn an_audio_feed_thread_runs_at_the_user_interactive_qos_class() {
        let (answer, class) = on_fresh_thread(ThreadClass::AudioFeed, qos);

        assert_eq!(answer, Ok(()));
        assert!(
            matches!(class, libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE),
            "{class:?}"
        );
    }

    #[cfg(windows)]
    #[kithara::test(native)]
    fn an_audio_feed_thread_joins_the_mmcss_audio_task() {
        let (answer, ()) = on_fresh_thread(ThreadClass::AudioFeed, || ());

        assert_eq!(answer, Ok(()));
    }
}
