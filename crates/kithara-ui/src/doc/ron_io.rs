use ron::{Options, extensions::Extensions};

pub(crate) fn options() -> Options {
    Options::default().with_default_extension(Extensions::IMPLICIT_SOME)
}
