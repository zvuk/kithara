use kithara_audio::SeekOutcome;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_test_utils::kithara;

use super::*;
use crate::{
    api::{Player, PlayerCollector, PlayerComponent, StartAt},
    error::PlayError,
    player::{PlayerConfig, PlayerImpl},
    session::testing,
};

struct Probe {
    calls: Arc<Mutex<Vec<&'static str>>>,
    nested: Vec<MultiPlayer>,
    reentrant_was_blocked: Arc<Mutex<bool>>,
    reentrant_group: Option<Arc<MultiPlayer>>,
    players: Vec<Arc<PlayerImpl>>,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            calls: Arc::default(),
            nested: Vec::new(),
            reentrant_was_blocked: Arc::default(),
            reentrant_group: None,
            players: vec![player_in(&testing::test_session())],
        }
    }
}

impl Probe {
    fn try_reentrant_registration(&self) {
        if let Some(group) = &self.reentrant_group {
            *self.reentrant_was_blocked.lock() = matches!(
                group.register(Self::default()),
                Err(PlayError::PlayerTransactionPending)
            );
        }
    }
}

impl Player for Probe {
    fn duration_seconds(&self) -> Option<f64> {
        Some(10.0)
    }

    fn pause(&self) -> Result<(), PlayError> {
        self.try_reentrant_registration();
        self.calls.lock().push("pause");
        Ok(())
    }

    fn play(&self) -> Result<(), PlayError> {
        self.calls.lock().push("play");
        Ok(())
    }

    fn seek_seconds(&self, _seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.calls.lock().push("seek");
        Ok(SeekOutcome::Landed {
            target: Duration::ZERO,
            landed_at: Duration::ZERO,
        })
    }

    fn start_at(&self, _start: StartAt) -> Result<(), PlayError> {
        self.calls.lock().push("start_at");
        Ok(())
    }
}

impl PlayerComponent for Probe {
    fn collect_players(&self, collector: &mut PlayerCollector<'_>) {
        for nested in &self.nested {
            nested.collect_players(collector);
        }
        collector.extend(self.players.iter().cloned());
        self.try_reentrant_registration();
    }
}

impl From<Probe> for PlayerComponentBox {
    fn from(probe: Probe) -> Self {
        let component: Box<dyn PlayerComponent> = Box::new(probe);
        Self::from(component)
    }
}

struct BoundedProbe {
    calls: Arc<Mutex<Vec<f64>>>,
    duration: f64,
    players: Arc<Mutex<Vec<Arc<PlayerImpl>>>>,
}

impl Player for BoundedProbe {
    fn duration_seconds(&self) -> Option<f64> {
        Some(self.duration)
    }

    fn pause(&self) -> Result<(), PlayError> {
        Ok(())
    }

    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.calls.lock().push(seconds);
        let target = Duration::from_secs_f64(seconds.max(0.0));
        if seconds >= self.duration {
            Ok(SeekOutcome::PastEof {
                target,
                duration: Duration::from_secs_f64(self.duration),
            })
        } else {
            Ok(SeekOutcome::Landed {
                target,
                landed_at: target,
            })
        }
    }

    fn start_at(&self, _start: StartAt) -> Result<(), PlayError> {
        Ok(())
    }
}

impl PlayerComponent for BoundedProbe {
    fn collect_players(&self, collector: &mut PlayerCollector<'_>) {
        collector.extend(self.players.lock().iter().cloned());
    }
}

impl From<BoundedProbe> for PlayerComponentBox {
    fn from(probe: BoundedProbe) -> Self {
        let component: Box<dyn PlayerComponent> = Box::new(probe);
        Self::from(component)
    }
}

fn player_in(session: &Arc<dyn crate::session::SessionDispatcher>) -> Arc<PlayerImpl> {
    Arc::new(PlayerImpl::new(PlayerConfig {
        session: Some(Arc::clone(session)),
        ..PlayerConfig::default()
    }))
}

#[kithara::test]
fn member_routes_through_nested_root() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let inner = MultiPlayer::default();
    let member = inner
        .register(Probe {
            calls: Arc::clone(&calls),
            ..Probe::default()
        })
        .expect("probe registers");
    let outer = MultiPlayer::default();
    outer.register(inner).expect("nested group registers");

    member.play().expect("member routes to the live root");

    assert_eq!(&*calls.lock(), &["start_at"]);
}

#[kithara::test]
fn erased_nested_component_keeps_one_root_transaction_owner() {
    let inner = MultiPlayer::default();
    let member = inner
        .register(Probe::default())
        .expect("probe registers in nested group");
    let outer = MultiPlayer::default();
    let erased: Box<dyn PlayerComponent> = Box::new(inner);
    outer
        .register(erased)
        .expect("erased nested group registers");
    let transaction = outer
        .core
        .begin_control()
        .expect("outer root owns control transaction");

    let error = member
        .pause()
        .expect_err("nested member must route through the outer root");

    assert!(matches!(error, PlayError::PlayerTransactionPending));
    drop(transaction);
}

#[kithara::test]
fn component_with_multiple_nested_roots_is_rejected() {
    let multi = MultiPlayer::default();
    let result = multi.register(Probe {
        nested: vec![MultiPlayer::default(), MultiPlayer::default()],
        ..Probe::default()
    });

    assert!(matches!(
        result,
        Err(PlayError::PlayerComponentTopologyAmbiguous)
    ));
}

