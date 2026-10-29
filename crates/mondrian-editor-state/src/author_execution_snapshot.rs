//! Read-only execution admission issued by the canonical Authoring Session.
//!
//! This gate stores no document, timeline, Undo record or independent generation
//! counter. The sole Session holds one writer across canonical document, History,
//! revision and generation publication, before the first author state change. A bounded execution install keeps a read guard only
//! across its final current-root CAS and infallible ownership move.

use crate::AuthorGeneration;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) struct AuthorExecutionSnapshotOwner {
    state: Arc<RwLock<Option<AuthorGeneration>>>,
}

impl AuthorExecutionSnapshotOwner {
    pub(crate) fn new(generation: AuthorGeneration) -> Self {
        Self { state: Arc::new(RwLock::new(Some(generation))) }
    }

    pub(crate) fn snapshot(&self, generation: AuthorGeneration) -> AuthorExecutionSnapshot {
        AuthorExecutionSnapshot { state: Arc::clone(&self.state), generation }
    }

    pub(crate) fn revoke(&self) {
        match self.state.write() {
            Ok(mut state) => *state = None,
            Err(poison) => *poison.into_inner() = None,
        }
    }

    pub(crate) fn begin_publication(&self) -> AuthorExecutionPublication<'_> {
        // Take this writer only after all validation, allocation and cache
        // preparation, but before changing any canonical author field.
        match self.state.write() {
            Ok(state) => AuthorExecutionPublication { state, poisoned: false },
            Err(poison) => {
                AuthorExecutionPublication { state: poison.into_inner(), poisoned: true }
            }
        }
    }
}

/// Local writer spanning the complete infallible canonical publication.
/// Retired documents/History, logs and callbacks must remain outside its scope.
pub(crate) struct AuthorExecutionPublication<'a> {
    state: RwLockWriteGuard<'a, Option<AuthorGeneration>>,
    poisoned: bool,
}

impl AuthorExecutionPublication<'_> {
    pub(crate) fn publish(&mut self, canonical: &mut AuthorGeneration, next: AuthorGeneration) {
        // Poison refuses every execution admission, but cannot prevent an
        // already validated author transaction from publishing coherently.
        *canonical = next;
        *self.state = if self.poisoned { None } else { Some(next) };
    }
}

impl Drop for AuthorExecutionSnapshotOwner {
    fn drop(&mut self) {
        self.revoke();
    }
}

/// Opaque read-only snapshot from one actual Authoring Session lifetime.
///
/// No constructor, Clone, Deserialize or generation setter exists. Every use
/// requires current admission: edit, Undo, Redo or closing the Session revokes
/// the old generation. This is permission metadata, not an authored snapshot
/// or a native/runtime/editor ownership ticket.
#[derive(Debug)]
pub struct AuthorExecutionSnapshot {
    state: Arc<RwLock<Option<AuthorGeneration>>>,
    generation: AuthorGeneration,
}

impl AuthorExecutionSnapshot {
    /// Canonical generation captured by the issuing Authoring Session.
    pub const fn generation(&self) -> AuthorGeneration {
        self.generation
    }

    /// Serialize a bounded execution installation against author publication.
    ///
    /// Acquire after all allocation/native/foreign preflight. Keep this guard
    /// only across final current-root admission and infallible ownership moves.
    /// No callback, author mutation, rendering, queue wait, foreign destruction
    /// or nested acquisition belongs inside it. The guard stays on this thread.
    pub fn enter_commit(&self) -> Result<AuthorExecutionCommit<'_>, AuthorExecutionAdmissionError> {
        let state = self.state.read().map_err(|_| AuthorExecutionAdmissionError::Poisoned)?;
        match *state {
            Some(current) if current == self.generation => {
                Ok(AuthorExecutionCommit { _state: state })
            }
            Some(_) => Err(AuthorExecutionAdmissionError::Superseded),
            None => Err(AuthorExecutionAdmissionError::SessionClosed),
        }
    }
}

/// Local non-Send admission retained only during one bounded execution commit.
/// Dropping it grants no ownership and performs no mutation or foreign work.
#[must_use]
pub struct AuthorExecutionCommit<'a> {
    _state: RwLockReadGuard<'a, Option<AuthorGeneration>>,
}

/// Canonical author execution generation no longer admits this installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthorExecutionAdmissionError {
    /// A real author transaction, Undo or Redo superseded the captured snapshot.
    #[error("author execution snapshot was superseded")]
    Superseded,
    /// Closing the original Session revoked its execution permission.
    #[error("author execution session is closed")]
    SessionClosed,
    /// An incomplete host commit cannot issue execution permission.
    #[error("author execution admission is poisoned")]
    Poisoned,
}
