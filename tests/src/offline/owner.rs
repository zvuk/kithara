//! Owner of a Host for the offline harness: a dedicated thread natively, the
//! calling thread on wasm, where the caller is already the worker the Host
//! belongs to.

#[cfg(target_arch = "wasm32")]
pub(super) use inline::InlineOwner as HostOwner;
#[cfg(not(target_arch = "wasm32"))]
pub(super) use kithara_test_utils::off_thread::OffThread as HostOwner;

#[cfg(target_arch = "wasm32")]
mod inline {
    use kithara::platform::sync::Mutex;

    pub(in crate::offline) struct InlineOwner<T>(Mutex<T>);

    impl<T: 'static> InlineOwner<T> {
        pub(in crate::offline) async fn spawn<E, I>(_name: &'static str, init: I) -> Result<Self, E>
        where
            I: FnOnce() -> Result<T, E>,
        {
            init().map(|value| Self(Mutex::new(value)))
        }

        pub(in crate::offline) async fn call<R, F>(&self, job: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            job(&mut self.0.lock())
        }

        pub(in crate::offline) async fn close(self) {}
    }
}
