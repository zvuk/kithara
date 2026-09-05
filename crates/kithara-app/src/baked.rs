include!(concat!(env!("OUT_DIR"), "/app_config_baked.rs"));

#[cfg(test)]
mod tests {
    use super::baked_env;

    #[kithara::test(native, flash(false))]
    fn a_name_the_document_never_references_is_absent() {
        assert_eq!(baked_env("KITHARA_NOT_REFERENCED_BY_APP_YAML"), None);
    }

    #[kithara::test(native, flash(false))]
    fn the_document_text_survives_the_build_verbatim() {
        assert_eq!(
            super::BAKED_DOCUMENT,
            include_str!("../app.yaml"),
            "the embedded document must be the file byte-for-byte, not a rendering of it"
        );
    }
}
