//! Non-RT current-renderer installation permission across execution adapters.
//!
//! The actual Media owner retains the only mutable source. A concrete Runtime
//! binds one read-only registration before installation; an unrelated pair
//! cannot grant permission to that already-bound Runtime. This gate contains
//! no graph, authored value, native state, Undo history or execution schedule.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard};

/// A rejected installation or stale observation, before any native author work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AudioOwnerInstallationError {
    /// A request belongs to another Media installation source or registration.
    #[error("audio installation belongs to another registered owner")]
    WrongRegistration,
    /// The registered renderer has not been installed into the actual slot.
    #[error("audio renderer is not currently installed")]
    NotInstalled,
    /// Another renderer or installation generation replaced this claim.
    #[error("audio renderer installation was superseded")]
    Superseded,
    /// A one-shot actual Runtime installation authorization was already used.
    #[error("audio Runtime installation authorization was consumed")]
    ConsumedAuthorization,
    /// Monotonic identities cannot be reused after their integer space ends.
    #[error("audio installation identity space is exhausted")]
    SerialExhausted,
    /// An incomplete host critical section cannot grant current ownership.
    #[error("audio installation state is poisoned")]
    Poisoned,
}

#[derive(Debug)]
struct InstallationState {
    current: Option<(NonZeroU64, NonZeroU64)>,
    next_serial: u64,
}

/// The single non-Clone issuer retained privately by the actual Media owner.
///
/// Callers create a fresh unrelated source for their own renderer slot; they
/// cannot manufacture a claim accepted by another Runtime's bound verifier.
/// Registering and reserving a commit grant no installed permission. The host
/// commits only alongside its actual current-renderer slot exchange, without
/// calling foreign code while the short installation guard is held.
#[derive(Debug)]
pub struct AudioOwnerInstallationSource {
    state: Arc<Mutex<InstallationState>>,
}

