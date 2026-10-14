//! Scalar policy, actual receipt Drop and deterministic RMW interleavings.
//! Synthetic storage is a Drop-order control, not a native-owner qualification.

use super::*;
use std::sync::mpsc;

fn active_fee(scope: &Arc<AtomicUsize>, old: usize, new: usize) -> LogicalPlanFee {
    // Every caller uses a tiny bounded scope and the sole enrollment thread.
    // This is the same private receipt, not a duplicated accounting algorithm.
    let mut fee = LogicalPlanFee::inactive(Arc::clone(scope), old, new);
    fee.enroll().expect("one private bounded enrollment");
    fee
}

#[test]
fn logical_fee_inactive_zero_and_duplicate_enrollment_credit_exactly_once() {
    let scope = Arc::new(AtomicUsize::new(0));
    drop(LogicalPlanFee::inactive(Arc::clone(&scope), 19, 23));
    assert_eq!(scope.load(Ordering::SeqCst), 0);
    let mut zero = active_fee(&scope, 0, 0);
    let transition = zero.checked_retirement(&scope, 41, 41, 41).unwrap();
    zero.retire(transition);
    drop(zero);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
    let mut fee = active_fee(&scope, 19, 23);
    assert_eq!(scope.load(Ordering::SeqCst), 23);
    assert!(matches!(
        fee.enroll(),
        Err(AudioRuntimeBuildError::StaleResourceAdmission)
    ));
    assert_eq!(scope.load(Ordering::SeqCst), 23);
    drop(fee);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
}

#[test]
fn logical_fee_rejects_foreign_scope_stale_phase_and_invalid_projection_without_mutation() {
    let scope = Arc::new(AtomicUsize::new(0));
    let foreign = Arc::new(AtomicUsize::new(0));
    let inactive = LogicalPlanFee::inactive(Arc::clone(&scope), 17, 29);
    assert!(matches!(
        inactive.checked_retirement(&scope, 40, 52, 100),
        Err(AudioRuntimeBuildError::StaleResourceAdmission)
    ));
    drop(inactive);
    let mut fee = active_fee(&scope, 17, 29);
    assert!(matches!(
        fee.checked_retirement(&foreign, 40, 52, 100),
        Err(AudioRuntimeBuildError::StaleResourceAdmission)
    ));
    assert!(matches!(
        fee.checked_retirement(&scope, 16, 28, 100),
        Err(AudioRuntimeBuildError::InvalidResourceReplacement)
    ));
    assert!(matches!(
        fee.checked_retirement(&scope, 40, 51, 100),
        Err(AudioRuntimeBuildError::InvalidResourceReplacement)
    ));
    assert!(matches!(
        fee.checked_retirement(&scope, 40, 52, 68),
        Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            required: 69,
            granted: 68,
        })
    ));
    assert_eq!(scope.load(Ordering::SeqCst), 29);
    assert_eq!(foreign.load(Ordering::SeqCst), 0);
    let transition = fee.checked_retirement(&scope, 40, 52, 69).unwrap();
    fee.retire(transition);
    assert_eq!(scope.load(Ordering::SeqCst), 17);
    assert!(matches!(
        fee.checked_retirement(&scope, 52, 64, 100),
        Err(AudioRuntimeBuildError::StaleResourceAdmission)
    ));
    drop(fee);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
}

#[test]
fn logical_fee_checked_overflow_rejects_before_any_role_rmw() {
    // Deliberately malformed scalar input exercises checked arithmetic only.
    // A valid Root can never enroll this impossible H; no wrapping enrollment
    // or refund is issued, and this inactive receipt has no contribution.
    let scope = Arc::new(AtomicUsize::new(usize::MAX));
    let fee = active_fee(&scope, 0, 0);
    assert!(matches!(
        fee.checked_retirement(&scope, 1, 1, usize::MAX),
        Err(AudioRuntimeBuildError::ResourceFootprint(
            crate::AudioResourceFootprintError::ExtentOverflow
        ))
    ));
    assert_eq!(scope.load(Ordering::SeqCst), usize::MAX);
    drop(fee); // Its prepared amount is zero; even this drop cannot alter H.
    assert_eq!(scope.load(Ordering::SeqCst), usize::MAX);
}

fn role_transition_after_peer_drop(old: usize, new: usize, current: usize, next: usize) {
    let scope = Arc::new(AtomicUsize::new(0));
    let mut fee = active_fee(&scope, old, new);
    let peer = active_fee(&scope, 7, 11);
    let observed = scope.load(Ordering::SeqCst);
    assert_eq!(observed, new + 11);
    let combined = sum(current, observed).unwrap();
    let transition = fee.checked_retirement(&scope, current, next, combined).unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        release_rx.recv().unwrap();
        drop(peer);
        dropped_tx.send(()).unwrap();
    });
    // The other real receipt credits after our check, before the final RMW.
    release_tx.send(()).unwrap();
    dropped_rx.recv().unwrap();
    assert_eq!(scope.load(Ordering::SeqCst), new);
    fee.retire(transition);
    assert_eq!(
        scope.load(Ordering::SeqCst),
        old,
        "RMW preserves the intervening peer credit; a stale store cannot"
    );
    assert_eq!(
        sum(next, scope.load(Ordering::SeqCst)).unwrap(),
        combined - 11
    );
    worker.join().unwrap();
    drop(fee);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
}

