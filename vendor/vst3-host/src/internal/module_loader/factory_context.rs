//! Stable, module-scoped host services for a native factory identity.
//!
//! Native loaders may return the same factory for several live instances or
//! discovery passes. Replacing its context per instance redirects module-global
//! timers and can leave a borrowed context dangling. Only this owner binds the
//! factory; instance parameter, progress and Data Exchange state remain separate.

use super::VstModule;
use crate::error::{Error, Result};
use crate::host_storage::{HostStorageError, HostStorageRef, HostStorageVisitor};
use crate::internal::{com_implementations, host_storage::RetainedComStorage};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, ThreadId};
use vst3::{Class, ComPtr, ComWrapper, Steinberg::Vst::*, Steinberg::*};

#[cfg(target_os = "linux")]
use crate::internal::run_loop::RunLoopRegistry;

/// Factory services deliberately do not expose instance Data Exchange/progress.
struct FactoryHostContext {
    storage: RetainedComStorage,
    #[cfg(target_os = "linux")]
    run_loop: Arc<Mutex<RunLoopRegistry>>,
}

impl FactoryHostContext {
    fn new() -> Self {
        Self {
            storage: RetainedComStorage::new(),
            #[cfg(target_os = "linux")]
            run_loop: Arc::new(Mutex::new(RunLoopRegistry::new())),
        }
    }

    fn visit_owned_storage(
        &self,
        visitor: &mut dyn HostStorageVisitor,
    ) -> std::result::Result<(), HostStorageError> {
        self.storage.visit_owned_storage(visitor)?;
        #[cfg(target_os = "linux")]
        crate::internal::run_loop::visit_owned_storage(&self.run_loop, visitor)?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Class for FactoryHostContext {
    type Interfaces = (IHostApplication, Linux::IRunLoop);
}

#[cfg(not(target_os = "linux"))]
impl Class for FactoryHostContext {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for FactoryHostContext {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        com_implementations::host_application_name(name)
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> tresult {
        com_implementations::host_application_create_instance(&self.storage, cid, iid, obj)
    }
}

#[cfg(target_os = "linux")]
impl Linux::IRunLoopTrait for FactoryHostContext {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut Linux::IEventHandler,
        fd: Linux::FileDescriptor,
    ) -> tresult {
        RunLoopRegistry::register_event_handler(&self.run_loop, handler, fd)
    }
    unsafe fn unregisterEventHandler(&self, handler: *mut Linux::IEventHandler) -> tresult {
        RunLoopRegistry::unregister_event_handler(&self.run_loop, handler)
    }
    unsafe fn registerTimer(
        &self,
        handler: *mut Linux::ITimerHandler,
        milliseconds: Linux::TimerInterval,
    ) -> tresult {
        RunLoopRegistry::register_timer(&self.run_loop, handler, milliseconds)
    }
    unsafe fn unregisterTimer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        RunLoopRegistry::unregister_timer(&self.run_loop, handler)
    }
}

struct FactoryContextState {
    identity: usize,
    owner_thread: ThreadId,
    modules: AtomicUsize,
    bound: AtomicBool,
    context: ComWrapper<FactoryHostContext>,
}

// This is an ownership index, not a module/DSP cache. Weak entries cannot retain
// plugins or their contexts, and are removed after the final module unloads.
type ContextIndex = Vec<(usize, Weak<FactoryContextState>)>;
fn contexts() -> &'static Mutex<ContextIndex> {
    static CONTEXTS: OnceLock<Mutex<ContextIndex>> = OnceLock::new();
    CONTEXTS.get_or_init(|| Mutex::new(Vec::new()))
}

impl Drop for FactoryContextState {
    fn drop(&mut self) {
        let mut index = contexts().lock().unwrap_or_else(|poison| poison.into_inner());
        index.retain(|(identity, entry)| {
            *identity != self.identity || entry.as_ptr() != std::ptr::from_ref(self)
        });
        // Release peak ownership-index capacity when the last factory retires.
        if index.is_empty() {
            *index = Vec::new();
        }
    }
}

