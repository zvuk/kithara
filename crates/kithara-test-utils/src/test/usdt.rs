use std::{
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, ThreadId},
};

use kithara_platform::{
    sync::{Arc, Notify},
    time::timeout,
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    filter::filter_fn,
    layer::{Context, Layer},
    registry::LookupSpan,
};

/// Payload fields one probe firing can carry: the USDT provider's six `u64`
/// arguments minus the operation id.
const MAX_FIELDS: usize = 5;

/// Firings of one probe a scope's history holds. Past it that probe stops
/// entering the history and every reader of it fails, so a hot probe can
/// neither swell the process nor hand a census a silently truncated history,
/// while the other probes keep recording. The latest firing of each probe
/// stays readable either way. The history is bounded by this cap times the
/// probe call sites the process compiled.
pub const MAX_EVENTS: usize = 1 << 19;

/// One recorded firing. Every string is `'static`, so recording a firing
/// allocates nothing beyond the history itself.
#[derive(Clone, Copy, Debug)]
pub struct ProbeEvent {
    pub target: &'static str,
    pub probe: &'static str,
    /// Source file of the product function the probe is attached to.
    pub file: Option<&'static str>,
    /// Source line of that function.
    pub line: Option<u32>,
    /// Thread the probe fired on.
    pub thread: ThreadId,
    fields: [(&'static str, u64); MAX_FIELDS],
    len: usize,
}

impl ProbeEvent {
    /// Every payload field this firing carried, in the order the probe named them.
    pub fn fields(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        self.fields[..self.len].iter().copied()
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<u64> {
        self.fields[..self.len]
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    }
}

struct State {
    /// Probe names seen by this process. Bounded by the probe call sites, and
    /// kept across scopes so each name is interned once.
    names: Vec<&'static str>,
    events: Vec<ProbeEvent>,
    probes: Vec<ProbeTally>,
    recorded: Option<Arc<Notify>>,
}

/// What the live scope saw of one probe: its latest firing and how many times
/// it fired, including the firings past [`MAX_EVENTS`] the history dropped.
struct ProbeTally {
    latest: ProbeEvent,
    fired: usize,
}

impl ProbeTally {
    fn overflowed(&self) -> bool {
        self.fired > MAX_EVENTS
    }
}

impl State {
    fn intern(&mut self, probe: &str) -> &'static str {
        if let Some(name) = self.names.iter().find(|name| **name == probe) {
            return name;
        }
        let name: &'static str = Box::leak(probe.into());
        self.names.push(name);
        name
    }

    fn tally(&self, probe: &str) -> Option<&ProbeTally> {
        self.probes.iter().find(|tally| tally.latest.probe == probe)
    }

    fn overflowed(&self) -> impl Iterator<Item = &ProbeTally> {
        self.probes.iter().filter(|tally| tally.overflowed())
    }

    fn history(&self) -> &[ProbeEvent] {
        let overflowed = self
            .overflowed()
            .map(|tally| format!("{} x{}", tally.latest.probe, tally.fired))
            .collect::<Vec<_>>();
        assert!(
            overflowed.is_empty(),
            "usdt scope history overflowed {MAX_EVENTS} firings of {}; \
             read the probes under test with `events_of`",
            overflowed.join(", ")
        );
        &self.events
    }

    fn history_of(&self, probe: &str) -> Vec<ProbeEvent> {
        if let Some(tally) = self.tally(probe) {
            assert!(
                !tally.overflowed(),
                "usdt scope history overflowed {MAX_EVENTS} firings of {probe} \
                 ({} fired); scope the observation to the operation under test",
                tally.fired
            );
        }
        self.events
            .iter()
            .filter(|event| event.probe == probe)
            .copied()
            .collect()
    }
}

static STATE: Mutex<State> = Mutex::new(State {
    names: Vec::new(),
    events: Vec::new(),
    probes: Vec::new(),
    recorded: None,
});
static SCOPE: Mutex<()> = Mutex::new(());
/// Thread that owns the live recording and the handle nested scopes join,
/// while one is open.
static NESTING: Mutex<Option<(ThreadId, Arc<Notify>)>> = Mutex::new(None);
/// Lock-free gate for the probe hot path: set only while a [`Scope`] lives.
static ARMED: AtomicBool = AtomicBool::new(false);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Scope {
    recorded: Arc<Notify>,
    /// Held by the outermost scope on the recording thread; a nested scope
    /// carries `None` and leaves the serialisation to the one that opened it.
    /// Which of the two this is decides what dropping it does.
    serial: Option<MutexGuard<'static, ()>>,
}

/// Records every probe firing until the scope drops.
///
/// The process keeps one recording, so a scope taken while this thread already
/// holds one joins it rather than starting a second: the history survives until
/// the outermost scope drops. Another thread waits for that drop.
///
/// A nested scope closes before the one it joined, which is what holding it in
/// a local or a nested value gives. Outliving that one leaves it reading a
/// recording the outer drop already closed.
#[must_use]
pub fn scope() -> Scope {
    let here = thread::current().id();
    {
        let nesting = lock(&NESTING);
        if let Some((owner, recorded)) = nesting.as_ref()
            && *owner == here
        {
            return Scope {
                recorded: Arc::clone(recorded),
                serial: None,
            };
        }
    }
    let serial = lock(&SCOPE);
    let recorded = Arc::new(Notify::default());
    {
        let mut state = lock(&STATE);
        state.events = Vec::new();
        state.probes = Vec::new();
        state.recorded = Some(Arc::clone(&recorded));
    }
    *lock(&NESTING) = Some((here, Arc::clone(&recorded)));
    ARMED.store(true, Ordering::Release);
    Scope {
        recorded,
        serial: Some(serial),
    }
}

/// Every firing recorded by the live [`Scope`], in order.
#[must_use]
pub fn events() -> Vec<ProbeEvent> {
    lock(&STATE).history().to_vec()
}

/// The firings recorded so far, and whether any probe stopped entering them.
///
/// The reader for evidence written from a destructor: [`events`] fails on an
/// overflowed history, and a panic there aborts the process instead of
/// reporting the assertion the test was already failing.
#[must_use]
pub fn recorded() -> (Vec<ProbeEvent>, bool) {
    let state = lock(&STATE);
    (state.events.clone(), state.overflowed().next().is_some())
}

/// The latest firing of `probe` recorded by the live [`Scope`].
#[must_use]
pub fn last(probe: &str) -> Option<ProbeEvent> {
    lock(&STATE).tally(probe).map(|tally| tally.latest)
}

/// Every firing of `probe` recorded by the live [`Scope`], in order. Fails
/// only when that probe overflowed, whatever the other probes did.
#[must_use]
pub fn events_of(probe: &str) -> Vec<ProbeEvent> {
    lock(&STATE).history_of(probe)
}

impl Scope {
    #[must_use]
    pub fn events(&self) -> Vec<ProbeEvent> {
        events()
    }

    #[must_use]
    pub fn events_of(&self, probe: &str) -> Vec<ProbeEvent> {
        events_of(probe)
    }

    #[must_use]
    pub fn last(&self, probe: &str) -> Option<ProbeEvent> {
        last(probe)
    }

    /// Resolves once the firings recorded so far satisfy `holds`,
    /// re-checking after every newly recorded firing.
    ///
    /// The wait runs under the hang watchdog, so a condition the product never
    /// satisfies fails the test with what was recorded instead of parking it
    /// until the runner kills the binary. The budget is real time, so the
    /// caller runs under `flash(false)`: a virtual clock jumps the deadline
    /// while the work it waits on runs on a real-time thread.
    ///
    /// # Panics
    ///
    /// Panics when `holds` has not held within the watchdog budget.
    #[crate::kithara::hang_watchdog(ctx = String)]
    pub async fn wait_for<F>(&self, mut holds: F)
    where
        F: FnMut(&[ProbeEvent]) -> bool,
    {
        loop {
            let recorded = self.recorded.notified();
            if holds(&lock(&STATE).events) {
                return;
            }
            if timeout(__hang_detector.remaining(), recorded)
                .await
                .is_err()
            {
                hang_tick!(Self::unsatisfied_wait());
            } else {
                hang_tick!();
            }
        }
    }

    /// Describe a wait that ran out of budget: which probes it did see, so
    /// the failure names the missing firing.
    fn unsatisfied_wait() -> String {
        let (events, overflowed) = recorded();
        let mut counts: Vec<(&'static str, usize, ProbeEvent)> = Vec::new();
        for event in &events {
            match counts.iter_mut().find(|(probe, ..)| *probe == event.probe) {
                Some((_, count, latest)) => {
                    *count += 1;
                    *latest = *event;
                }
                None => counts.push((event.probe, 1, *event)),
            }
        }
        counts.sort_unstable_by_key(|(_, count, _)| std::cmp::Reverse(*count));
        let seen = counts
            .iter()
            .map(|(probe, count, latest)| {
                let fields = latest
                    .fields()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{probe} x{count} [{fields}]")
            })
            .collect::<Vec<_>>()
            .join("; ");
        let overflow = if overflowed {
            let probes = lock(&STATE)
                .overflowed()
                .map(|tally| format!("{} x{}", tally.latest.probe, tally.fired))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" (history overflowed on {probes})")
        } else {
            String::new()
        };
        format!(
            "usdt scope condition never held; \
             recorded {} firings{overflow}, latest of each: {seen}",
            events.len()
        )
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.serial.is_none() {
            return;
        }
        *lock(&NESTING) = None;
        ARMED.store(false, Ordering::Release);
        let mut state = lock(&STATE);
        state.recorded = None;
        state.events = Vec::new();
        state.probes = Vec::new();
    }
}

#[must_use]
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    UsdtLayer.with_filter(filter_fn(|meta| {
        meta.is_event() && meta.target().ends_with("_probe")
    }))
}

struct UsdtLayer;

impl<S: Subscriber> Layer<S> for UsdtLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !ARMED.load(Ordering::Acquire) {
            return;
        }
        let mut visitor = ProbeVisitor {
            probe: None,
            fields: [("", 0); MAX_FIELDS],
            len: 0,
        };
        let mut state = lock(&STATE);
        let Some(recorded) = state.recorded.clone() else {
            return;
        };
        event.record(&mut FieldVisitor {
            state: &mut state,
            visitor: &mut visitor,
        });
        let Some(probe) = visitor.probe else {
            return;
        };
        let metadata = event.metadata();
        let recorded_event = ProbeEvent {
            target: metadata.target(),
            probe,
            file: metadata.file(),
            line: metadata.line(),
            thread: thread::current().id(),
            fields: visitor.fields,
            len: visitor.len,
        };
        let fired = if let Some(tally) = state
            .probes
            .iter_mut()
            .find(|tally| tally.latest.probe == probe)
        {
            tally.latest = recorded_event;
            tally.fired += 1;
            tally.fired
        } else {
            state.probes.push(ProbeTally {
                latest: recorded_event,
                fired: 1,
            });
            1
        };
        if fired <= MAX_EVENTS {
            state.events.push(recorded_event);
        }
        drop(state);
        recorded.notify_one();
    }
}

