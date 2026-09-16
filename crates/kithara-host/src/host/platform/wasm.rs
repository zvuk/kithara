use std::{collections::HashMap, mem, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_platform::sync::{Arc, Mutex};
use kithara_play::{
    GroupState, PlayError,
    effects::LimiterConfig,
    player::{PlayerControlSource, PlayerMember},
};
use kithara_warp::{
    BeatGridId, SyncAdmission, SyncCapability, SyncError, SyncOperation, SyncRejected,
};

use super::super::{Host, HostOwned, SessionRuntime};
use crate::{
    session::{HostDispatcher, RootView, web::WebSessionState},
    wasm::HostRoute,
};
type Resident = Box<dyn FnMut() -> Result<(), PlayError>>;
type StartedPlatform<S> = (Arc<dyn HostDispatcher<S>>, Platform<S>);

pub(in crate::host) struct Platform<S> {
    remote_routes: Mutex<Vec<Arc<HostRoute<S>>>>,
    remote_residents: Option<HashMap<BeatGridId, Resident>>,
    web_state: Option<WebSessionState<S>>,
}

impl<S> Platform<S> {
    pub(in crate::host) fn close(platform: &mut Self, host_id: BeatGridId) {
        for route in mem::take(&mut *platform.remote_routes.lock()) {
            route.close();
        }
        if let Some(residents) = platform.remote_residents.take()
            && !residents.is_empty()
        {
            let resident_count = residents.len();
            for resident in residents.into_values() {
                mem::forget(resident);
            }
            tracing::error!(
                ?host_id,
                resident_count,
                "remote wasm Host dropped before its players detached; retaining residents"
            );
        }
    }

    fn close_resident(&mut self, id: BeatGridId) -> Result<(), PlayError> {
        let resident = self
            .remote_residents
            .as_mut()
            .and_then(|residents| residents.get_mut(&id))
            .ok_or_else(|| PlayError::Internal("attached wasm player lost its owner".into()))?;
        resident()
    }

    fn insert_resident(
        &mut self,
        id: BeatGridId,
        resident: Resident,
    ) -> Result<Option<Resident>, PlayError> {
        self.remote_residents
            .as_mut()
            .ok_or_else(|| {
                PlayError::Internal("wasm Worker resident registry is unavailable".into())
            })
            .map(|residents| residents.insert(id, resident))
    }

    #[cfg(feature = "offline")]
    pub(in crate::host) fn offline() -> Result<Self, PlayError> {
        if kithara_platform::thread::is_main_thread() {
            return Err(PlayError::SessionCategoryUnsupported {
                reason: "offline Host must run in a Web Worker".to_owned(),
            });
        }
        Ok(Self::remote())
    }

    pub(in crate::host) fn owner(web_state: WebSessionState<S>) -> Self {
        Self {
            web_state: Some(web_state),
            remote_routes: Mutex::default(),
            remote_residents: None,
        }
    }

    pub(in crate::host) fn realtime(
        group: GroupState<PlayerMember>,
        view: RootView,
        sample_rate: NonZeroU32,
        _output_block_frames: Option<NonZeroU32>,
        limiter: LimiterConfig,
    ) -> Result<StartedPlatform<S>, PlayError>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let (dispatcher, web_state) =
            crate::session::web::spawn::<S>(group, view, sample_rate, limiter)?;
        Ok((dispatcher, Self::owner(web_state)))
    }

    fn release_on_session_gone<T>(
        &mut self,
        id: BeatGridId,
        result: Result<T, PlayError>,
    ) -> Result<T, PlayError> {
        match result {
            Err(error @ PlayError::SessionGone { .. }) => {
                self.release_resident(id)?;
                Err(error)
            }
            result => result,
        }
    }

    fn release_resident(&mut self, id: BeatGridId) -> Result<(), PlayError> {
        let resident = self
            .remote_residents
            .as_mut()
            .and_then(|residents| residents.remove(&id))
            .ok_or_else(|| {
                PlayError::Internal("detached wasm player lost its Worker resident".into())
            })?;
        drop(resident);
        Ok(())
    }

    fn remote() -> Self {
        Self {
            web_state: None,
            remote_routes: Mutex::default(),
            remote_residents: Some(HashMap::new()),
        }
    }

    fn require_remote(&self) -> Result<(), PlayError> {
        if self.remote_residents.is_some() {
            return Ok(());
        }
        Err(PlayError::Internal(
            "wasm players must be inserted from their owning Worker".into(),
        ))
    }

    pub(in crate::host) fn transact(
        _platform: &Self,
        dispatcher: &Arc<dyn HostDispatcher<S>>,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        if matches!(&operation, SyncOperation::Topology { .. }) {
            return Err(SyncRejected::new(
                SyncError::CapabilityUnavailable {
                    capability: SyncCapability::Topology,
                },
                operation,
            ));
        }
        dispatcher.transact(operation)
    }
}

impl<S> Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Attaches and transfers one fully configured player or decorator into
    /// this Host, then prepares its graph and initial slot before returning.
    /// Audio-device setup may block; musical playback remains stopped.
    ///
    /// # Errors
    /// Returns an error when binding, attachment, or graph preparation fails.
    pub fn insert<P>(&mut self, mut player: P) -> Result<HostOwned<P>, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        self.session.platform().require_remote()?;
        let (grid_id, control) = self.bind_player(&mut player)?;
        let member = player.take_host_member()?;
        self.attach_member(member)?;
        let resident: Resident = Box::new(move || player.close());
        if let Some(replaced) = self
            .session
            .platform_mut()
            .insert_resident(grid_id, resident)?
        {
            mem::forget(replaced);
            return Err(PlayError::Internal(
                "wasm player residence changed during insertion".into(),
            ));
        }
        let owned = self.owned::<P>(grid_id, control);
        if let Err(error) = P::prepare_control(owned.control()) {
            self.remove(&owned)?;
            return Err(error);
        }
        Ok(owned)
    }

    pub(crate) fn register_remote_route(&self, route: Arc<HostRoute<S>>) {
        self.session.platform().remote_routes.lock().push(route);
    }

    pub(crate) fn remote(
        id: BeatGridId,
        root_view: RootView,
        dispatcher: Arc<dyn HostDispatcher<S>>,
    ) -> Self {
        Self {
            id,
            root_view,
            dispatcher,
            owns_session: false,
            session: SessionRuntime::realtime(Platform::remote()),
        }
    }

    pub(crate) fn remote_identity(&self) -> (BeatGridId, RootView) {
        (self.id, self.root_view.clone())
    }

    /// Closes the lower runtime on the caller thread, then detaches its
    /// canonical member after graph unregistration has completed.
    ///
    /// # Errors
    /// Returns an error when close or canonical detachment fails.
    pub fn remove<P>(&mut self, player: &HostOwned<P>) -> Result<(), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        self.validate_removal(player)?;
        self.remove_resident(player.id())
    }

    fn remove_resident(&mut self, id: BeatGridId) -> Result<(), PlayError> {
        let close_result = self.session.platform_mut().close_resident(id);
        self.session
            .platform_mut()
            .release_on_session_gone(id, close_result)?;
        let detach_result = self.detach_member(id);
        self.session
            .platform_mut()
            .release_on_session_gone(id, detach_result)?;
        self.session.platform_mut().release_resident(id)
    }

    pub(crate) fn web_state(&self) -> Option<&WebSessionState<S>> {
        self.session.platform().web_state.as_ref()
    }
}

#[cfg(test)]
#[path = "wasm_tests.rs"]
mod tests;