#[kithara::test]
fn registration_rejects_players_from_different_sessions() {
    let multi = MultiPlayer::default();
    multi
        .register(PlayerImpl::new(PlayerConfig::default()))
        .expect("first session establishes the group");

    let Err(error) = multi.register(PlayerImpl::new(PlayerConfig::default())) else {
        panic!("a different session must be rejected");
    };

    assert!(matches!(error, PlayError::SessionMismatch));
}

#[kithara::test]
fn registration_rejects_component_without_a_canonical_player() {
    let multi = MultiPlayer::default();

    let result = multi.register(BoundedProbe {
        calls: Arc::new(Mutex::new(Vec::new())),
        duration: 10.0,
        players: Arc::new(Mutex::new(Vec::new())),
    });

    assert!(matches!(result, Err(PlayError::PlayerComponentEmpty)));
}

#[kithara::test]
fn registration_rejects_mixed_sessions_inside_first_component() {
    let players = vec![
        Arc::new(PlayerImpl::new(PlayerConfig::default())),
        Arc::new(PlayerImpl::new(PlayerConfig::default())),
    ];
    let multi = MultiPlayer::default();

    let result = multi.register(Probe {
        players,
        ..Probe::default()
    });

    assert!(matches!(result, Err(PlayError::SessionMismatch)));
}

#[kithara::test]
fn registration_rejects_duplicate_canonical_player() {
    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
    let multi = MultiPlayer::default();

    let result = multi.register(Probe {
        players: vec![Arc::clone(&player), player],
        ..Probe::default()
    });

    assert!(matches!(
        result,
        Err(PlayError::PlayerComponentAlreadyRegistered)
    ));
}

#[kithara::test]
fn registration_advances_typed_topology_revision() {
    let multi = MultiPlayer::default();
    let before = multi.topology_revision();
    let member = multi.register(Probe::default()).expect("probe registers");

    assert_eq!(member.id(), MemberId::FIRST);
    assert_eq!(multi.topology_revision().get(), before.get() + 1);
}

#[kithara::test]
fn registration_freezes_the_validated_canonical_player_leaves() {
    let session = testing::test_session();
    let registered = player_in(&session);
    let replacement = player_in(&session);
    let players = Arc::new(Mutex::new(vec![Arc::clone(&registered)]));
    let multi = MultiPlayer::default();
    multi
        .register(BoundedProbe {
            calls: Arc::new(Mutex::new(Vec::new())),
            duration: 10.0,
            players: Arc::clone(&players),
        })
        .expect("probe registers");
    let revision = multi.topology_revision();

    *players.lock() = vec![replacement];
    let collected = multi.core.collect_players();

    assert_eq!(multi.topology_revision(), revision);
    assert_eq!(collected.len(), 1);
    assert!(Arc::ptr_eq(&collected[0], &registered));
}

#[kithara::test]
fn active_transaction_owns_the_composition_topology() {
    let multi = MultiPlayer::default();
    let transaction = multi.core.begin_seek().expect("seek owns the root");

    let Err(error) = multi.register(Probe::default()) else {
        panic!("topology cannot change during a transaction");
    };
    assert!(matches!(error, PlayError::PlayerTransactionPending));

    drop(transaction);
    multi
        .register(Probe::default())
        .expect("topology is released with the transaction guard");
}

#[kithara::test]
fn registration_calls_component_code_without_holding_the_state_lock() {
    let multi = Arc::new(MultiPlayer::default());
    let reentrant_was_blocked = Arc::new(Mutex::new(false));

    multi
        .register(Probe {
            reentrant_group: Some(Arc::clone(&multi)),
            reentrant_was_blocked: Arc::clone(&reentrant_was_blocked),
            ..Probe::default()
        })
        .expect("outer registration commits");

    assert!(*reentrant_was_blocked.lock());
}

#[kithara::test]
fn routed_control_owns_the_root_topology() {
    let multi = Arc::new(MultiPlayer::default());
    let reentrant_was_blocked = Arc::new(Mutex::new(false));
    let member = multi
        .register(Probe {
            reentrant_group: Some(Arc::clone(&multi)),
            reentrant_was_blocked: Arc::clone(&reentrant_was_blocked),
            ..Probe::default()
        })
        .expect("probe registers");
    *reentrant_was_blocked.lock() = false;

    member.pause().expect("member control routes through root");

    assert!(*reentrant_was_blocked.lock());
}

#[kithara::test]
fn group_seek_preflights_the_shortest_member() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let session = testing::test_session();
    let multi = MultiPlayer::default();
    multi
        .register(BoundedProbe {
            calls: Arc::clone(&calls),
            duration: 10.0,
            players: Arc::new(Mutex::new(vec![player_in(&session)])),
        })
        .expect("long probe registers");
    multi
        .register(BoundedProbe {
            calls: Arc::clone(&calls),
            duration: 5.0,
            players: Arc::new(Mutex::new(vec![player_in(&session)])),
        })
        .expect("short probe registers");

    let outcome = multi.seek_seconds(7.0).expect("range check is typed");

    assert!(matches!(
        outcome,
        SeekOutcome::PastEof { target, duration }
            if target == Duration::from_secs(7) && duration == Duration::from_secs(5)
    ));
    assert!(calls.lock().is_empty());
}