impl AudioOwnerInstallationSource {
    /// Create one installation source for one concrete owner slot.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(InstallationState {
                current: None,
                next_serial: 1,
            })),
        }
    }

    fn reserve_serial(
        state: &mut InstallationState,
    ) -> Result<NonZeroU64, AudioOwnerInstallationError> {
        let serial = NonZeroU64::new(state.next_serial)
            .ok_or(AudioOwnerInstallationError::SerialExhausted)?;
        state.next_serial = state
            .next_serial
            .checked_add(1)
            .ok_or(AudioOwnerInstallationError::SerialExhausted)?;
        Ok(serial)
    }

    /// Register exactly one actual renderer/Runtime without marking it installed.
    /// The concrete adapter binds the returned verifier once to that Runtime.
    pub fn register(
        &self,
    ) -> Result<
        (
            AudioOwnerInstallationRegistration,
            AudioOwnerInstallationVerifier,
        ),
        AudioOwnerInstallationError,
    > {
        let mut state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        let serial = Self::reserve_serial(&mut state)?;
        Ok((
            AudioOwnerInstallationRegistration { state: Arc::clone(&self.state), serial },
            AudioOwnerInstallationVerifier { state: Arc::clone(&self.state), serial },
        ))
    }

    /// Check registration provenance without granting permission or mutating it.
    pub fn owns_registration(&self, registration: &AudioOwnerInstallationRegistration) -> bool {
        Arc::ptr_eq(&self.state, &registration.state)
    }

    /// Reserve all fallible identity arithmetic before foreign preflight.
    /// No lock is retained and no installed permission is created. A failed
    /// output/native preflight can discard this reservation with the old slot
    /// and old claim unchanged. Serial gaps are intentional and never reused.
    pub fn reserve_install(
        &self,
        registration: &AudioOwnerInstallationRegistration,
        expected_current: Option<&AudioOwnerInstallationClaim>,
    ) -> Result<AudioOwnerInstallationReservation, AudioOwnerInstallationError> {
        if !self.owns_registration(registration) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        if expected_current.is_some_and(|claim| !Arc::ptr_eq(&self.state, &claim.state)) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let mut state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        let expected = expected_current.map(|claim| (claim.registration, claim.generation));
        if state.current != expected {
            return Err(AudioOwnerInstallationError::Superseded);
        }
        let generation = Self::reserve_serial(&mut state)?;
        Ok(AudioOwnerInstallationReservation {
            source: Arc::clone(&self.state),
            registration: Some(registration.serial),
            generation,
            expected,
        })
    }

    /// Reserve one actual Runtime publication through this same private Source.
    ///
    /// This consumes a previously reserved registered replacement and exposes
    /// no source, registration, revocation or issuer API to the provider. It is
    /// not an installed claim. The concrete provider must hold its actual old
    /// Runtime and acquire the final guard beside the infallible real swap.
    pub fn authorize_runtime_installation(
        &self,
        reservation: AudioOwnerInstallationReservation,
    ) -> Result<AudioOwnerRuntimeInstallationAuthorization, AudioOwnerInstallationError> {
        if !Arc::ptr_eq(&self.state, &reservation.source) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let registration =
            reservation.registration.ok_or(AudioOwnerInstallationError::WrongRegistration)?;
        Ok(AudioOwnerRuntimeInstallationAuthorization {
            source: reservation.source,
            registration,
            generation: reservation.generation,
            expected: reservation.expected,
            available: true,
        })
    }

    /// Reserve an actual clear or a renderer without a native owner consumer.
    /// This grants no permission and keeps the previous claim current if any
    /// foreign output preflight fails. Publication occurs beside the slot swap.
    pub fn reserve_clear(
        &self,
        expected_current: Option<&AudioOwnerInstallationClaim>,
    ) -> Result<AudioOwnerInstallationReservation, AudioOwnerInstallationError> {
        if expected_current.is_some_and(|claim| !Arc::ptr_eq(&self.state, &claim.state)) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let mut state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        let expected = expected_current.map(|claim| (claim.registration, claim.generation));
        if state.current != expected {
            return Err(AudioOwnerInstallationError::Superseded);
        }
        let generation = Self::reserve_serial(&mut state)?;
        Ok(AudioOwnerInstallationReservation {
            source: Arc::clone(&self.state),
            registration: None,
            generation,
            expected,
        })
    }

    /// Acquire the short publication guard locally beside the actual slot swap.
    /// The expected old slot is checked again after all foreign preflight, so
    /// stale worker reservations cannot overwrite a newer UI installation.
    /// No callbacks, queues, foreign drop or native work belong inside the guard.
    pub fn begin_commit(
        &self,
        reservation: AudioOwnerInstallationReservation,
    ) -> Result<AudioOwnerInstallationCommit<'_>, AudioOwnerInstallationError> {
        if !Arc::ptr_eq(&self.state, &reservation.source) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        if state.current != reservation.expected {
            return Err(AudioOwnerInstallationError::Superseded);
        }
        Ok(AudioOwnerInstallationCommit {
            state,
            source: reservation.source,
            registration: reservation.registration,
            generation: reservation.generation,
        })
    }

    /// Observe only the already-published current slot, without installing it.
    /// A later swap may supersede this claim before its next exact admission.
    pub fn current_claim(
        &self,
    ) -> Result<Option<AudioOwnerInstallationClaim>, AudioOwnerInstallationError> {
        let state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        Ok(
            state.current.map(|(registration, generation)| AudioOwnerInstallationClaim {
                state: Arc::clone(&self.state),
                registration,
                generation,
            }),
        )
    }

    /// Revoke the currently installed slot before clearing or retiring it.
    pub fn revoke(&self) -> Result<(), AudioOwnerInstallationError> {
        self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?.current = None;
        Ok(())
    }
}

impl Default for AudioOwnerInstallationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioOwnerInstallationSource {
    fn drop(&mut self) {
        // Retained read-only claims cannot outlive their actual installation
        // issuer. Poison is already a refusal; teardown still clears the slot.
        match self.state.lock() {
            Ok(mut state) => state.current = None,
            Err(poison) => poison.into_inner().current = None,
        }
    }
}

/// Opaque registration moved or cloned only with its actual renderer owner.
/// It cannot mint claims; no public constructor or deserialization exists.
#[derive(Debug, Clone)]
pub struct AudioOwnerInstallationRegistration {
    state: Arc<Mutex<InstallationState>>,
    serial: NonZeroU64,
}

