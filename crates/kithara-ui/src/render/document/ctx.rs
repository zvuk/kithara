use kithara_platform::time::Duration;
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::wave::zoom_math::DEFAULT_ZOOM,
    compile::CompiledUi,
    draw::Pt,
    expand::{Binding, BindingKind, BlockSpec},
    registry::SECONDS,
    render::{ReadValue, Reads, custom::CustomKinds},
    size::Snapshot,
    skin::SkinDoc,
    view::ViewState,
};

/// The host's reading of time for one frame.
///
/// Held by the host rather than read from a wall clock wherever it is wanted, so
/// a frame is reproduced by driving the clock instead of by waiting for real
/// time to come round to the same value.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct Clock {
    /// How long the host has been running.
    pub elapsed: Duration,
    /// How many frames this host has drawn.
    pub frame: u64,
}

impl Clock {
    #[must_use]
    pub const fn new(frame: u64, elapsed: Duration) -> Self {
        Self { elapsed, frame }
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
#[derive(Clone, Copy, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub struct Ctx<'a, 'r> {
    /// The compiled document being walked.
    pub ui: &'a CompiledUi,
    /// The skin the layout pass consults.
    pub skin: &'a SkinDoc,
    /// This host's reading of time for the frame being drawn.
    pub clock: Clock,
    /// The extensions the application registered, which is what a `Custom`
    /// control resolves its kind against. Nothing registered is the ordinary
    /// case: a document that names a kind is refused while it compiles unless
    /// the same set was declared to `UiConfig`.
    #[field(with, option_set_some, vis = "pub")]
    pub kinds: Option<&'a CustomKinds>,
    /// The state the screen keeps for itself, which the host owns and the
    /// application is not asked for.
    view: &'r ViewState,
    reads: &'r dyn Reads,
}

impl<'a, 'r> Ctx<'a, 'r> {
    #[must_use]
    pub const fn new(
        ui: &'a CompiledUi,
        reads: &'r dyn Reads,
        view: &'r ViewState,
        skin: &'a SkinDoc,
        clock: Clock,
    ) -> Self {
        Self {
            ui,
            skin,
            clock,
            view,
            reads,
            kinds: None,
        }
    }

    /// The endpoint a binding reads from, or nothing when it only writes.
    ///
    /// A name outlives the frame that found it, which a value does not, so this
    /// is what a control keeps when it means to ask the same question again on
    /// a later frame without holding on to this one.
    #[must_use]
    pub fn endpoint(self, read: Option<&Binding>) -> Option<&'a str> {
        read.filter(|binding| {
            !matches!(
                binding.kind,
                BindingKind::Command | BindingKind::View { .. } | BindingKind::Page { .. }
            )
        })
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

    /// The value at one endpoint, with the host's own answered before the
    /// application is asked.
    #[must_use]
    pub fn get(self, endpoint: &str) -> Option<ReadValue<'r>> {
        if endpoint == SECONDS {
            return Some(ReadValue::Scalar(self.clock.seconds()));
        }
        self.reads.get(endpoint)
    }

    /// What one text binding answers, or nothing when there is no binding, no
    /// answer, or an empty answer.
    #[must_use]
    pub fn label(self, binding: Option<&Binding>) -> Option<&'r str> {
        match self.read(binding?)? {
            ReadValue::Text(label) if !label.is_empty() => Some(label),
            _ => None,
        }
    }

    /// The point one binding answers with, or nothing when there is no
    /// binding, no answer, or an answer that is not a point.
    #[must_use]
    pub fn point(self, binding: Option<&Binding>) -> Option<Pt> {
        match self.read(binding?)? {
            ReadValue::Point(at) if at.x.is_finite() && at.y.is_finite() => Some(at),
            _ => None,
        }
    }

    /// What one binding reads to. A command binding writes and never reads, so
    /// it has no value on this side.
    #[must_use]
    pub fn read(self, binding: &Binding) -> Option<ReadValue<'r>> {
        match binding.kind {
            BindingKind::Command => None,
            BindingKind::View { .. } => Some(ReadValue::Bool(
                self.view.flag(self.ui.resolve(binding.key)),
            )),
            BindingKind::Page { name } => Some(ReadValue::Bool(
                self.ui
                    .views()
                    .standing(self.view, self.ui.resolve(binding.key))
                    == Some(self.ui.resolve(name)),
            )),
            _ => self.get(self.ui.resolve(binding.key)),
        }
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
    pub fn wave_zoom(self, zoom: Option<&Binding>) -> crate::render::Zoom {
        zoom.and_then(|binding| self.read(binding))
            .and_then(|value| match value {
                ReadValue::Scalar(value) => Some(AsPrimitive::<f32>::as_(value)),
                _ => None,
            })
            .map_or_else(
                || crate::render::Zoom::from(DEFAULT_ZOOM),
                crate::render::Zoom::from,
            )
    }
}

/// The document asks about room through the same context it asks about values:
/// what a block hides, and what a measured binding reads to.
impl Snapshot for Ctx<'_, '_> {
    fn hidden(&self, block: &BlockSpec) -> bool {
        self.flag(Some(&block.hidden))
    }

    /// A measurement that is not a finite number is no measurement, so the
    /// node falls back to the branch it draws with nothing read.
    fn measure(&self, measure: &Binding) -> Option<f32> {
        let Some(ReadValue::Scalar(value)) = self.read(measure) else {
            return None;
        };
        let value: f32 = value.as_();
        value.is_finite().then_some(value)
    }
}

/// A context reads like any other source, so a helper that only wants values
/// takes one without the application's own reader ever being reachable again.
impl Reads for Ctx<'_, '_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        Self::get(*self, endpoint)
    }
}