#[test]
fn logical_fee_growth_rmw_preserves_peer_drop_between_precheck_and_swap() {
    role_transition_after_peer_drop(17, 29, 40, 52);
}

#[test]
fn logical_fee_shrink_rmw_preserves_peer_drop_between_precheck_and_swap() {
    role_transition_after_peer_drop(29, 17, 40, 28);
}

#[test]
fn logical_fee_enrollment_rmw_preserves_credit_after_bounded_root_observation() {
    let scope = Arc::new(AtomicUsize::new(0));
    let peer = active_fee(&scope, 19, 23);
    let mut incoming = LogicalPlanFee::inactive(Arc::clone(&scope), 11, 17);
    let current = 100;
    let observed = sum(sum(current, scope.load(Ordering::SeqCst)).unwrap(), 17).unwrap();
    assert_eq!(observed, 140); // A valid original-grant observation.
    let (release_tx, release_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        release_rx.recv().unwrap();
        drop(peer);
        dropped_tx.send(()).unwrap();
    });
    release_tx.send(()).unwrap();
    dropped_rx.recv().unwrap();
    assert_eq!(scope.load(Ordering::SeqCst), 0);
    incoming.enroll().unwrap();
    assert_eq!(
        scope.load(Ordering::SeqCst),
        17,
        "enrollment must not recreate a fee credited after its observation"
    );
    assert!(sum(current, scope.load(Ordering::SeqCst)).unwrap() <= observed);
    worker.join().unwrap();
    drop(incoming);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
}

struct BlockingStorageDrop {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Drop for BlockingStorageDrop {
    fn drop(&mut self) {
        self.entered.send(()).unwrap();
        self.release.recv().unwrap();
    }
}

struct FinalStorageDrop {
    complete: Arc<AtomicUsize>,
    scope: Arc<AtomicUsize>,
    expected: usize,
}

impl Drop for FinalStorageDrop {
    fn drop(&mut self) {
        assert_eq!(
            self.scope.load(Ordering::SeqCst),
            self.expected,
            "the receipt must still charge while the last owner destructor runs"
        );
        self.complete.fetch_add(1, Ordering::SeqCst);
    }
}

struct StorageThenLastFee {
    _first: BlockingStorageDrop,
    _last_owner: FinalStorageDrop,
    fee: LogicalPlanFee,
}

#[test]
fn logical_fee_last_field_credits_only_after_all_real_destructors_complete() {
    let scope = Arc::new(AtomicUsize::new(0));
    let complete = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut owner = StorageThenLastFee {
        _first: BlockingStorageDrop { entered: entered_tx, release: release_rx },
        _last_owner: FinalStorageDrop {
            complete: Arc::clone(&complete),
            scope: Arc::clone(&scope),
            expected: 23,
        },
        fee: LogicalPlanFee::inactive(Arc::clone(&scope), 19, 23),
    };
    owner.fee.enroll().unwrap(); // Actual owner assembly precedes enrollment.
    let worker = std::thread::spawn(move || drop(owner));
    entered_rx.recv().unwrap();
    assert_eq!(complete.load(Ordering::SeqCst), 0);
    assert_eq!(
        scope.load(Ordering::SeqCst),
        23,
        "a weak strong-count alone cannot prove a blocked destructor completed"
    );
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(complete.load(Ordering::SeqCst), 1);
    assert_eq!(scope.load(Ordering::SeqCst), 0);
}

#[test]
fn logical_fee_receipt_outlives_root_scope_reference_without_cross_root_credit() {
    // This checks Arc-only accounting lifetime, not full Runtime/native Drop.
    let root_scope = Arc::new(AtomicUsize::new(0));
    let witness = Arc::clone(&root_scope);
    let other_scope = Arc::new(AtomicUsize::new(0));
    let fee = active_fee(&root_scope, 17, 29);
    let other_fee = active_fee(&other_scope, 5, 7);
    drop(root_scope);
    assert_eq!(witness.load(Ordering::SeqCst), 29);
    assert_eq!(other_scope.load(Ordering::SeqCst), 7);
    drop(fee);
    assert_eq!(witness.load(Ordering::SeqCst), 0);
    assert_eq!(other_scope.load(Ordering::SeqCst), 7);
    drop(other_fee);
    assert_eq!(other_scope.load(Ordering::SeqCst), 0);
}
