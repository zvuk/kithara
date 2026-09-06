#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Intake {
    Full,
    Continuing,
    Anywhere,
}

#[derive(Clone, Copy)]
pub(crate) enum Opens {
    Run,
    Extends,
}
