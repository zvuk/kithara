use std::{collections::HashMap, time::Instant};

/// One recorded probe event.
#[derive(Clone)]
pub struct ProbeEvent {
    /// Numeric / boolean fields, keyed by name.
    pub fields: HashMap<String, u64>,
    /// Field values that arrived as strings (e.g. `probe = "enqueued"`).
    pub string_fields: HashMap<String, String>,
    /// Wall-clock timestamp of the probe firing.
    pub at: Instant,
    /// Target string of the captured tracing event (e.g.
    /// `"kithara_stream_probe"`, `"kithara_hls_probe"`).
    pub target: String,
}

/// Probe-event keys auto-injected by the `#[kithara::probe]` macro
/// (counter sequencing and call-site location). Pulled out of the
/// custom `Debug` impl so the dump skips the noise and shows only
/// what the probe site actually carried.
mod debug_keys {
    pub(super) const NUMERIC_SERVICE: &[&str] = &["seq", "thread_id", "thread_seq", "caller_line"];
    pub(super) const STRING_SERVICE: &[&str] = &["probe", "caller_file", "caller_fn"];
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "trace dump intentionally omits `at` (timestamp) and `target` \
              (always `<crate>_probe`) — probe events are read by humans \
              scanning per-thread sequences, not deserialized; including \
              these fields buries the signal."
)]
impl std::fmt::Debug for ProbeEvent {
    /// Print only the fields that are actually present. Probes carry
    /// different argument shapes (one fires with `variant`+`segment_index`,
    /// another with `request_id`+`bytes_transferred`); a derived `Debug`
    /// would dump the full `HashMap` payload — printing every probe with
    /// every possible field name regardless of whether the probe carried
    /// it makes traces unreadable. This impl shows just the keys that the
    /// probe set, in the order:
    ///
    /// 1. header — `probe`, `thread_id`, `thread_seq`, `seq`;
    /// 2. user numeric fields (sorted alphabetically);
    /// 3. `caller_fn` if the probe was declared with `caller` and the
    ///    backtrace resolved (empty placeholders are suppressed);
    /// 4. user string fields (sorted alphabetically).
    ///
    /// Service keys (`caller_file`, `caller_line`) are not part of the
    /// dump — they are noisy and rarely useful when reading per-thread
    /// sequences. If you need them, read `event.fields` / `event.string_fields`
    /// directly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("ProbeEvent");
        if let Some(name) = self.probe_name() {
            s.field("probe", &name);
        }
        if let Some(thread_id) = self.thread_id() {
            s.field("thread_id", &thread_id);
        }
        if let Some(thread_seq) = self.thread_seq() {
            s.field("thread_seq", &thread_seq);
        }
        if let Some(seq) = self.seq() {
            s.field("seq", &seq);
        }
        let mut numeric_keys: Vec<&String> = self
            .fields
            .keys()
            .filter(|k| !debug_keys::NUMERIC_SERVICE.contains(&k.as_str()))
            .collect();
        numeric_keys.sort();
        for key in numeric_keys {
            if let Some(value) = self.fields.get(key) {
                s.field(key, value);
            }
        }
        if let Some(caller_fn) = self.caller_fn() {
            s.field("caller_fn", &caller_fn);
        }
        let mut string_keys: Vec<&String> = self
            .string_fields
            .keys()
            .filter(|k| !debug_keys::STRING_SERVICE.contains(&k.as_str()))
            .collect();
        string_keys.sort();
        for key in string_keys {
            if let Some(value) = self.string_fields.get(key) {
                s.field(key, value);
            }
        }
        s.finish()
    }
}

impl ProbeEvent {
    /// File path of the call site that triggered this probe.
    ///
    /// Populated by `#[kithara::probe]` via `Location::caller()` paired
    /// with the macro-injected `#[track_caller]` attribute, so the
    /// recorded file is the caller's, not the probe-attributed
    /// function's own definition. Returns `None` when the probe was
    /// emitted by an older expansion or by the `probe_return` path
    /// (which uses the derived `Probe::record_probe` trait method
    /// without a caller hook).
    #[must_use]
    pub fn caller_file(&self) -> Option<&str> {
        self.str("caller_file")
    }

    /// Demangled symbol name of the function that called this probe.
    ///
    /// Resolved at firing time via
    /// `kithara_test_utils::probes::caller_fn_above` (backtrace +
    /// rustc demangling, with the probe machinery and the
    /// probe-attributed frame itself filtered out). Available only on
    /// non-wasm targets — empty string on wasm32.
    ///
    /// Tests should prefer this over `caller_file()` for call-site
    /// assertions: the symbol name survives file moves and renames,
    /// `caller_file` does not. Returns `None` when the macro could
    /// not resolve the caller (older expansion, debug info stripped,
    /// or an empty placeholder string was recorded).
    #[must_use]
    pub fn caller_fn(&self) -> Option<&str> {
        let value = self.str("caller_fn")?;
        if value.is_empty() { None } else { Some(value) }
    }

    /// Line of the call site that triggered this probe. Pairs with
    /// [`Self::caller_file`].
    #[must_use]
    pub fn caller_line(&self) -> Option<u64> {
        self.u64("caller_line")
    }

    /// The `probe` discriminator field (e.g. `"enqueued"`,
    /// `"fetch_cmd_emitted"`).
    #[must_use]
    pub fn probe_name(&self) -> Option<&str> {
        self.string_fields.get("probe").map(String::as_str)
    }

    /// Process-wide monotonic sequence number for this probe firing.
    ///
    /// Increments via `kithara_test_utils::probes::next_probe_seq()`
    /// each time `#[kithara::probe]` emits a tracing event. Tests can
    /// sort by `seq` to obtain a deterministic ordering even when two
    /// firings share the same `Instant`.
    #[must_use]
    pub fn seq(&self) -> Option<u64> {
        self.u64("seq")
    }

    /// Read a string field, e.g. `caller_file` set automatically by the
    /// `#[kithara::probe]` expansion.
    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.string_fields.get(key).map(String::as_str)
    }

    /// Hashed identifier of the OS thread that fired this probe.
    ///
    /// Pair with [`Self::thread_seq`] to reconstruct each thread's own
    /// call order. Two probes with the same `thread_id` come from the
    /// same OS thread (modulo a 64-bit hash collision); filtering by
    /// `thread_id` gives a single thread's slice of the global trace.
    #[must_use]
    pub fn thread_id(&self) -> Option<u64> {
        self.u64("thread_id")
    }

    /// Per-thread monotonic sequence number for this probe firing.
    ///
    /// Independent of [`Self::seq`]: thread A's first probe and thread
    /// B's first probe both have `thread_seq=0`, but distinct
    /// `thread_id`s. Use this to assert "the i-th probe on thread T
    /// was X with args (...)" without the global ordering noise from
    /// unrelated threads.
    #[must_use]
    pub fn thread_seq(&self) -> Option<u64> {
        self.u64("thread_seq")
    }

    /// Read a `u64` field (`segment_index`, `request_id`, …).
    #[must_use]
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.fields.get(key).copied()
    }
}
