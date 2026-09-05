use std::{cell::RefCell, num::NonZeroU32, rc::Rc};

use delegate::delegate;
use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::testing::TestPools;
use kithara_platform::sync::Arc;
use kithara_play::{GroupState, PlayError, SessionDispatcher, player::PlayerMember};
use kithara_test_utils::kithara;
use kithara_warp::{
    BeatGridId, SyncAdmission, SyncGroup, SyncMember, SyncOperation, TopologyOperation,
};

use super::{Host, Platform, Resident, SessionRuntime};
use crate::{
    host::SessionRoot,
    session::{
        HostCmd, HostDispatcher, HostReply, Reply,
        protocol::{HostDispatchError, SyncCmd},
        testing::{FixtureSession, fixture_member},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    SessionGone,
    OtherError,
}

struct ResidentProbe {
    close: Outcome,
    drops: Rc<RefCell<usize>>,
}

impl Drop for ResidentProbe {
    fn drop(&mut self) {
        *self.drops.borrow_mut() += 1;
    }
}

impl ResidentProbe {
    fn close(&mut self) -> Result<(), PlayError> {
        match self.close {
            Outcome::Ok => Ok(()),
            Outcome::SessionGone => Err(PlayError::SessionGone {
                reason: "fixture resident close",
            }),
            Outcome::OtherError => Err(PlayError::Internal("fixture resident close failed".into())),
        }
    }
}

fn resident(close: Outcome, drops: Rc<RefCell<usize>>) -> Resident {
    let mut probe = ResidentProbe { close, drops };
    Box::new(move || probe.close())
}

struct Dispatcher {
    session: FixtureSession,
    root: RefCell<GroupState<PlayerMember>>,
    detach: Outcome,
}

impl SessionDispatcher<TestPools> for Dispatcher {
    delegate! {
        to &self.session {
            #[through(SessionDispatcher::<TestPools>)]
            fn exec(&self, cmd: kithara_play::Cmd<TestPools>) -> Result<Reply, PlayError>;
            #[through(SessionDispatcher::<TestPools>)]
            fn consumer_wake_mode(&self) -> ConsumerWakeMode;
        }
    }
}

impl HostDispatcher<TestPools> for Dispatcher {
    fn exec_host(
        &self,
        cmd: HostCmd<TestPools>,
    ) -> Result<HostReply, HostDispatchError<TestPools>> {
        let HostCmd::Sync(SyncCmd::TransactCurrent(operations)) = cmd else {
            panic!("unexpected fixture Host command")
        };
        match self.detach {
            Outcome::SessionGone => Err(HostDispatchError::before_send(
                PlayError::SessionGone {
                    reason: "fixture detach",
                },
                HostCmd::Sync(SyncCmd::TransactCurrent(operations)),
            )),
            Outcome::OtherError => Ok(HostReply::Err(PlayError::Internal(
                "fixture detach failed".into(),
            ))),
            Outcome::Ok => {
                let mut root = self.root.borrow_mut();
                let base = root.topology().expect("fixture topology").stamp();
                Ok(HostReply::Admission(
                    root.transact(SyncOperation::Topology { base, operations }),
                ))
            }
        }
    }
}

fn fixture(close: Outcome, detach: Outcome) -> (Host<TestPools>, BeatGridId, Rc<RefCell<usize>>) {
    let SessionRoot {
        id: host_id,
        sample_rate,
        group: mut root,
        view: root_view,
    } = Host::<TestPools>::session_root(NonZeroU32::new(44_100).expect("fixture sample rate"))
        .expect("fixture Host session");
    let resident_id = BeatGridId::allocate().expect("fixture resident grid id");
    let base = root.topology().expect("fixture root topology").stamp();
    let admission = root
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Group {
                    alignment: None,
                    group: Box::new(fixture_member(resident_id, sample_rate)),
                },
            }]),
        })
        .expect("fixture resident attachment");
    assert!(matches!(admission, SyncAdmission::TopologyChanged { .. }));

    let dispatcher: Arc<dyn HostDispatcher<TestPools>> = Arc::new(Dispatcher {
        session: FixtureSession,
        root: RefCell::new(root),
        detach,
    });
    let drops = Rc::new(RefCell::new(0));
    let mut platform = Platform::remote();
    let replaced = platform
        .insert_resident(resident_id, resident(close, Rc::clone(&drops)))
        .expect("fixture resident registry");
    assert!(replaced.is_none());
    let host = Host {
        id: host_id,
        owns_session: false,
        root_view,
        dispatcher,
        session: SessionRuntime::realtime(platform),
    };
    (host, resident_id, drops)
}

#[kithara::test(wasm, flash(false))]
fn successful_remove_releases_resident() {
    let (mut host, resident, drops) = fixture(Outcome::Ok, Outcome::Ok);

    host.remove_resident(resident).expect("remove resident");

    assert_eq!(*drops.borrow(), 1);
}

#[kithara::test(wasm, flash(false))]
#[case::closing(Outcome::SessionGone, Outcome::Ok)]
#[case::detaching(Outcome::Ok, Outcome::SessionGone)]
fn session_gone_releases_resident(#[case] close: Outcome, #[case] detach: Outcome) {
    let (mut host, resident, drops) = fixture(close, detach);

    assert!(matches!(
        host.remove_resident(resident),
        Err(PlayError::SessionGone { .. })
    ));
    assert_eq!(*drops.borrow(), 1);
}

#[kithara::test(wasm, flash(false))]
fn other_errors_retain_resident() {
    for (close, detach) in [
        (Outcome::OtherError, Outcome::Ok),
        (Outcome::Ok, Outcome::OtherError),
    ] {
        let (mut host, resident, drops) = fixture(close, detach);

        assert!(matches!(
            host.remove_resident(resident),
            Err(PlayError::Internal(_))
        ));
        assert_eq!(*drops.borrow(), 0);
        assert!(
            host.session
                .platform()
                .remote_residents
                .as_ref()
                .is_some_and(|residents| residents.contains_key(&resident))
        );
        drop(host);
        assert_eq!(*drops.borrow(), 0);
    }
}
