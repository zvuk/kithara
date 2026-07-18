use super::Resource;

impl Resource {
    pub(crate) const fn supports_reverse_source(&self) -> bool {
        self.supports_reverse_source
    }
}
