use kithara_platform::time::Duration;
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::wave::zoom_math::DEFAULT_ZOOM,
    compile::CompiledUi,
    expand::{Binding, BindingKind},
    registry::SECONDS,
    render::{ReadValue, Reads},
    skin::SkinDoc,
};

/// The host's reading of time for one frame.
///
/// Held by the host rather than read from a wall clock wherever it is wanted, so
/// a frame is reproduced by driving the clock instead of by waiting for real
/// time to come round to the same value.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct Clock {
    /// How many frames this host has drawn.
    pub frame: u64,
    /// How long the host has been running.
    pub elapsed: Duration,
}

impl Clock {
    #[must_use]
    pub const fn new(frame: u64, elapsed: Duration) -> Self {
        Self { frame, elapsed }
    }

    /// The next frame, `step` later.
    #[must_use]
    pub fn advance(self, step: Duration) -> Self {
        Self {
            frame: self.frame.saturating_add(1),
            elapsed: self.elapsed.saturating_add(step),
        }
    }

    /// The reading a document sees when it binds to [`SECONDS`].
    #[must_use]
    pub fn seconds(self) -> f64 {
        self.elapsed.as_secs_f64()
    }
}

/// Everything that is the same for every node of one frame, carried as one
/// structure so a new thing to hand down does not widen every signature on the
/// way.
///
/// The application's values are reached only through this, which is what stops a
/// widget being written that quietly misses the host's own endpoints.
///
/// The document and the reader are held apart because they do not live the same
/// length of time: a host compiles once and answers afresh every frame, so what
/// it draws must not be tied to the reader that frame borrowed.
#[derive(Clone, Copy)]
pub struct Ctx<'a, 'r> {
    /// The compiled document being walked.
    pub ui: &'a CompiledUi,
    /// The skin the layout pass consults.
    pub skin: &'a SkinDoc,
    /// This host's reading of time for the frame being drawn.
    pub clock: Clock,
    reads: &'r dyn Reads,
}

impl<'a, 'r> Ctx<'a, 'r> {
    #[must_use]
    pub const fn new(
        ui: &'a CompiledUi,
        reads: &'r dyn Reads,
        skin: &'a SkinDoc,
        clock: Clock,
    ) -> Self {
        Self {
            ui,
            skin,
            clock,
            reads,
        }
    }

    /// The value at one endpoint, with the host's own answered before the
    /// application is asked.
    #[must_use]
    pub fn get(self, endpoint: &str) -> Option<ReadValue<'r>> {
        if endpoint == SECONDS {
            return Some(ReadValue::Scalar(self.clock.seconds()));
        }
        self.reads.get(endpoint)
    }

    /// What one binding reads to. A command binding writes and never reads, so
    /// it has no value on this side.
    #[must_use]
    pub fn read(self, binding: &Binding) -> Option<ReadValue<'r>> {
        match binding.kind {
            BindingKind::Command => None,
            _ => self.get(self.ui.resolve(binding.key)),
        }
    }

    /// The endpoint a binding reads from, or nothing when it only writes.
    ///
    /// A name outlives the frame that found it, which a value does not, so this
    /// is what a control keeps when it means to ask the same question again on
    /// a later frame without holding on to this one.
    #[must_use]
    pub fn endpoint(self, read: Option<&Binding>) -> Option<&'a str> {
        read.filter(|binding| !matches!(binding.kind, BindingKind::Command))
            .map(|binding| self.ui.resolve(binding.key))
    }

    /// Whether a binding reads true. An absent binding, or one that reads
    /// anything but a true flag, is false.
    #[must_use]
    pub fn flag(self, binding: Option<&Binding>) -> bool {
        matches!(
            binding.and_then(|binding| self.read(binding)),
            Some(ReadValue::Bool(true))
        )
    }

    /// The scope argument a binding carries after its endpoint id.
    #[must_use]
    pub fn scope(self, read: Option<&Binding>) -> &'a str {
        read.map_or("", |binding| {
            let key = self.ui.resolve(binding.key);
            let id_len = self.ui.resolve(binding.id).len();
            key.get(id_len..).unwrap_or("")
        })
    }

    /// The zoom a waveform is drawn at, or the default when nothing answers.
    #[must_use]
    pub fn wave_zoom(self, zoom: Option<&Binding>) -> f32 {
        zoom.and_then(|binding| self.read(binding))
            .and_then(|value| match value {
                ReadValue::Scalar(value) => Some(value.as_()),
                _ => None,
            })
            .unwrap_or(DEFAULT_ZOOM)
    }
}

/// A context reads like any other source, so a helper that only wants values
/// takes one without the application's own reader ever being reachable again.
impl Reads for Ctx<'_, '_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        Self::get(*self, endpoint)
    }
}

/// A context over a document that says nothing, for a test that measures what
/// one reader answers rather than what a document is shaped like.
#[cfg(test)]
pub(crate) fn probe(reads: &dyn Reads) -> Ctx<'_, '_> {
    use std::sync::LazyLock;

    use crate::{
        builtin,
        compile::compile,
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry},
        source::UiConfig,
    };

    struct Nothing;

    impl EndpointRegistry for Nothing {
        fn endpoint(&self, _category: EndpointCategory, _id: &EndpointId) -> Option<&EndpointDesc> {
            None
        }
    }

    static EMPTY: LazyLock<CompiledUi> = LazyLock::new(|| {
        let mut resolver = builtin::resolver();
        resolver.insert(
            "probe.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "probe",
                root: Module(instance: "page", source: "modules/probe.kmodule.ron"))"#,
        );
        resolver.insert(
            "modules/probe.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "probe", root: Slot(id: "probe", default: []))"#,
        );
        compile(
            "probe.klayout.ron",
            &resolver,
            &Nothing,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the probe document must compile: {error}"))
    });

    Ctx::new(&EMPTY, reads, builtin::skin_doc(), Clock::default())
}
