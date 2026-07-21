use std::{collections::BTreeMap, num::NonZeroU64};

use kithara_platform::sync::{Arc, Mutex, Weak};

use super::component::PlayerComponentBox;
use crate::{
    api::{CollectedComposition, PlayerCollector},
    error::PlayError,
    player::PlayerImpl,
};

/// Stable identity of one direct member in a player composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemberId(NonZeroU64);

impl MemberId {
    pub(super) const FIRST: Self = Self(NonZeroU64::MIN);

    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Numeric identity used by logs and external snapshots.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Revision of a player composition's topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopologyRevision(NonZeroU64);

impl TopologyRevision {
    const FIRST: Self = Self(NonZeroU64::MIN);

    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Numeric revision used by snapshots and diagnostics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone)]
struct ParentRoute {
    member_id: MemberId,
    owner: Weak<MultiPlayerCore>,
}

struct MultiPlayerState {
    components: BTreeMap<MemberId, PlayerComponentBox>,
    next_member_id: MemberId,
    active_transaction: Option<MultiPlayerTransaction>,
    revision: TopologyRevision,
}

impl Default for MultiPlayerState {
    fn default() -> Self {
        Self {
            components: BTreeMap::new(),
            next_member_id: MemberId::FIRST,
            active_transaction: None,
            revision: TopologyRevision::FIRST,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiPlayerTransaction {
    Control,
    Registration,
    Seek,
    Tempo,
}

pub(super) struct TransactionGuard {
    owner: Arc<MultiPlayerCore>,
    kind: MultiPlayerTransaction,
}

impl Drop for TransactionGuard {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock();
        if state.active_transaction == Some(self.kind) {
            state.active_transaction = None;
        }
    }
}

#[derive(Default)]
pub(crate) struct MultiPlayerCore {
    parent: Mutex<Option<ParentRoute>>,
    state: Mutex<MultiPlayerState>,
}

impl MultiPlayerCore {
    fn attach(&self, parent: ParentRoute) -> Result<(), PlayError> {
        let mut current = self.parent.lock();
        if current.is_some() {
            return Err(PlayError::PlayerComponentAlreadyRegistered);
        }
        *current = Some(parent);
        drop(current);
        Ok(())
    }

    pub(super) fn begin_control(self: &Arc<Self>) -> Result<TransactionGuard, PlayError> {
        self.begin_transaction(MultiPlayerTransaction::Control)
    }

    fn begin_registration(self: &Arc<Self>) -> Result<TransactionGuard, PlayError> {
        self.begin_transaction(MultiPlayerTransaction::Registration)
    }

    pub(super) fn begin_seek(self: &Arc<Self>) -> Result<TransactionGuard, PlayError> {
        self.begin_transaction(MultiPlayerTransaction::Seek)
    }

    pub(super) fn begin_tempo(self: &Arc<Self>) -> Result<TransactionGuard, PlayError> {
        self.begin_transaction(MultiPlayerTransaction::Tempo)
    }

    fn begin_transaction(
        self: &Arc<Self>,
        kind: MultiPlayerTransaction,
    ) -> Result<TransactionGuard, PlayError> {
        let mut state = self.state.lock();
        if state.active_transaction.is_some() {
            return Err(PlayError::PlayerTransactionPending);
        }
        state.active_transaction = Some(kind);
        drop(state);
        Ok(TransactionGuard {
            kind,
            owner: Arc::clone(self),
        })
    }

    pub(super) fn collect_players(&self) -> Vec<Arc<PlayerImpl>> {
        let components = self.components();
        Self::collect_players_from(&components)
    }

    pub(super) fn collect_players_from(components: &[PlayerComponentBox]) -> Vec<Arc<PlayerImpl>> {
        let mut players = Vec::new();
        for component in components {
            players.extend(component.players.iter().cloned());
        }
        players
    }

    pub(super) fn components(&self) -> Vec<PlayerComponentBox> {
        self.state
            .lock()
            .components
            .values()
            .map(PlayerComponentBox::clone_for_dispatch)
            .collect()
    }

    pub(super) fn register(
        self: &Arc<Self>,
        mut component: PlayerComponentBox,
    ) -> Result<MemberId, PlayError> {
        let _transaction = self.begin_registration()?;
        let components: Vec<_> = self
            .state
            .lock()
            .components
            .values()
            .map(PlayerComponentBox::clone_for_dispatch)
            .collect();
        let existing = Self::collect_players_from(&components);
        let mut incoming = Vec::new();
        let mut composition = CollectedComposition::None;
        component
            .component
            .collect_players(&mut PlayerCollector::new(
                &mut incoming,
                Some(&mut composition),
            ));
        component.nested = match composition {
            CollectedComposition::None => None,
            CollectedComposition::One(nested) => Some(nested),
            CollectedComposition::Ambiguous => {
                return Err(PlayError::PlayerComponentTopologyAmbiguous);
            }
        };
        validate_registration(&existing, &incoming)?;
        component.players = incoming.into();

        let (member_id, next_member_id, next_revision) = {
            let state = self.state.lock();
            let member_id = state.next_member_id;
            let next_member_id = member_id
                .checked_next()
                .ok_or(PlayError::PlayerMemberIdExhausted)?;
            let next_revision = state
                .revision
                .checked_next()
                .ok_or(PlayError::PlayerTopologyRevisionExhausted)?;
            let next = (member_id, next_member_id, next_revision);
            drop(state);
            next
        };

        if let Some(nested) = &component.nested {
            nested.attach(ParentRoute {
                member_id,
                owner: Arc::downgrade(self),
            })?;
        }
        let mut state = self.state.lock();
        state.components.insert(member_id, component);
        state.next_member_id = next_member_id;
        state.revision = next_revision;
        drop(state);
        Ok(member_id)
    }

    pub(super) fn resolve_root(
        self: &Arc<Self>,
        member_id: MemberId,
    ) -> Result<RoutedMember, PlayError> {
        let mut owner = Arc::clone(self);
        let mut path = vec![member_id];
        loop {
            let parent = owner.parent.lock().clone();
            let Some(parent) = parent else {
                return Ok(RoutedMember { owner, path });
            };
            let parent_owner = parent
                .owner
                .upgrade()
                .ok_or(PlayError::PlayerMemberDetached)?;
            path.insert(0, parent.member_id);
            owner = parent_owner;
        }
    }

    pub(super) fn target(&self, path: &[MemberId]) -> Result<PlayerComponentBox, PlayError> {
        let Some((&member_id, remaining)) = path.split_first() else {
            return Err(PlayError::PlayerMemberDetached);
        };
        let component = self
            .state
            .lock()
            .components
            .get(&member_id)
            .map(PlayerComponentBox::clone_for_dispatch)
            .ok_or_else(|| PlayError::PlayerMemberNotFound {
                member_id: member_id.get(),
            })?;
        if remaining.is_empty() {
            return Ok(component);
        }
        let nested = component
            .nested
            .ok_or_else(|| PlayError::PlayerMemberNotFound {
                member_id: remaining[0].get(),
            })?;
        nested.target(remaining)
    }

    pub(super) fn topology_revision(&self) -> TopologyRevision {
        self.state.lock().revision
    }
}

fn validate_registration(
    existing: &[Arc<PlayerImpl>],
    incoming: &[Arc<PlayerImpl>],
) -> Result<(), PlayError> {
    if incoming.is_empty() {
        return Err(PlayError::PlayerComponentEmpty);
    }
    for (index, player) in incoming.iter().enumerate() {
        if existing
            .iter()
            .chain(&incoming[..index])
            .any(|registered| Arc::ptr_eq(registered, player))
        {
            return Err(PlayError::PlayerComponentAlreadyRegistered);
        }
    }

    let mut players = existing.iter().chain(incoming);
    let Some(owner) = players.next() else {
        return Ok(());
    };
    if players.any(|player| !owner.engine().shares_session_with(player.engine())) {
        return Err(PlayError::SessionMismatch);
    }
    Ok(())
}

pub(super) struct RoutedMember {
    pub(super) owner: Arc<MultiPlayerCore>,
    pub(super) path: Vec<MemberId>,
}
