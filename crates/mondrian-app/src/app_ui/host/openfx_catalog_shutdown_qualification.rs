//! Windows qualification only: real App installation and actual native callback.

use super::*;
use std::ffi::c_void;
use std::io;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

const SYNCHRONIZE: u32 = 0x0010_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_TERMINATE: u32 = 1;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "OpenProcess"]
    fn open_process(access: u32, inherit: i32, process: u32) -> *mut c_void;
    #[link_name = "WaitForSingleObject"]
    fn wait_for_single_object(handle: *mut c_void, milliseconds: u32) -> u32;
    #[link_name = "QueryFullProcessImageNameW"]
    fn query_full_process_image_name(
        handle: *mut c_void,
        flags: u32,
        buffer: *mut u16,
        size: *mut u32,
    ) -> i32;
    #[link_name = "TerminateProcess"]
    fn terminate_process(handle: *mut c_void, exit_code: u32) -> i32;
}

fn signaled(handle: &OwnedHandle) -> io::Result<bool> {
    // SAFETY: owned kernel object remains alive for this zero-time observation.
    match unsafe { wait_for_single_object(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

struct NativeProcess {
    handle: OwnedHandle,
    settlement_observed: bool,
}

impl NativeProcess {
    fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: OpenProcess returns an independent reference; no borrowed
        // plugin pointer or Rust Child handle crosses this observation seam.
        let raw = unsafe {
            open_process(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful OpenProcess transfers exactly one owned reference.
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(raw) },
            settlement_observed: false,
        })
    }

    fn executable(&self) -> io::Result<PathBuf> {
        let mut buffer = vec![0_u16; 32768];
        let mut length = buffer.len() as u32;
        // SAFETY: writable buffer and its declared capacity agree; native
        // process handle is retained independently of catalog destruction.
        if unsafe {
            query_full_process_image_name(
                self.handle.as_raw_handle(),
                0,
                buffer.as_mut_ptr(),
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let content = buffer
            .get(..length as usize)
            .ok_or_else(|| io::Error::other("native process image length exceeds buffer"))?;
        Ok(PathBuf::from(std::ffi::OsString::from_wide(content)))
    }

    fn require_settled(&mut self) {
        assert!(
            signaled(&self.handle).expect("observe child exit"),
            "actual native child remains alive after Host Drop"
        );
        self.settlement_observed = true;
    }
}

impl Drop for NativeProcess {
    fn drop(&mut self) {
        if !self.settlement_observed {
            // Failure-only best effort. No wait, second catalog Drop, or thread
            // termination is performed here. Forced cleanup never makes PASS.
            // SAFETY: this retained handle identifies exactly the observed
            // test-owned native process, independently of PID reuse.
            let result = unsafe { terminate_process(self.handle.as_raw_handle(), 125) };
            if result == 0 && !matches!(signaled(&self.handle), Ok(true)) {
                eprintln!(
                    "OpenFX qualification cleanup failed: {}",
                    io::Error::last_os_error()
                );
            }
        }
    }
}

fn read_native_pid(marker: &Path, deadline: Instant) -> u32 {
    loop {
        match std::fs::read_to_string(marker) {
            Ok(value) if value.ends_with('\n') => {
                let rows: Vec<_> = value.lines().collect();
                assert_eq!(
                    rows.len(),
                    1,
                    "only the named callback may issue this receipt"
                );
                let pid = rows[0].parse::<u32>().expect("native PID receipt");
                assert_ne!(pid, 0);
                assert_ne!(
                    pid,
                    std::process::id(),
                    "native callback must be in the product child"
                );
                return pid;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("read native receipt: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "product helper did not enter the required native callback"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn qualify_active_catalog_drop(mode: &str) {
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("fixture mode"),
        mode
    );
    let product = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_WORKER_PATH").expect("matching product helper"),
    );
    let probe = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("rebuilt SessionProbe"),
    );
    let marker = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_CATALOG_NATIVE_STARTED").expect("fresh marker path"),
    );
    assert!(product.is_absolute() && product.is_file());
    assert!(probe.is_absolute() && probe.is_file());
    assert!(marker.is_absolute() && !marker.exists());
    let directory = tempfile::tempdir().expect("isolated preferences directory");
    let preferences_path = directory.path().join("preferences.json");
    persist_app_ui_preferences_to(&preferences_path, &AppUiPreferences::default())
        .expect("initial preferences");
    // Failure unwinding must never start a second potentially blocked Host
    // destructor. The external owning runner must enforce this process's
    // deadline/descendant cleanup. No normal path leaks the Host.
    let mut host = ManuallyDrop::new(Box::new(AppUiHost::new_with_preferences_path(
        AppState::new(),
        AppUiPreferences::default(),
        preferences_path.clone(),
    )));
    let preferences_before = std::fs::read(&preferences_path).expect("initial persisted bytes");
    let generation = host.app_state().project_author_generation();
    assert!(
        mondrian_effects::effect_definition(&mondrian_effects::EffectType::GaussianBlur).is_some()
    );
    let registry_before = mondrian_effects::effect_registry_revision();
    host.dispatch_editor_action(
        ProductAction::VisualEffect(
            crate::app::product_action::VisualEffectProductAction::InstallOpenFxBundle(
                crate::app::product_action::VisualEffectInstallOpenFxBundlePayload {
                    path: probe,
                    context: crate::openfx_host::OpenFxContext::Filter,
                },
            ),
        )
        .into_external_action(),
    )
    .expect("queue actual product install");
    let worker = host
        .app_state()
        .openfx_catalog_worker_handle_for_qualification()
        .expect("duplicate actual catalog worker thread handle");
    let pid = read_native_pid(&marker, Instant::now() + Duration::from_secs(15));
    let mut child = NativeProcess::open(pid).expect("retain actual native child handle");
    assert_eq!(
        std::fs::canonicalize(child.executable().expect("child executable"))
            .expect("actual helper path"),
        std::fs::canonicalize(&product).expect("bound product path")
    );
    assert!(!signaled(&child.handle).expect("child is running"));
    assert!(!signaled(&worker).expect("worker is running"));
    assert_eq!(host.app_state().project_author_generation(), generation);
    assert_eq!(
        mondrian_effects::effect_registry_revision(),
        registry_before
    );
    assert!(host.preferences.installed_openfx_bundles.is_empty());
    let started = Instant::now();
    // SAFETY: the sole boxed Host is dropped exactly once. ManuallyDrop prevents
    // failure unwinding from calling its synchronous shutdown a second time.
    unsafe { ManuallyDrop::drop(&mut host) };
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "active catalog cancellation must settle before the 10/30-second native timeout"
    );
    child.require_settled();
    assert!(
        signaled(&worker).expect("observe joined catalog worker"),
        "actual catalog worker remains alive"
    );
    assert_eq!(
        mondrian_effects::effect_registry_revision(),
        registry_before
    );
    assert_eq!(
        std::fs::read(&preferences_path).expect("final persisted bytes"),
        preferences_before
    );
    assert!(load_app_ui_preferences_from(&preferences_path)
        .installed_openfx_bundles
        .is_empty());
    println!("OFX_CATALOG_DROP phase={mode} child_pid={pid} process_settled=true worker_settled=true registry_unchanged=true preferences_unchanged=true elapsed_ms={}", started.elapsed().as_millis());
}

#[test]
#[ignore = "requires matching product, rebuilt SessionProbe catalog_discovery_block, and fresh native marker; isolated exact Windows run"]
fn active_discovery_drop_settles_catalog_without_publication() {
    qualify_active_catalog_drop("catalog_discovery_block");
}

#[test]
#[ignore = "requires matching product, rebuilt SessionProbe catalog_describe_block, and fresh native marker; isolated exact Windows run"]
fn active_describe_drop_settles_catalog_without_publication() {
    qualify_active_catalog_drop("catalog_describe_block");
}