pub(super) struct FactoryModule {
    // Fields drop in declaration order: native factory, then module code, then
    // the host context. The last module retires callbacks in Drop, BEFORE any
    // plugin code can unmap; borrowed host contexts remain valid through unload.
    factory: ComPtr<IPluginFactory>,
    native: Box<dyn VstModule>,
    services: Arc<FactoryContextState>,
}

impl FactoryModule {
    pub(super) fn attach(native: Box<dyn VstModule>) -> Result<Self> {
        // SAFETY: get_factory supplies an owning native COM reference. Keep it
        // until module teardown, including on every constructor error path.
        let factory = unsafe { ComPtr::from_raw(native.get_factory()?) }
            .ok_or_else(|| Error::PluginLoadFailed("GetPluginFactory returned null".to_string()))?;
        let identity = factory.cast::<FUnknown>().ok_or_else(|| {
            Error::InterfaceError("factory does not expose its FUnknown identity".to_string())
        })?;
        let key = identity.as_ptr() as usize;
        drop(identity); // No native QueryInterface/AddRef/Release under the index lock.
        let (services, first) = {
            let mut index = contexts().lock().map_err(|_| {
                Error::PluginLoadFailed("factory context ownership is poisoned".to_string())
            })?;
            index.retain(|(_, entry)| entry.strong_count() != 0);
            if let Some(state) = index
                .iter()
                .find(|(identity, _)| *identity == key)
                .and_then(|(_, value)| value.upgrade())
            {
                if state.owner_thread != thread::current().id() {
                    // Drop the guard before dropping a possible last state reference.
                    drop(index);
                    return Err(Error::PluginLoadFailed(
                        "live native factory belongs to a different control thread".to_string(),
                    ));
                }
                if !state.bound.load(Ordering::Acquire)
                    || state.modules.load(Ordering::Acquire) == 0
                {
                    drop(index);
                    return Err(Error::PluginLoadFailed(
                        "factory context is initializing or retiring".to_string(),
                    ));
                }
                state
                    .modules
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        count.checked_add(1)
                    })
                    .map_err(|_| {
                        Error::PluginLoadFailed("too many live factory module owners".to_string())
                    })?;
                (state, false)
            } else {
                let state = Arc::new(FactoryContextState {
                    identity: key,
                    owner_thread: thread::current().id(),
                    modules: AtomicUsize::new(1),
                    bound: AtomicBool::new(false),
                    context: ComWrapper::new(FactoryHostContext::new()),
                });
                index.push((key, Arc::downgrade(&state)));
                (state, true)
            }
        };
        let module = Self { factory, native, services };
        if first {
            if let Some(factory3) = module.factory.cast::<IPluginFactory3>() {
                let context = module
                    .services
                    .context
                    .to_com_ptr::<IHostApplication>()
                    .expect("factory context implements IHostApplication");
                // SAFETY: this stable context survives every module lease and unload.
                let result = unsafe { factory3.setHostContext(context.as_ptr().cast()) };
                if result != kResultOk && result != kResultTrue {
                    // Preserve optional Factory3 refusal semantics on all platforms.
                    log::warn!("IPluginFactory3::setHostContext failed: {result:#x}");
                }
            }
            module.services.bound.store(true, Ordering::Release);
        }
        Ok(module)
    }
}

impl VstModule for FactoryModule {
    fn visit_owned_storage(
        &self,
        visitor: &mut dyn HostStorageVisitor,
    ) -> std::result::Result<(), HostStorageError> {
        visitor.visit(HostStorageRef::boxed(&self.native))?;
        self.native.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::arc(&self.services))?;
        visitor.visit(HostStorageRef::com(&self.services.context))?;
        self.services.context.visit_owned_storage(visitor)?;
        // The shared index is host-owned storage too. Its actual capacity is
        // observable; no opaque tree-node heap is hidden behind this inventory.
        let index = contexts().lock().map_err(|_| HostStorageError::PoisonedOwner)?;
        visitor.visit(HostStorageRef::vector(&index)?)
    }

    fn get_factory(&self) -> Result<*mut IPluginFactory> {
        Ok(self.factory.clone().into_raw())
    }

    fn path(&self) -> &Path {
        self.native.path()
    }

    #[cfg(target_os = "linux")]
    fn factory_run_loop(&self) -> Option<&Arc<Mutex<RunLoopRegistry>>> {
        Some(&self.services.context.run_loop)
    }
}