struct ProbeVisitor {
    probe: Option<&'static str>,
    fields: [(&'static str, u64); MAX_FIELDS],
    len: usize,
}

struct FieldVisitor<'a> {
    state: &'a mut State,
    visitor: &'a mut ProbeVisitor,
}

impl FieldVisitor<'_> {
    fn push(&mut self, field: &Field, value: u64) {
        let visitor = &mut *self.visitor;
        if let Some(slot) = visitor.fields.get_mut(visitor.len) {
            *slot = (field.name(), value);
            visitor.len += 1;
        }
    }
}

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "probe" {
            self.visitor.probe = Some(self.state.intern(value));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value);
    }

    /// Session-axis frames arrive as `i64`; a negative one has no `u64`
    /// reading, so it stays absent and the reader asking for it fails loudly.
    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.push(field, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_EVENTS, events, events_of, recorded, scope};

    fn fire(probe: &'static str, value: u64) {
        tracing::event!(target: "kithara_test_probe", tracing::Level::TRACE, probe = probe, value = value);
    }

    #[test]
    fn a_scope_records_every_probe_in_order() {
        crate::test::setup_tracing();
        let trace = scope();
        fire("first", 1);
        fire("second", 2);

        let events = trace.events();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].probe, "first");
        assert_eq!(events[1].field("value"), Some(2));
    }

    #[test]
    fn an_overflowing_history_keeps_the_latest_firing() {
        crate::test::setup_tracing();
        let trace = scope();
        for value in 0..=MAX_EVENTS as u64 {
            fire("tick", value);
        }

        assert_eq!(
            trace.last("tick").and_then(|event| event.field("value")),
            Some(MAX_EVENTS as u64)
        );
    }

    #[test]
    #[should_panic(expected = "overflowed")]
    fn an_overflowing_history_fails_its_reader() {
        crate::test::setup_tracing();
        let trace = scope();
        for value in 0..=MAX_EVENTS as u64 {
            fire("tick", value);
        }

        let _ = trace.events();
    }

    #[test]
    fn a_hot_probe_leaves_the_history_of_another_readable() {
        crate::test::setup_tracing();
        let trace = scope();
        fire("quiet", 1);
        for value in 0..=MAX_EVENTS as u64 {
            fire("hot", value);
        }
        fire("quiet", 2);

        let quiet = trace.events_of("quiet");

        assert_eq!(quiet.len(), 2);
        assert_eq!(quiet[1].field("value"), Some(2));
    }

    #[test]
    #[should_panic(expected = "overflowed")]
    fn an_overflowed_probe_fails_its_own_reader() {
        crate::test::setup_tracing();
        let _trace = scope();
        for value in 0..=MAX_EVENTS as u64 {
            fire("hot", value);
        }

        let _ = events_of("hot");
    }

    #[test]
    fn a_complete_history_reads_back_untruncated() {
        crate::test::setup_tracing();
        let _trace = scope();
        fire("first", 1);
        fire("second", 2);

        let (events, truncated) = recorded();

        assert!(!truncated);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn an_overflowing_history_reads_back_truncated_for_a_destructor() {
        crate::test::setup_tracing();
        let _trace = scope();
        for value in 0..=MAX_EVENTS as u64 {
            fire("tick", value);
        }

        let (events, truncated) = recorded();

        assert!(truncated);
        assert_eq!(events.len(), MAX_EVENTS);
    }

    #[test]
    fn a_nested_scope_joins_the_live_recording() {
        crate::test::setup_tracing();
        let outer = scope();
        fire("before", 1);
        let nested = scope();
        fire("during", 2);

        assert_eq!(nested.events().len(), 2);

        drop(nested);
        fire("after", 3);

        assert_eq!(outer.events().len(), 3);
    }

    #[test]
    fn only_the_outermost_scope_closes_the_recording() {
        crate::test::setup_tracing();
        let outer = scope();
        fire("kept", 1);
        drop(scope());

        assert_eq!(outer.events().len(), 1);

        drop(outer);

        assert!(events().is_empty());
    }
}