/// Read-only verifier bound once to the actual registered Audio Runtime.
/// Cloning it never acquires the issuer or permission to change installations.
#[derive(Debug, Clone)]
pub struct AudioOwnerInstallationVerifier {
    state: Arc<Mutex<InstallationState>>,
    serial: NonZeroU64,
}

impl AudioOwnerInstallationVerifier {
    /// Read-only registration of this exact Runtime; it grants no installation.
    /// Only the private actual slot Source can reserve/commit a matching claim.
    pub fn registration(&self) -> AudioOwnerInstallationRegistration {
        AudioOwnerInstallationRegistration {
            state: Arc::clone(&self.state),
            serial: self.serial,
        }
    }

    /// Observe the already-installed generation only for this exact Runtime.
    /// Registration alone cannot create a claim or make a new generation live.
    pub fn current_claim(
        &self,
    ) -> Result<AudioOwnerInstallationClaim, AudioOwnerInstallationError> {
        let state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        match state.current {
            Some((registration, generation)) if registration == self.serial => {
                Ok(AudioOwnerInstallationClaim {
                    state: Arc::clone(&self.state),
                    registration,
                    generation,
                })
            }
            Some(_) => Err(AudioOwnerInstallationError::Superseded),
            None => Err(AudioOwnerInstallationError::NotInstalled),
        }
    }

    /// Verify this Runtime's registration and the currently installed generation.
    /// This is a bounded non-RT control check, never a per-sample/DSP operation.
    pub fn validate(
        &self,
        claim: &AudioOwnerInstallationClaim,
    ) -> Result<(), AudioOwnerInstallationError> {
        if self.serial != claim.registration || !Arc::ptr_eq(&self.state, &claim.state) {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let state = self.state.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        match state.current {
            Some((registration, generation))
                if registration == self.serial && generation == claim.generation =>
            {
                Ok(())
            }
            Some(_) => Err(AudioOwnerInstallationError::Superseded),
            None => Err(AudioOwnerInstallationError::NotInstalled),
        }
    }

    /// Compare the one Media installation Source across distinct reservations.
    /// This read-only relationship never grants installed/native/author permission.
    pub fn same_source(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Determine whether another read-only verifier describes this exact registration.
    /// It does not make either verifier installed or current.
    pub fn same_registration(&self, other: &Self) -> bool {
        self.serial == other.serial && Arc::ptr_eq(&self.state, &other.state)
    }
}

/// Non-Clone preflight reservation with no installed/native/author permission.
/// There is no public constructor or deserialization. Only its issuing Source
/// can consume it, while comparing the exact still-current previous slot.
#[derive(Debug)]
pub struct AudioOwnerInstallationReservation {
    source: Arc<Mutex<InstallationState>>,
    registration: Option<NonZeroU64>,
    generation: NonZeroU64,
    expected: Option<(NonZeroU64, NonZeroU64)>,
}

/// A short non-Send publication guard for the actual current-renderer swap.
/// Abandoning it never replaces the old installed owner; it calls no foreign code.
#[must_use]
#[derive(Debug)]
pub struct AudioOwnerInstallationCommit<'a> {
    state: MutexGuard<'a, InstallationState>,
    source: Arc<Mutex<InstallationState>>,
    registration: Option<NonZeroU64>,
    generation: NonZeroU64,
}

impl AudioOwnerInstallationCommit<'_> {
    /// Publish immediately beside the successfully exchanged actual renderer slot.
    /// The host has already reserved every fallible operation; this performs no
    /// allocation, callback, native call or second request dispatch.
    pub fn commit(mut self) -> Option<AudioOwnerInstallationClaim> {
        self.state.current = self.registration.map(|registration| (registration, self.generation));
        self.registration.map(|registration| AudioOwnerInstallationClaim {
            state: Arc::clone(&self.source),
            registration,
            generation: self.generation,
        })
    }
}

/// Opaque current installation claim, never reconstructible from IDs or wire data.
///
/// A copy can become stale. Every editor/author admission must validate it using
/// the actual Runtime's bound verifier and current Hosted native binding record.
/// It proves the renderer slot only, never a native object, author transaction,
/// root DSP window, candidate resource grant or private DSP state checkpoint.
#[derive(Debug, Clone)]
pub struct AudioOwnerInstallationClaim {
    state: Arc<Mutex<InstallationState>>,
    registration: NonZeroU64,
    generation: NonZeroU64,
}

