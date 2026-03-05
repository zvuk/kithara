//! Foreign observer traits for push-based property updates.

/// Receives player-level state changes from Rust.
///
/// All methods are called on an arbitrary background thread.
/// Swift implementations must dispatch to main actor as needed.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait PlayerObserver: Send + Sync {
    fn on_time_changed(&self, seconds: f64);
    fn on_rate_changed(&self, rate: f32);
    fn on_current_item_changed(&self);
    fn on_status_changed(&self, status_code: i32);
    fn on_error(&self, code: i32, message: String);
    fn on_duration_changed(&self, seconds: f64);
    fn on_buffered_duration_changed(&self, seconds: f64);
}

/// Receives item-level state changes from Rust.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait ItemObserver: Send + Sync {
    fn on_duration_changed(&self, seconds: f64);
    fn on_buffered_duration_changed(&self, seconds: f64);
    fn on_status_changed(&self, status_code: i32);
    fn on_error(&self, code: i32, message: String);
}

/// Callback for seek completion.
#[cfg_attr(feature = "backend-uniffi", uniffi::export(with_foreign))]
pub trait SeekCallback: Send + Sync {
    fn on_complete(&self, finished: bool);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[kithara::test]
    fn observer_traits_are_send_sync() {
        assert_send_sync::<dyn PlayerObserver>();
        assert_send_sync::<dyn ItemObserver>();
        assert_send_sync::<dyn SeekCallback>();
    }
}
