use kithara_audio::ServiceClass;

pub(crate) struct PreparedElasticRenderer {
    _private: (),
}

impl PreparedElasticRenderer {
    pub(super) const fn decoded_frontier(&self) -> f64 {
        0.0
    }

    pub(super) fn set_service_class(&mut self, _class: ServiceClass) {}
}