impl AudioOwnerInstallationClaim {
    /// Exact monotonic generation of this actual installation source.
    pub const fn generation(&self) -> NonZeroU64 {
        self.generation
    }
}

#[cfg(test)]
mod installation_contracts {
    use super::*;

    #[test]
    fn registered_renderer_has_no_permission_before_actual_slot_swap() {
        let source = AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().expect("actual registration");
        let (other, other_verifier) = source.register().expect("another real registration");
        let old = source
            .reserve_install(
                &other,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("reserve old slot")
            .commit()
            .expect("native installation claim");
        assert_eq!(
            verifier.validate(&old),
            Err(AudioOwnerInstallationError::WrongRegistration)
        );
        assert!(other_verifier.validate(&old).is_ok());
        let claim = source
            .reserve_install(
                &registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("fallible preflight")
            .commit()
            .expect("native installation claim");
        assert!(verifier.validate(&claim).is_ok());
        assert_eq!(
            other_verifier.validate(&old),
            Err(AudioOwnerInstallationError::Superseded)
        );
    }

    #[test]
    fn abandoned_slot_exchange_keeps_old_claim_current_and_does_not_install_new_owner() {
        let source = AudioOwnerInstallationSource::new();
        let (old_registration, old_verifier) = source.register().expect("old registration");
        let old = source
            .reserve_install(
                &old_registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("actual old swap")
            .commit()
            .expect("native installation claim");
        let (new_registration, new_verifier) = source.register().expect("new registration");
        drop(
            source
                .reserve_install(
                    &new_registration,
                    source.current_claim().expect("current slot").as_ref(),
                )
                .and_then(|reservation| source.begin_commit(reservation))
                .expect("prepared but failed swap"),
        );
        assert!(
            old_verifier.validate(&old).is_ok(),
            "fallible swap cannot revoke the still-current old owner"
        );
        assert_eq!(
            new_verifier.validate(&old),
            Err(AudioOwnerInstallationError::WrongRegistration)
        );
    }

    #[test]
    fn wrong_source_cannot_grant_or_replace_already_registered_runtime() {
        let first = AudioOwnerInstallationSource::new();
        let second = AudioOwnerInstallationSource::new();
        let (registration, verifier) = first.register().expect("bound actual Runtime");
        let current = first
            .reserve_install(
                &registration,
                first.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| first.begin_commit(reservation))
            .expect("current slot")
            .commit()
            .expect("native installation claim");
        assert!(matches!(
            second
                .reserve_install(
                    &registration,
                    second.current_claim().expect("current slot").as_ref()
                )
                .and_then(|reservation| second.begin_commit(reservation)),
            Err(AudioOwnerInstallationError::WrongRegistration)
        ));
        let (other, other_verifier) = second.register().expect("unrelated pair");
        let unrelated = second
            .reserve_install(
                &other,
                second.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| second.begin_commit(reservation))
            .expect("its own slot only")
            .commit()
            .expect("native installation claim");
        assert!(other_verifier.validate(&unrelated).is_ok());
        assert_eq!(
            verifier.validate(&unrelated),
            Err(AudioOwnerInstallationError::WrongRegistration)
        );
        assert!(verifier.validate(&current).is_ok());
    }

    #[test]
    fn same_runtime_reinstallation_revokes_old_generation_and_source_drop_revokes_last_claim() {
        let source = AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().expect("same Runtime registration");
        let before = source
            .reserve_install(
                &registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("first actual swap")
            .commit()
            .expect("native installation claim");
        let after = source
            .reserve_install(
                &registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("fresh real generation")
            .commit()
            .expect("native installation claim");
        assert!(after.generation() > before.generation());
        assert_eq!(
            verifier.validate(&before),
            Err(AudioOwnerInstallationError::Superseded)
        );
        assert!(verifier.validate(&after).is_ok());
        source.revoke().expect("actual renderer cleared");
        assert_eq!(
            verifier.validate(&after),
            Err(AudioOwnerInstallationError::NotInstalled)
        );
        let restored = source
            .reserve_install(
                &registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("later explicit installation")
            .commit()
            .expect("native installation claim");
        assert!(verifier.validate(&restored).is_ok());
        drop(source);
        assert_eq!(
            verifier.validate(&restored),
            Err(AudioOwnerInstallationError::NotInstalled)
        );
    }

    #[test]
    fn exhausted_serial_preflight_preserves_old_current_installation() {
        let source = AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().expect("registration");
        let current = source
            .reserve_install(
                &registration,
                source.current_claim().expect("current slot").as_ref(),
            )
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("old actual swap")
            .commit()
            .expect("native installation claim");
        source.state.lock().expect("known state").next_serial = u64::MAX;
        assert!(matches!(
            source
                .reserve_install(
                    &registration,
                    source.current_claim().expect("current slot").as_ref()
                )
                .and_then(|reservation| source.begin_commit(reservation)),
            Err(AudioOwnerInstallationError::SerialExhausted)
        ));
        assert!(
            verifier.validate(&current).is_ok(),
            "preflight exhaustion precedes all real slot mutation"
        );
    }

    #[test]
    fn stale_worker_installation_cannot_replace_a_newer_actual_slot() {
        let source = AudioOwnerInstallationSource::new();
        let (old, old_verifier) = source.register().expect("old Runtime");
        let initial = source
            .reserve_install(&old, None)
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("first actual slot")
            .commit()
            .expect("native installation claim");
        let (candidate, _) = source.register().expect("worker candidate only");
        let (new_ui, new_verifier) = source.register().expect("new UI Runtime");
        let newer = source
            .reserve_install(&new_ui, Some(&initial))
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("actual UI swap")
            .commit()
            .expect("native installation claim");
        assert!(matches!(
            source
                .reserve_install(&candidate, Some(&initial))
                .and_then(|reservation| source.begin_commit(reservation)),
            Err(AudioOwnerInstallationError::Superseded)
        ));
        assert!(matches!(
            source
                .reserve_install(&candidate, None)
                .and_then(|reservation| source.begin_commit(reservation)),
            Err(AudioOwnerInstallationError::Superseded)
        ));
        assert!(new_verifier.validate(&newer).is_ok());
        assert!(matches!(
            old_verifier.current_claim(),
            Err(AudioOwnerInstallationError::Superseded)
        ));
    }
    #[test]
    fn delayed_native_installation_reservation_cannot_overwrite_an_intervening_real_slot_swap() {
        let source = AudioOwnerInstallationSource::new();
        let (old, _) = source.register().expect("actual old registration");
        let before = source
            .reserve_install(&old, None)
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("first actual slot")
            .commit()
            .expect("old native claim");
        let (candidate, _) = source.register().expect("not yet installed candidate");
        let delayed = source
            .reserve_install(&candidate, Some(&before))
            .expect("worker foreign preflight may now run");
        let (next, verifier) = source.register().expect("next actual renderer");
        let after = source
            .reserve_install(&next, Some(&before))
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("intervening real UI swap")
            .commit()
            .expect("new claim");
        assert!(matches!(
            source.begin_commit(delayed),
            Err(AudioOwnerInstallationError::Superseded)
        ));
        assert!(verifier.validate(&after).is_ok());
    }

    #[test]
    fn failed_clear_preflight_keeps_old_permission_and_actual_clear_publication_revokes_it() {
        let source = AudioOwnerInstallationSource::new();
        let (old, verifier) = source.register().expect("old actual renderer");
        let old = source
            .reserve_install(&old, None)
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("actual old slot")
            .commit()
            .expect("claim");
        drop(source.reserve_clear(Some(&old)).expect("fallible output preflight follows"));
        assert!(
            verifier.validate(&old).is_ok(),
            "a failed output clear preflight keeps the actual old slot current"
        );
        let clear = source
            .reserve_clear(Some(&old))
            .and_then(|reservation| source.begin_commit(reservation))
            .expect("successful output preflight then exact clear swap")
            .commit();
        assert!(
            clear.is_none(),
            "unsupported/empty actual slot cannot mint native permission"
        );
        assert_eq!(
            verifier.validate(&old),
            Err(AudioOwnerInstallationError::NotInstalled)
        );
    }
}

/// One non-Clone reserved publication passed by the sole private Media source.
///
/// It cannot register, revoke, mint another permission or deserialize a claim.
/// A successful local acquisition consumes it, including an abandoned guard.
pub struct AudioOwnerRuntimeInstallationAuthorization {
    source: Arc<Mutex<InstallationState>>,
    registration: NonZeroU64,
    generation: NonZeroU64,
    expected: Option<(NonZeroU64, NonZeroU64)>,
    available: bool,
}

impl AudioOwnerRuntimeInstallationAuthorization {
    /// Acquire the final current-slot CAS for the exact reserved destination.
    ///
    /// All native calls, allocations and fallible graph/author/resource
    /// admission precede this point. The actual Runtime is already exclusively
    /// held; only its infallible ownership exchange and publication may follow.
    pub fn begin_commit(
        &mut self,
        destination: &AudioOwnerInstallationVerifier,
    ) -> Result<AudioOwnerRuntimeInstallationCommit<'_>, AudioOwnerInstallationError> {
        if !self.available {
            return Err(AudioOwnerInstallationError::ConsumedAuthorization);
        }
        if self.registration != destination.serial || !Arc::ptr_eq(&self.source, &destination.state)
        {
            return Err(AudioOwnerInstallationError::WrongRegistration);
        }
        let state = self.source.lock().map_err(|_| AudioOwnerInstallationError::Poisoned)?;
        if state.current != self.expected {
            return Err(AudioOwnerInstallationError::Superseded);
        }
        self.available = false;
        Ok(AudioOwnerRuntimeInstallationCommit {
            state,
            source: Arc::clone(&self.source),
            registration: self.registration,
            generation: self.generation,
        })
    }
}

/// A non-Send registered Runtime swap guard, never a clear-slot operation.
///
/// Its exact destination was admitted before acquisition. Dropping without
/// commit leaves the old slot unchanged and consumes the one-shot permission.
#[must_use]
pub struct AudioOwnerRuntimeInstallationCommit<'a> {
    state: MutexGuard<'a, InstallationState>,
    source: Arc<Mutex<InstallationState>>,
    registration: NonZeroU64,
    generation: NonZeroU64,
}

impl AudioOwnerRuntimeInstallationCommit<'_> {
    /// Exact reserved installation generation used only by the bounded same-source
    /// Runtime swap. It is not an installed claim until this guard commits.
    pub const fn generation(&self) -> NonZeroU64 {
        self.generation
    }

    /// Publish beside the actual infallible registered Runtime exchange.
    /// This performs no allocation, callback, foreign drop or native work.
    pub fn commit(mut self) -> AudioOwnerInstallationClaim {
        self.state.current = Some((self.registration, self.generation));
        AudioOwnerInstallationClaim {
            state: Arc::clone(&self.source),
            registration: self.registration,
            generation: self.generation,
        }
    }
}

#[cfg(test)]
mod runtime_authorization_contracts {
    use super::*;

    fn installed(
        source: &AudioOwnerInstallationSource,
    ) -> (AudioOwnerInstallationClaim, AudioOwnerInstallationVerifier) {
        let (registration, verifier) = source.register().expect("actual source registration");
        let claim = source
            .begin_commit(source.reserve_install(&registration, None).expect("actual reservation"))
            .expect("actual slot publication")
            .commit()
            .expect("registered claim");
        (claim, verifier)
    }

    #[test]
    fn runtime_authorization_exact_destination_and_local_publication_are_both_required() {
        let source = AudioOwnerInstallationSource::new();
        let (old, old_verifier) = installed(&source);
        let (registration, destination) =
            source.register().expect("actual replacement registration");
        let mut authorization = source
            .authorize_runtime_installation(
                source
                    .reserve_install(&registration, Some(&old))
                    .expect("preflight reservation"),
            )
            .expect("same private Source authorization");
        assert!(old_verifier.validate(&old).is_ok());
        assert!(matches!(
            destination.current_claim(),
            Err(AudioOwnerInstallationError::Superseded)
        ));
        let claim = authorization
            .begin_commit(&destination)
            .expect("exact actual destination CAS")
            .commit();
        assert!(destination.validate(&claim).is_ok());
        assert!(matches!(
            old_verifier.validate(&old),
            Err(AudioOwnerInstallationError::Superseded)
        ));
        assert!(matches!(
            authorization.begin_commit(&destination),
            Err(AudioOwnerInstallationError::ConsumedAuthorization)
        ));
    }

    #[test]
    fn runtime_authorization_wrong_destination_cannot_publish_or_consume_the_actual_reservation() {
        let source = AudioOwnerInstallationSource::new();
        let (old, old_verifier) = installed(&source);
        let (registration, destination) = source.register().expect("actual replacement");
        let (_, unrelated) =
            source.register().expect("same Source, different Runtime registration");
        let other = AudioOwnerInstallationSource::new();
        let (_, foreign) = other.register().expect("foreign Source");
        let mut authorization = source
            .authorize_runtime_installation(
                source
                    .reserve_install(&registration, Some(&old))
                    .expect("reserved current slot"),
            )
            .expect("actual Source authorization");
        for invalid in [&unrelated, &foreign] {
            assert!(matches!(
                authorization.begin_commit(invalid),
                Err(AudioOwnerInstallationError::WrongRegistration)
            ));
            assert!(old_verifier.validate(&old).is_ok());
        }
        let claim = authorization
            .begin_commit(&destination)
            .expect("valid destination still admissible")
            .commit();
        assert!(destination.validate(&claim).is_ok());
    }

    #[test]
    fn runtime_authorization_abandoned_guard_preserves_old_owner_and_is_one_shot() {
        let source = AudioOwnerInstallationSource::new();
        let (old, verifier) = installed(&source);
        let (registration, destination) = source.register().expect("actual replacement");
        let mut authorization = source
            .authorize_runtime_installation(
                source.reserve_install(&registration, Some(&old)).expect("reservation"),
            )
            .expect("authorization");
        drop(
            authorization
                .begin_commit(&destination)
                .expect("local guard without actual exchange"),
        );
        assert!(verifier.validate(&old).is_ok());
        assert!(matches!(
            authorization.begin_commit(&destination),
            Err(AudioOwnerInstallationError::ConsumedAuthorization)
        ));
    }

    #[test]
    fn runtime_authorization_superseded_or_cleared_actual_slot_refuses_late_publication() {
        for clear in [false, true] {
            let source = AudioOwnerInstallationSource::new();
            let (old, _) = installed(&source);
            let (registration, destination) = source.register().expect("actual replacement");
            let mut authorization = source
                .authorize_runtime_installation(
                    source
                        .reserve_install(&registration, Some(&old))
                        .expect("before unrelated slot mutation"),
                )
                .expect("authorization");
            if clear {
                source
                    .begin_commit(
                        source.reserve_clear(Some(&old)).expect("actual clear reservation"),
                    )
                    .expect("actual clear")
                    .commit();
            } else {
                let (other, _) = source.register().expect("another actual Runtime");
                source
                    .begin_commit(
                        source
                            .reserve_install(&other, Some(&old))
                            .expect("actual slot replacement"),
                    )
                    .expect("actual replacement")
                    .commit();
            }
            assert!(matches!(
                authorization.begin_commit(&destination),
                Err(AudioOwnerInstallationError::Superseded)
            ));
        }
    }

    #[test]
    fn runtime_authorization_foreign_source_and_clear_reservations_never_grant_runtime_installation(
    ) {
        let source = AudioOwnerInstallationSource::new();
        let other = AudioOwnerInstallationSource::new();
        let (registration, _) = source.register().expect("actual registration");
        assert!(matches!(
            other.authorize_runtime_installation(
                source.reserve_install(&registration, None).expect("same-source reservation")
            ),
            Err(AudioOwnerInstallationError::WrongRegistration)
        ));
        assert!(matches!(
            source.authorize_runtime_installation(
                source.reserve_clear(None).expect("clear reservation")
            ),
            Err(AudioOwnerInstallationError::WrongRegistration)
        ));
        assert!(source.current_claim().expect("actual unchanged slot").is_none());
    }
}