impl Drop for FactoryModule {
    fn drop(&mut self) {
        let last = {
            let _index = contexts().lock().unwrap_or_else(|poison| poison.into_inner());
            self.services.modules.fetch_sub(1, Ordering::AcqRel) == 1
        };
        if last {
            // Shutdown rejects reentrant registration and releases native owners
            // outside ALL host locks, while this module still keeps code mapped.
            #[cfg(target_os = "linux")]
            RunLoopRegistry::shutdown(&self.services.context.run_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::sync::atomic::AtomicI32;

    #[derive(Default)]
    struct Observed {
        binds: AtomicUsize,
        context: AtomicUsize,
        host: Mutex<Option<vst3::ComWeak<FactoryHostContext>>>,
        at_unload: Mutex<Vec<(bool, usize)>>,
        timer_drops: AtomicUsize,
    }

    struct FactoryProbe(Arc<Observed>);
    impl Class for FactoryProbe {
        type Interfaces = (IPluginFactory3,);
    }
    impl IPluginFactoryTrait for FactoryProbe {
        unsafe fn getFactoryInfo(&self, _info: *mut PFactoryInfo) -> tresult {
            kResultFalse
        }
        unsafe fn countClasses(&self) -> i32 {
            0
        }
        unsafe fn getClassInfo(&self, _index: i32, _info: *mut PClassInfo) -> tresult {
            kResultFalse
        }
        unsafe fn createInstance(
            &self,
            _cid: FIDString,
            _iid: FIDString,
            obj: *mut *mut std::ffi::c_void,
        ) -> tresult {
            if !obj.is_null() {
                *obj = std::ptr::null_mut();
            }
            kNoInterface
        }
    }
    impl IPluginFactory2Trait for FactoryProbe {
        unsafe fn getClassInfo2(&self, _index: i32, _info: *mut PClassInfo2) -> tresult {
            kResultFalse
        }
    }
    impl IPluginFactory3Trait for FactoryProbe {
        unsafe fn getClassInfoUnicode(&self, _index: i32, _info: *mut PClassInfoW) -> tresult {
            kResultFalse
        }
        unsafe fn setHostContext(&self, context: *mut FUnknown) -> tresult {
            self.0.context.store(context as usize, Ordering::SeqCst);
            self.0.binds.fetch_add(1, Ordering::SeqCst);
            kResultOk
        }
    }

    struct ModuleProbe {
        factory: ComWrapper<FactoryProbe>,
        observed: Arc<Observed>,
    }
    impl VstModule for ModuleProbe {
        fn visit_owned_storage(
            &self,
            _visitor: &mut dyn HostStorageVisitor,
        ) -> std::result::Result<(), HostStorageError> {
            Ok(())
        }
        fn get_factory(&self) -> Result<*mut IPluginFactory> {
            Ok(self.factory.to_com_ptr::<IPluginFactory>().unwrap().into_raw())
        }
        fn path(&self) -> &Path {
            Path::new("native-factory-ABI-probe")
        }
    }
    impl Drop for ModuleProbe {
        fn drop(&mut self) {
            if let Some(host) = self.observed.host.lock().unwrap().as_ref() {
                self.observed.at_unload.lock().unwrap().push((
                    host.is_alive(),
                    self.observed.timer_drops.load(Ordering::SeqCst),
                ));
            }
        }
    }
    fn attach(
        factory: &ComWrapper<FactoryProbe>,
        observed: &Arc<Observed>,
    ) -> Result<FactoryModule> {
        let module = FactoryModule::attach(Box::new(ModuleProbe {
            factory: factory.clone(),
            observed: Arc::clone(observed),
        }))?;
        *observed.host.lock().unwrap() = Some(module.services.context.downgrade());
        Ok(module)
    }

    #[test]
    fn native_factory_identity_keeps_one_context_across_instances_and_discovery() {
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let first = attach(&factory, &observed).unwrap();
        let key = first.services.identity;
        let pointer = observed.context.load(Ordering::SeqCst);
        let second = attach(&factory, &observed).unwrap();
        assert!(Arc::ptr_eq(&first.services, &second.services));
        assert_eq!(observed.binds.load(Ordering::SeqCst), 1);
        assert_eq!(observed.context.load(Ordering::SeqCst), pointer);
        drop(first);
        assert!(observed.host.lock().unwrap().as_ref().unwrap().is_alive());
        assert!(contexts().lock().unwrap().iter().any(|(id, _)| *id == key));
        drop(second);
        assert_eq!(*observed.at_unload.lock().unwrap(), [(true, 0), (true, 0)]);
        assert!(!observed.host.lock().unwrap().as_ref().unwrap().is_alive());
        assert!(!contexts().lock().unwrap().iter().any(|(id, _)| *id == key));
    }

    #[test]
    fn another_control_thread_cannot_replace_a_live_factory_context() {
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let owner = attach(&factory, &observed).unwrap();
        let peer = factory.clone();
        let record = Arc::clone(&observed);
        let refusal = std::thread::spawn(move || {
            matches!(attach(&peer, &record), Err(Error::PluginLoadFailed(_)))
        })
        .join()
        .unwrap();
        assert!(refusal);
        assert_eq!(observed.binds.load(Ordering::SeqCst), 1);
        assert_eq!(owner.services.modules.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn factory_context_does_not_share_instance_progress_or_data_exchange() {
        let context = ComWrapper::new(FactoryHostContext::new());
        assert!(context.to_com_ptr::<IHostApplication>().is_some());
        assert!(context.to_com_ptr::<IProgress>().is_none());
        assert!(context.to_com_ptr::<IDataExchangeHandler>().is_none());
        assert!(context.to_com_ptr::<IPlugInterfaceSupport>().is_none());
    }

    #[test]
    fn module_inventory_includes_the_actual_shared_ownership_index() {
        struct Inventory(Vec<(usize, usize, crate::host_storage::HostStorageKind)>);
        impl HostStorageVisitor for Inventory {
            fn visit(
                &mut self,
                value: HostStorageRef<'_>,
            ) -> std::result::Result<(), HostStorageError> {
                self.0.push((value.address(), value.layout().size(), value.kind()));
                Ok(())
            }
        }
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let module = attach(&factory, &observed).unwrap();
        let mut inventory = Inventory(Vec::new());
        module.visit_owned_storage(&mut inventory).unwrap();
        // The ownership index is visited last under its borrow. Do not compare
        // a pointer captured before visiting: another factory may grow the
        // shared vector concurrently, legitimately replacing that allocation.
        let observed = inventory.0.last().unwrap();
        assert_ne!(observed.0, 0);
        assert!(observed.1 >= std::mem::size_of::<(usize, Weak<FactoryContextState>)>());
        assert_eq!(
            observed.2,
            crate::host_storage::HostStorageKind::CompleteAllocation
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires explicit real MONDRIAN_VST3_FACTORY_PLUGIN ELF/bundle and supervised native execution"]
    fn real_sdk_factory_context_survives_peer_instance_and_discovery_teardown() {
        let path = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_FACTORY_PLUGIN").expect("explicit native VST3"),
        );
        assert!(path.is_absolute() && path.exists());
        let path = path.canonicalize().unwrap();
        for _ in 0..4 {
            let first = crate::internal::module_loader::load_module(&path).unwrap();
            let peer = crate::internal::module_loader::load_module(&path).unwrap();
            assert!(Arc::ptr_eq(
                first.factory_run_loop().unwrap(),
                peer.factory_run_loop().unwrap()
            ));
            let before = {
                let factory = unsafe { ComPtr::from_raw(first.get_factory().unwrap()) }.unwrap();
                factory.cast::<FUnknown>().unwrap().as_ptr() as usize
            };
            let info = crate::discovery::get_plugin_info(&path)
                .expect("actual component/controller discovery while module owners are live");
            assert_eq!(info.uid.len(), 32);
            assert!(info.uid.bytes().all(|b| b.is_ascii_hexdigit()));
            let after = {
                let factory = unsafe { ComPtr::from_raw(peer.get_factory().unwrap()) }.unwrap();
                factory.cast::<FUnknown>().unwrap().as_ptr() as usize
            };
            assert_eq!(before, after);
            drop(first);
            // No editor is created. Actual native SDK factory retains the Linux
            // context, while this callback probes the host ABI/service lifetime.
            let calls = Arc::new(AtomicUsize::new(0));
            let record = Arc::new(Observed::default());
            let timer = ComWrapper::new(TimerProbe {
                hits: Arc::clone(&calls),
                observed: record,
                on_drop: None,
            })
            .to_com_ptr::<Linux::ITimerHandler>()
            .unwrap();
            let registry = peer.factory_run_loop().unwrap();
            assert_eq!(
                unsafe { RunLoopRegistry::register_timer(registry, timer.as_ptr(), 1) },
                kResultOk
            );
            for timer in &mut registry.lock().unwrap().timers {
                timer.due = std::time::Instant::now();
            }
            crate::internal::run_loop::service(registry).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                unsafe { RunLoopRegistry::unregister_timer(registry, timer.as_ptr()) },
                kResultOk
            );
            drop(timer);
            drop(peer);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn factory_fd_handler_uses_actual_readiness_and_unregisters_without_a_view() {
        use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixStream};
        use Linux::{IEventHandler, IEventHandlerTrait, IRunLoopTrait};
        struct EventProbe(Arc<AtomicI32>);
        impl Class for EventProbe {
            type Interfaces = (IEventHandler,);
        }
        impl IEventHandlerTrait for EventProbe {
            unsafe fn onFDIsSet(&self, fd: Linux::FileDescriptor) {
                self.0.store(fd, Ordering::SeqCst);
            }
        }
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let module = attach(&factory, &observed).unwrap();
        let run_loop = module.services.context.to_com_ptr::<Linux::IRunLoop>().unwrap();
        let (read, mut write) = UnixStream::pair().unwrap();
        let actual = Arc::new(AtomicI32::new(-1));
        let event = ComWrapper::new(EventProbe(Arc::clone(&actual)))
            .to_com_ptr::<IEventHandler>()
            .unwrap();
        assert_eq!(
            unsafe { run_loop.registerEventHandler(event.as_ptr(), -1) },
            kInvalidArgument
        );
        assert_eq!(
            unsafe { run_loop.registerEventHandler(event.as_ptr(), read.as_raw_fd()) },
            kResultOk
        );
        write.write_all(b"x").unwrap();
        crate::internal::run_loop::service(module.factory_run_loop().unwrap()).unwrap();
        assert_eq!(actual.swap(-1, Ordering::SeqCst), read.as_raw_fd());
        assert_eq!(
            unsafe { run_loop.unregisterEventHandler(event.as_ptr()) },
            kResultOk
        );
        crate::internal::run_loop::service(module.factory_run_loop().unwrap()).unwrap();
        assert_eq!(actual.load(Ordering::SeqCst), -1);
    }

    #[cfg(target_os = "linux")]
    struct TimerProbe {
        hits: Arc<AtomicUsize>,
        observed: Arc<Observed>,
        on_drop: Option<Box<dyn Fn()>>,
    }
    #[cfg(target_os = "linux")]
    impl Class for TimerProbe {
        type Interfaces = (Linux::ITimerHandler,);
    }
    #[cfg(target_os = "linux")]
    impl Linux::ITimerHandlerTrait for TimerProbe {
        unsafe fn onTimer(&self) {
            self.hits.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[cfg(target_os = "linux")]
    impl Drop for TimerProbe {
        fn drop(&mut self) {
            self.observed.timer_drops.fetch_add(1, Ordering::SeqCst);
            if let Some(callback) = &self.on_drop {
                callback();
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn make_due(module: &FactoryModule) {
        for timer in &mut module.factory_run_loop().unwrap().lock().unwrap().timers {
            timer.due = std::time::Instant::now();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn factory_timer_runs_without_editor_and_survives_another_module_owner_closing() {
        use Linux::IRunLoopTrait;
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let first = attach(&factory, &observed).unwrap();
        let second = attach(&factory, &observed).unwrap();
        let run_loop = first.services.context.to_com_ptr::<Linux::IRunLoop>().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let timer = ComWrapper::new(TimerProbe {
            hits: Arc::clone(&hits),
            observed: Arc::clone(&observed),
            on_drop: None,
        })
        .to_com_ptr::<Linux::ITimerHandler>()
        .unwrap();
        assert_eq!(
            unsafe { run_loop.registerTimer(timer.as_ptr(), 1) },
            kResultOk
        );
        drop(timer);
        drop(run_loop);
        make_due(&first);
        crate::internal::run_loop::service(first.factory_run_loop().unwrap()).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(observed.timer_drops.load(Ordering::SeqCst), 0);
        make_due(&second);
        crate::internal::run_loop::service(second.factory_run_loop().unwrap()).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        drop(second);
        assert_eq!(observed.timer_drops.load(Ordering::SeqCst), 1);
        assert_eq!(*observed.at_unload.lock().unwrap(), [(true, 0), (true, 1)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn final_module_releases_callbacks_outside_locks_and_rejects_reentrant_registration() {
        use Linux::IRunLoopTrait;
        let observed = Arc::new(Observed::default());
        let factory = ComWrapper::new(FactoryProbe(Arc::clone(&observed)));
        let module = attach(&factory, &observed).unwrap();
        let weak = Arc::downgrade(&module.services);
        let result = Arc::new(AtomicI32::new(kInternalError));
        let output = Arc::clone(&result);
        let record = Arc::clone(&observed);
        let callback = move || {
            assert!(contexts().try_lock().is_ok());
            let services = weak.upgrade().unwrap();
            assert!(services.context.run_loop.try_lock().is_ok());
            let next = ComWrapper::new(TimerProbe {
                hits: Arc::new(AtomicUsize::new(0)),
                observed: Arc::clone(&record),
                on_drop: None,
            })
            .to_com_ptr::<Linux::ITimerHandler>()
            .unwrap();
            let run_loop = services.context.to_com_ptr::<Linux::IRunLoop>().unwrap();
            output.store(
                unsafe { run_loop.registerTimer(next.as_ptr(), 1) },
                Ordering::SeqCst,
            );
        };
        let timer = ComWrapper::new(TimerProbe {
            hits: Arc::new(AtomicUsize::new(0)),
            observed: Arc::clone(&observed),
            on_drop: Some(Box::new(callback)),
        })
        .to_com_ptr::<Linux::ITimerHandler>()
        .unwrap();
        let run_loop = module.services.context.to_com_ptr::<Linux::IRunLoop>().unwrap();
        assert_eq!(
            unsafe { run_loop.registerTimer(timer.as_ptr(), 1) },
            kResultOk
        );
        drop(timer);
        drop(run_loop);
        drop(module);
        assert_eq!(result.load(Ordering::SeqCst), kResultFalse);
        assert_eq!(observed.timer_drops.load(Ordering::SeqCst), 2);
        assert_eq!(*observed.at_unload.lock().unwrap(), [(true, 2)]);
    }
}
