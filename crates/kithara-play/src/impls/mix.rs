use kithara_platform::sync::Arc;

use super::{engine::EngineImpl, player::PlayerImpl, session::PlayerLevel};
use crate::error::PlayError;

/// Actuate the final linear levels of a group of players in one session command.
/// Invalid input is rejected, never clamped, and moves no gain.
///
/// The session is taken from the first input; every other player must share it.
/// Players that have never started are accepted - their level is stored for the
/// first `start` to adopt.
///
/// # Errors
/// Returns [`PlayError`] on validation failure ([`PlayError::MixLevel`],
/// [`PlayError::MixForeignSession`], [`PlayError::MixDuplicatePlayer`]) or on
/// dispatch failure.
pub fn apply_mix<'a>(
    inputs: impl IntoIterator<Item = (&'a PlayerImpl, f32)>,
) -> Result<(), PlayError> {
    let inputs: Vec<(&EngineImpl, f32)> = inputs
        .into_iter()
        .map(|(player, level)| (player.engine(), level))
        .collect();
    let Some(&(first, _)) = inputs.first() else {
        return Ok(());
    };
    let session = first.session_dispatcher();

    for &(_, level) in &inputs {
        if !level.is_finite() || !(0.0..=1.0).contains(&level) {
            return Err(PlayError::MixLevel { level });
        }
    }
    for &(engine, _) in &inputs {
        if !Arc::ptr_eq(engine.session_dispatcher(), session) {
            return Err(PlayError::MixForeignSession);
        }
    }
    for (i, &(engine, _)) in inputs.iter().enumerate() {
        let addr = std::ptr::from_ref(engine);
        if inputs[..i]
            .iter()
            .any(|&(other, _)| std::ptr::from_ref(other) == addr)
        {
            return Err(PlayError::MixDuplicatePlayer);
        }
    }

    // Stable address order: two batches sharing players cannot deadlock.
    let mut ordered: Vec<&EngineImpl> = inputs.iter().map(|&(engine, _)| engine).collect();
    ordered.sort_by_key(|engine| std::ptr::from_ref(*engine) as usize);
    let _guards: Vec<_> = ordered
        .iter()
        .map(|engine| engine.start_lock().lock())
        .collect();

    let levels: Vec<PlayerLevel> = inputs
        .iter()
        .filter_map(|&(engine, level)| {
            engine
                .registered_player_id()
                .map(|id| PlayerLevel::new(id, level))
        })
        .collect();

    session.set_player_master_volumes(levels)?;

    for &(engine, level) in &inputs {
        engine.commit_desired_master_volume(level);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        impls::{
            player::{PlayerConfig, PlayerImpl},
            session::{Cmd, Reply, SessionDispatcher, testing::test_session},
        },
        traits::engine::Engine,
    };

    struct ForeignSession;

    impl SessionDispatcher for ForeignSession {
        fn exec(&self, _cmd: Cmd) -> Result<Reply, PlayError> {
            Ok(Reply::Ok)
        }
    }

    fn player(session: Arc<dyn SessionDispatcher>) -> PlayerImpl {
        PlayerImpl::new(PlayerConfig::builder().session(session).build())
    }

    #[kithara::test]
    fn apply_updates_desired_levels_for_two_players() {
        let session = test_session();
        let a = player(session.clone());
        let b = player(session.clone());
        a.engine().start().unwrap();
        b.engine().start().unwrap();

        apply_mix([(&a, 0.25), (&b, 0.75)]).unwrap();
        assert_eq!(a.engine().master_volume(), 0.25);
        assert_eq!(b.engine().master_volume(), 0.75);
    }

    #[kithara::test]
    fn apply_rejects_duplicate_player_without_mutation() {
        let session = test_session();
        let a = player(session.clone());
        a.engine().start().unwrap();

        let err = apply_mix([(&a, 0.3), (&a, 0.4)]).unwrap_err();
        assert!(matches!(err, PlayError::MixDuplicatePlayer));
        assert_eq!(a.engine().master_volume(), 1.0);
    }

    #[kithara::test]
    fn apply_rejects_invalid_level_without_mutation() {
        let session = test_session();
        let a = player(session.clone());
        a.engine().start().unwrap();

        for bad in [f32::NAN, f32::INFINITY, 1.5, -0.1] {
            let err = apply_mix([(&a, bad)]).unwrap_err();
            assert!(matches!(err, PlayError::MixLevel { .. }));
            assert_eq!(a.engine().master_volume(), 1.0);
        }
    }

    #[kithara::test]
    fn apply_rejects_foreign_session() {
        let session = test_session();
        let a = player(session.clone());
        a.engine().start().unwrap();
        let foreign = player(Arc::new(ForeignSession) as Arc<dyn SessionDispatcher>);

        let err = apply_mix([(&a, 0.5), (&foreign, 0.5)]).unwrap_err();
        assert!(matches!(err, PlayError::MixForeignSession));
        assert_eq!(a.engine().master_volume(), 1.0);
    }

    #[kithara::test]
    fn never_started_player_receives_desired_before_first_start() {
        let session = test_session();
        let a = player(session.clone());
        apply_mix([(&a, 0.2)]).unwrap();
        assert_eq!(a.engine().master_volume(), 0.2);

        a.engine().start().unwrap();
        assert_eq!(
            a.engine().master_volume(),
            0.2,
            "start must adopt the desired level stored before registration"
        );
    }

    #[kithara::test]
    fn stopped_player_retains_level_on_restart() {
        let session = test_session();
        let a = player(session.clone());
        a.engine().start().unwrap();

        apply_mix([(&a, 0.4)]).unwrap();
        a.engine().stop().unwrap();
        assert_eq!(a.engine().master_volume(), 0.4);

        a.engine().start().unwrap();
        assert_eq!(a.engine().master_volume(), 0.4);
    }

    #[kithara::test]
    fn empty_batch_is_noop() {
        let session = test_session();
        let a = player(session.clone());
        a.engine().start().unwrap();
        let empty: [(&PlayerImpl, f32); 0] = [];
        apply_mix(empty).unwrap();
        assert_eq!(a.engine().master_volume(), 1.0);
    }
}
