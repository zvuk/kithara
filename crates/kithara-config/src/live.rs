use core::fmt::Debug;

use crate::Config;

/// A configuration that checks every field it declares a check for.
///
/// `check(error = E)` on the struct and `check = path` on its fields generate
/// it; a nested config is validated whole, so its type is checked too.
/// ```compile_fail
/// #[derive(Clone, Copy, kithara_config::Config)]
/// #[config(builder(none))]
/// struct Plain { #[config(value)] level: u8 }
/// #[derive(Clone, Copy, kithara_config::Config)]
/// #[config(builder(none), check(error = std::io::Error))]
/// struct Outer { #[config(nested)] plain: Plain }
/// ```
pub trait CheckedConfig: Config + Sized {
    /// What a field check refuses with; a nested config's error converts into it.
    /// ```compile_fail
    /// #[derive(Clone, Copy, kithara_config::Config)]
    /// #[config(builder(none), check(error = std::io::Error))]
    /// struct Inner { #[config(value, live)] level: u8 }
    /// #[derive(Clone, Copy, kithara_config::Config)]
    /// #[config(builder(none))]
    /// struct Outer { #[config(nested, live)] inner: Inner }
    /// ```
    type Error;

    /// This config if every field check accepts its value, in declaration
    /// order, nested configs included.
    ///
    /// # Errors
    ///
    /// Returns the refusal of the first field whose check refuses its value.
    fn validated(self) -> Result<Self, Self::Error>;
}

/// A configuration whose `live` fields change one at a time while it runs.
///
/// Each live field is one variant of [`LiveConfig::Change`]; a change passes
/// its field's check alone and assigns that field alone.
/// ```compile_fail
/// #[derive(Clone, kithara_config::Config)]
/// #[config(builder(none))]
/// struct Label { #[config(value, live)] text: String }
/// ```
pub trait LiveConfig: CheckedConfig + Copy {
    /// One change of one live field.
    type Change: Copy + Debug;

    /// Whether a field is `live(owner)`, executed by the owner's own method.
    /// A nested live config has none: its parent's owner executes it whole.
    /// ```compile_fail
    /// #[derive(Clone, Copy, kithara_config::Config)]
    /// #[config(builder(none))]
    /// struct Rate { #[config(value, live(owner))] hz: u32 }
    /// #[derive(Clone, Copy, kithara_config::Config)]
    /// #[config(builder(none))]
    /// struct Deck { #[config(nested, live)] rate: Rate }
    /// ```
    const OWNER_FIELDS: bool;

    /// Assigns the field `change` names; it neither checks nor allocates.
    fn apply_change(&mut self, change: Self::Change);

    /// The change if its field's check accepts the value it carries.
    ///
    /// # Errors
    ///
    /// Returns the field check's refusal.
    fn check(change: Self::Change) -> Result<Self::Change, Self::Error>;
}

/// The owner of a live configuration, addressed by the configuration's change
/// type so one owner configures several configurations without ambiguity.
pub trait Configure<Ch> {
    /// When a change executes; `Default` is the nearest moment.
    type At: Default;
    /// The configuration the changes belong to.
    type Config: LiveConfig<Change = Ch>;
    /// What the owner refuses a change with.
    type Error;
    /// What the owner answers once it accepts a change.
    type Output;

    /// Hands one change of one field to the owner to execute at `at`.
    ///
    /// # Errors
    ///
    /// Returns the owner's refusal, a failed field check included.
    fn configure(&self, change: Ch, at: Self::At) -> Result<Self::Output, Self::Error>;

    /// The configuration as last applied.
    fn settings(&self) -> Self::Config;
}

/// A nested live configuration, configured through its parent's owner.
///
/// A change goes to the owner as the parent's change, so the parent's check
/// guards it; `get` reads the nested configuration out of the parent's.
pub struct Nested<R, G> {
    pub(crate) get: G,
    pub(crate) owner: R,
}

impl<T, P, N> Configure<N::Change> for Nested<&T, fn(&P) -> N>
where
    T: Configure<P::Change, Config = P> + ?Sized,
    P: LiveConfig<Change: From<N::Change>>,
    N: LiveConfig,
{
    type At = T::At;
    type Config = N;
    type Error = T::Error;
    type Output = T::Output;

    fn configure(&self, change: N::Change, at: T::At) -> Result<T::Output, T::Error> {
        T::configure(self.owner, change.into(), at)
    }

    fn settings(&self) -> N {
        (self.get)(&T::settings(self.owner))
    }
}
