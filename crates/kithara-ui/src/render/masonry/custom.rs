use kithara_platform::time::Duration;
use masonry::core::ErasedAction;

use crate::{
    draw::{DrawListBuilder, Rect},
    interact::{Hit, Input, Outcome},
    shaping::{GlyphRun, TextContext},
    skin::TextRoleSkin,
};

/// Frame scheduling requested by custom content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Repaint {
    /// Paint only when external state invalidates the widget.
    #[default]
    None,
    /// Request one animation frame and then return to external invalidation.
    NextFrame,
    /// Keep requesting animation frames until the widget changes this declaration.
    Continuous,
}

/// Public drawing and intrinsic-measurement contract for Masonry-hosted content.
///
/// Intrinsic measurement is authoritative only on document axes declared
/// `Shrink`. A `Fill` axis receives the resolved document rectangle in
/// [`Self::paint`]. Content that preserves an authored aspect ratio must
/// letterbox itself inside that rectangle; the host never stretches or applies
/// an affine compensation behind the document's back.
pub trait CustomWidget: 'static {
    /// Consumer-owned action emitted by this component.
    type Action: std::fmt::Debug + Send + 'static;

    /// Returns the content's intrinsic logical extent under the supplied limits.
    fn measure(&mut self, text: &mut TextMeasurer<'_>, limits: SizeLimits) -> Size2;

    /// Handles one neutral input event in the component's local hit space.
    fn input(&mut self, _input: Input<'_>, _hit: Hit) -> Outcome<Self::Action> {
        Outcome::IGNORED
    }

    /// Whether focusing this component should start a platform input-method session.
    fn accepts_text_input(&self) -> bool {
        false
    }

    /// Advances retained component state and returns at most one typed action.
    ///
    /// This hook is the egress for actions recognised while drawing or by a
    /// consumer-owned signal such as a long-press recognizer. The host invokes
    /// it only from its normal animation-frame lifecycle.
    fn frame(&mut self, _elapsed: Duration) -> Option<Self::Action> {
        None
    }

    /// Appends drawing commands for the resolved local rectangle.
    fn paint(&mut self, list: &mut DrawListBuilder, text: &mut TextMeasurer<'_>, bounds: Rect);

    /// Declares whether the host should schedule animation frames.
    fn repaint(&self) -> Repaint {
        Repaint::None
    }
}

#[derive(Debug)]
pub(crate) struct HostAction(ErasedAction);

impl HostAction {
    pub(crate) fn new<Action>(action: Action) -> Self
    where
        Action: std::fmt::Debug + Send + 'static,
    {
        Self(Box::new(action))
    }

    pub(crate) fn downcast<Action>(self) -> Result<Action, Self>
    where
        Action: std::fmt::Debug + Send + 'static,
    {
        self.0.downcast().map(|action| *action).map_err(Self)
    }

    delegate::delegate! {
        to self.0 {
            pub(crate) fn type_name(&self) -> &'static str;
        }
    }
}

pub(crate) trait MountedCustom {
    fn measure(&mut self, text: &mut TextMeasurer<'_>, limits: SizeLimits) -> Size2;

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<HostAction>;

    fn accepts_text_input(&self) -> bool;

    fn frame(&mut self, elapsed: Duration) -> Option<HostAction>;

    fn paint(&mut self, list: &mut DrawListBuilder, text: &mut TextMeasurer<'_>, bounds: Rect);

    fn repaint(&self) -> Repaint;
}

pub(crate) struct MappedCustom<Widget, Map> {
    widget: Widget,
    map: Map,
}

impl<Widget, Map> MappedCustom<Widget, Map> {
    pub(crate) const fn new(widget: Widget, map: Map) -> Self {
        Self { widget, map }
    }
}

impl<Widget, Map> MountedCustom for MappedCustom<Widget, Map>
where
    Widget: CustomWidget,
    Map: Fn(Widget::Action) -> HostAction,
{
    delegate::delegate! {
        to self.widget {
            fn accepts_text_input(&self) -> bool;
            fn measure(&mut self, text: &mut TextMeasurer<'_>, limits: SizeLimits) -> Size2;
            #[expr($.map(&self.map))]
            fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<HostAction>;
            #[expr($.map(&self.map))]
            fn frame(&mut self, elapsed: Duration) -> Option<HostAction>;
            fn paint(&mut self, list: &mut DrawListBuilder, text: &mut TextMeasurer<'_>, bounds: Rect);
            fn repaint(&self) -> Repaint;
        }
    }
}

/// Logical two-dimensional extent used by custom Masonry content.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Size2 {
    pub w: f32,
    pub h: f32,
}

impl Size2 {
    #[must_use]
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// Minimum and maximum logical extents available during intrinsic measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SizeLimits {
    min: Size2,
    max: Size2,
}

impl SizeLimits {
    #[must_use]
    pub const fn new(min: Size2, max: Size2) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub const fn min(self) -> Size2 {
        self.min
    }

    #[must_use]
    pub const fn max(self) -> Size2 {
        self.max
    }
}

/// Borrowed access to Kithara's canonical text shaper.
///
/// This facade owns no cache and no font collection. Custom widgets therefore
/// receive the same metrics as built-in Kithara controls instead of creating a
/// second text answer at the host boundary.
pub struct TextMeasurer<'a> {
    context: &'a mut TextContext,
}

impl<'a> TextMeasurer<'a> {
    pub(crate) const fn new(context: &'a mut TextContext) -> Self {
        Self { context }
    }

    /// Shapes text with the complete skin role and an optional wrapping width.
    #[must_use]
    pub fn shape(&mut self, content: &str, role: TextRoleSkin, max_width: Option<f32>) -> GlyphRun {
        self.context.shape(content, role, max_width)
    }

    /// Measures a shaped run without retaining a second cached layout.
    #[must_use]
    pub fn measure(&mut self, content: &str, role: TextRoleSkin, max_width: Option<f32>) -> Size2 {
        let run = self.shape(content, role, max_width);
        Size2::new(run.width(), run.height())
    }
}
