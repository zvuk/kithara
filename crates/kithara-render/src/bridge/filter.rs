use kithara_command::Seq;

/// The deck filter as the executor last published it.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct FilterSnapshot {
    /// The knob position the filter runs at now.
    #[field(get, copy)]
    knob: f64,
    /// The program that last moved the knob, while it runs or holds its end.
    #[field(get, copy)]
    program: Option<Seq>,
    /// The manual change a new program must name to take over the knob.
    #[field(get, copy)]
    authority: Option<Seq>,
}

impl FilterSnapshot {
    pub(crate) fn publish(&mut self, knob: f64, program: Option<Seq>, authority: Option<Seq>) {
        *self = Self {
            knob,
            program,
            authority,
        };
    }
}
