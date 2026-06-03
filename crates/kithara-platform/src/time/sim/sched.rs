use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};

use parking_lot::{Condvar, Mutex};

use super::SIM_NANOS;

/// One waiter's wake handle: a flag + condvar so the parked OS thread blocks
/// off-lock and wakes when the engine (or a future signal) fires it.
struct Token {
    woken: Mutex<bool>,
    cv: Condvar,
}

impl Token {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            woken: Mutex::new(false),
            cv: Condvar::new(),
        })
    }
    fn fire(&self) {
        {
            let mut g = self.woken.lock();
            *g = true;
        }
        self.cv.notify_all();
    }
    fn wait(&self) {
        let mut g = self.woken.lock();
        while !*g {
            self.cv.wait(&mut g);
        }
    }
}

/// Process-global quiescence scheduler. ALL fields mutate only under this lock.
/// `SIM_NANOS` is written only by `try_advance_locked` (and the legacy additive
/// `advance`, which Inc 0 leaves untouched and which the harness never calls).
struct SimSched {
    /// Participants currently RUNNING (not inside a wrapped wait).
    active: usize,
    /// Timed waiters keyed by (virtual deadline nanos, unique token id) so the
    /// minimum deadline is `first_key_value`; ties share the same deadline and
    /// differ only by id. The map doubles as token storage.
    parked_timed: BTreeMap<(u64, u64), Arc<Token>>,
    next_id: u64,
    /// Test-only oracle: the sequence of `SIM_NANOS` values the engine jumped
    /// to, recorded under this lock alongside each advance. Proves min-jump,
    /// tie-batching (fewer entries than waiters), and determinism (identical
    /// sequence across runs). Cleared by [`reset`]; not present in non-test
    /// builds.
    #[cfg(test)]
    advance_log: Vec<u64>,
}

impl SimSched {
    const fn new() -> Self {
        Self {
            active: 0,
            parked_timed: BTreeMap::new(),
            next_id: 0,
            #[cfg(test)]
            advance_log: Vec::new(),
        }
    }
}

static SCHED: Mutex<SimSched> = Mutex::new(SimSched::new());

/// Evaluate the advance rule while holding `SCHED`. Returns the tokens to fire
/// AFTER the caller releases the lock (firing under the lock would make the
/// woken thread immediately contend on SCHED). Fires nothing unless every
/// participant is parked (`active == 0`) and at least one timed waiter exists.
fn try_advance_locked(s: &mut SimSched) -> Vec<Arc<Token>> {
    if s.active != 0 {
        return Vec::new();
    }
    let Some((&(min, _), _)) = s.parked_timed.iter().next() else {
        // active == 0 and no timed waiter: nothing to advance to. (Signal-only
        // quiescence detection belongs to a later increment + the watchdog.)
        return Vec::new();
    };
    debug_assert!(
        min >= SIM_NANOS.load(Ordering::Acquire),
        "virtual clock must not move backward"
    );
    SIM_NANOS.store(min, Ordering::Release);
    #[cfg(test)]
    s.advance_log.push(min);
    let mut woken = Vec::new();
    while let Some((&(d, _), _)) = s.parked_timed.iter().next() {
        if d != min {
            break;
        }
        if let Some((_, tok)) = s.parked_timed.pop_first() {
            woken.push(tok);
        }
    }
    // Pre-increment for the waiters we are about to wake, so a woken thread does
    // NOT re-increment `active` on return — it just resumes running.
    s.active += woken.len();
    woken
}

/// RAII handle for a participating root (a thread/task that can run or park).
/// Construct sets the baseline `active`; drop releases it and may trigger an
/// advance if it brought `active` to zero.
#[must_use]
pub(crate) struct Participant {
    _priv: (),
}

pub(crate) fn register_participant() -> Participant {
    SCHED.lock().active += 1;
    Participant { _priv: () }
}

impl Drop for Participant {
    fn drop(&mut self) {
        let mut s = SCHED.lock();
        debug_assert!(s.active > 0, "participant drop without matching register");
        s.active -= 1;
        let to_wake = try_advance_locked(&mut s);
        drop(s);
        for t in to_wake {
            t.fire();
        }
    }
}

/// Park the current thread until the virtual clock reaches `deadline_nanos`.
/// Registers the deadline and decrements `active` in ONE critical section
/// (closing the wake-to-re-register race), evaluates the advance rule, then
/// blocks off-lock on its token. Woken only by the engine crossing the deadline.
pub(crate) fn park_timed(deadline_nanos: u64) {
    let token = Token::new();
    let mut s = SCHED.lock();
    let id = s.next_id;
    s.next_id += 1;
    s.parked_timed
        .insert((deadline_nanos, id), Arc::clone(&token));
    debug_assert!(s.active > 0, "park without a registered participant");
    s.active -= 1;
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    for t in to_wake {
        t.fire();
    }
    token.wait();
}

/// Convenience: park for `Duration` from the current virtual instant.
pub(crate) fn park_for(d: super::Duration) {
    park_timed(super::now_nanos().saturating_add(super::duration_to_nanos(d)));
}

/// Clear all engine state (for the in-process test harness; nextest gives
/// production tests per-process isolation).
pub(crate) fn reset() {
    *SCHED.lock() = SimSched::new();
}

#[cfg(test)]
pub(crate) fn advance_log() -> Vec<u64> {
    SCHED.lock().advance_log.clone()
}

/// Test-only: number of currently RUNNING participants.
#[cfg(test)]
pub(crate) fn active_count() -> usize {
    SCHED.lock().active
}

/// Test-only: number of currently parked timed waiters.
#[cfg(test)]
pub(crate) fn timed_count() -> usize {
    SCHED.lock().parked_timed.len()
}
