//! Weak tracking of host-created COM allocations retained by plugin references.

use super::bounded_configuration_stream::ConfigurationCaptureStream;
use super::com_implementations::{HostAttributeList, HostContextMenu, HostMessage, MemoryStream};
use crate::host_storage::{HostStorageError, HostStorageRef, HostStorageVisitor};
use std::sync::Mutex;
use vst3::{ComWeak, ComWrapper};

const MAX_RETAINED_HOST_COM_OBJECTS: usize = 4096;

enum RetainedComOwner {
    Message(ComWeak<HostMessage>),
    Menu(ComWeak<HostContextMenu>),
    Attributes(ComWeak<HostAttributeList>),
    Stream(ComWeak<MemoryStream>),
    Capture(ComWeak<ConfigurationCaptureStream>),
}

impl RetainedComOwner {
    fn alive(&self) -> bool {
        match self {
            Self::Message(value) => value.is_alive(),
            Self::Menu(value) => value.is_alive(),
            Self::Attributes(value) => value.is_alive(),
            Self::Stream(value) => value.is_alive(),
            Self::Capture(value) => value.is_alive(),
        }
    }

    fn payload(&self) -> HostStorageRef<'_> {
        match self {
            Self::Message(value) => HostStorageRef::weak_com(value),
            Self::Menu(value) => HostStorageRef::weak_com(value),
            Self::Attributes(value) => HostStorageRef::weak_com(value),
            Self::Stream(value) => HostStorageRef::weak_com(value),
            Self::Capture(value) => HostStorageRef::weak_com(value),
        }
    }

    fn upgrade(&self) -> Option<ObservedComOwner> {
        match self {
            Self::Message(value) => value.upgrade().map(ObservedComOwner::Message),
            Self::Menu(value) => value.upgrade().map(ObservedComOwner::Menu),
            Self::Attributes(value) => value.upgrade().map(ObservedComOwner::Attributes),
            Self::Stream(value) => value.upgrade().map(ObservedComOwner::Stream),
            Self::Capture(value) => value.upgrade().map(ObservedComOwner::Capture),
        }
    }
}

enum ObservedComOwner {
    Message(ComWrapper<HostMessage>),
    Menu(ComWrapper<HostContextMenu>),
    Attributes(ComWrapper<HostAttributeList>),
    Stream(ComWrapper<MemoryStream>),
    Capture(ComWrapper<ConfigurationCaptureStream>),
}

impl ObservedComOwner {
    fn visit(&self, visitor: &mut dyn HostStorageVisitor) -> Result<(), HostStorageError> {
        match self {
            Self::Message(value) => value.visit_owned_storage(visitor),
            Self::Menu(value) => value.visit_owned_storage(visitor),
            Self::Attributes(value) => value.visit_owned_storage(visitor),
            Self::Stream(value) => value.visit_owned_storage(visitor),
            Self::Capture(value) => value.visit_owned_storage(visitor),
        }
    }
}

struct ObservationBorrow<'a> {
    buffer: std::sync::MutexGuard<'a, Vec<ObservedComOwner>>,
}

impl Drop for ObservationBorrow<'_> {
    fn drop(&mut self) {
        // Covers both typed errors and a panicking visitor. The registry's
        // inner owners guard has already left scope before this retirement.
        self.buffer.clear();
    }
}

pub(crate) struct RetainedComStorage {
    owners: Mutex<Vec<RetainedComOwner>>,
    observations: Mutex<Vec<ObservedComOwner>>,
}

impl RetainedComStorage {
    pub(crate) fn new() -> Self {
        Self {
            owners: Mutex::new(Vec::new()),
            observations: Mutex::new(Vec::new()),
        }
    }

    fn register(&self, owner: RetainedComOwner) -> Result<(), HostStorageError> {
        let mut owners = self
            .owners
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        // A zero-strong Arc cannot resurrect; release dead weak allocations
        // on the caller's control/creation lane. This thread-safe registry
        // does not make arbitrary plugin createInstance calls realtime-safe.
        owners.retain(RetainedComOwner::alive);
        if owners
            .iter()
            .any(|existing| existing.payload().address() == owner.payload().address())
        {
            return Ok(());
        }
        if owners.len() == MAX_RETAINED_HOST_COM_OBJECTS {
            return Err(HostStorageError::RegistryFull);
        }
        if owners.len() == owners.capacity() {
            let additional = MAX_RETAINED_HOST_COM_OBJECTS
                .saturating_sub(owners.len())
                .min(64);
            owners
                .try_reserve_exact(additional)
                .map_err(|_| HostStorageError::AllocationUnavailable)?;
            if owners.capacity() > MAX_RETAINED_HOST_COM_OBJECTS {
                return Err(HostStorageError::RegistryFull);
            }
        }
        owners.push(owner);
        Ok(())
    }

    pub(crate) fn register_menu(
        &self,
        value: &ComWrapper<HostContextMenu>,
    ) -> Result<(), HostStorageError> {
        self.register(RetainedComOwner::Menu(value.downgrade()))
    }
    pub(crate) fn register_message(
        &self,
        value: &ComWrapper<HostMessage>,
    ) -> Result<(), HostStorageError> {
        // IMessage::getAttributes can be independently AddRef-retained. Its
        // allocation is not destroyed necessarily when the message dies.
        self.register_attributes(value.host_attributes())?;
        self.register(RetainedComOwner::Message(value.downgrade()))
    }
    pub(crate) fn register_attributes(
        &self,
        value: &ComWrapper<HostAttributeList>,
    ) -> Result<(), HostStorageError> {
        self.register(RetainedComOwner::Attributes(value.downgrade()))
    }
    pub(crate) fn register_memory_stream(
        &self,
        value: &ComWrapper<MemoryStream>,
    ) -> Result<(), HostStorageError> {
        // IStreamAttributes permits the same independent attribute lifetime.
        self.register_attributes(value.host_attributes())?;
        self.register(RetainedComOwner::Stream(value.downgrade()))
    }
    pub(crate) fn register_capture_stream(
        &self,
        value: &ComWrapper<ConfigurationCaptureStream>,
    ) -> Result<(), HostStorageError> {
        self.register(RetainedComOwner::Capture(value.downgrade()))
    }

    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn HostStorageVisitor,
    ) -> Result<(), HostStorageError> {
        // This is a bounded non-RT owning observation, not a final install
        // guard. A last-strong menu Drop may Release foreign plugin targets.
        // Keep that retirement outside the owners lock and all author/install
        // guards. The serial observer buffer itself is actual accounted storage.
        let mut observations = self
            .observations
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        if !observations.is_empty() {
            return Err(HostStorageError::UnavailableOwner);
        }
        if observations.capacity() < MAX_RETAINED_HOST_COM_OBJECTS {
            observations
                .try_reserve_exact(MAX_RETAINED_HOST_COM_OBJECTS)
                .map_err(|_| HostStorageError::AllocationUnavailable)?;
        }
        let mut observations = ObservationBorrow {
            buffer: observations,
        };
        let result = (|| {
            {
                let owners = self
                    .owners
                    .lock()
                    .map_err(|_| HostStorageError::PoisonedOwner)?;
                visitor.visit(HostStorageRef::vector(&owners)?)?;
                for owner in owners.iter() {
                    // Even a dead weak owner still owns the Arc allocation.
                    visitor.visit(owner.payload())?;
                    if let Some(owner) = owner.upgrade() {
                        observations.buffer.push(owner);
                    }
                }
            }
            visitor.visit(HostStorageRef::vector(&observations.buffer)?)?;
            for owner in observations.buffer.iter() {
                owner.visit(visitor)?;
            }
            Ok(())
        })();
        // Explicitly retire on both success and error, after owners unlocks.
        // No lease can be published or author/Core guard retained here.
        drop(observations);
        result
    }
}
#[cfg(test)]
mod retained_host_storage_contracts {
    use super::super::com_implementations::{
        create_host_message, create_memory_stream_from_with_metadata, StreamStateType,
    };
    use super::*;
    use crate::host_storage::{
        HostStorageError, HostStorageKind, HostStorageRef, HostStorageVisitor,
    };
    use vst3::{
        ComRef,
        Steinberg::Vst::{
            IAttributeList, IAttributeListTrait, IMessage, IMessageTrait, IStreamAttributes,
            IStreamAttributesTrait,
        },
    };

    #[derive(Clone, Copy)]
    struct Extent {
        address: usize,
        bytes: usize,
        kind: HostStorageKind,
    }
    struct Inventory {
        values: [Option<Extent>; 64],
        used: usize,
    }
    impl Inventory {
        fn new() -> Self {
            Self {
                values: [None; 64],
                used: 0,
            }
        }
        fn contains(&self, address: usize, minimum_bytes: usize, kind: HostStorageKind) -> bool {
            self.values[..self.used].iter().flatten().any(|value| {
                value.address == address && value.bytes >= minimum_bytes && value.kind == kind
            })
        }
    }
    impl HostStorageVisitor for Inventory {
        fn visit(&mut self, storage: HostStorageRef<'_>) -> Result<(), HostStorageError> {
            let Some(slot) = self.values.get_mut(self.used) else {
                return Err(HostStorageError::ObserverRefused);
            };
            *slot = Some(Extent {
                address: storage.address(),
                bytes: storage.layout().size(),
                kind: storage.kind(),
            });
            self.used += 1;
            Ok(())
        }
    }

    fn write_and_observe_payload(attributes: &vst3::ComPtr<IAttributeList>) -> (usize, usize) {
        let original = [77_u8; 4097];
        // SDK methods make a distinct host-owned binary payload; the caller's
        // fixture array address cannot accidentally satisfy the owning view.
        assert_eq!(
            unsafe {
                attributes.setBinary(
                    c"payload".as_ptr(),
                    original.as_ptr().cast(),
                    original.len() as u32,
                )
            },
            vst3::Steinberg::kResultOk
        );
        let mut address = std::ptr::null();
        let mut length = 0;
        assert_eq!(
            unsafe { attributes.getBinary(c"payload".as_ptr(), &mut address, &mut length) },
            vst3::Steinberg::kResultOk
        );
        assert_ne!(address as usize, original.as_ptr() as usize);
        assert_eq!(length as usize, original.len());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(address.cast::<u8>(), length as usize) },
            original
        );
        (address as usize, length as usize)
    }

    #[test]
    fn retained_host_storage_message_attributes_outlive_the_parent_com_class() {
        let registry = RetainedComStorage::new();
        let message = create_host_message();
        registry
            .register_message(&message)
            .expect("register before exposure");
        let attributes_identity = message.host_attributes().payload_storage().address();
        let message_interface = message.to_com_ptr::<IMessage>().expect("actual interface");
        let attributes =
            unsafe { ComRef::<IAttributeList>::from_raw(message_interface.getAttributes()) }
                .expect("borrowed attributes")
                .to_com_ptr();
        let (payload, length) = write_and_observe_payload(&attributes);
        drop(message_interface);
        drop(message);
        let mut inventory = Inventory::new();
        registry
            .visit_owned_storage(&mut inventory)
            .expect("retained attribute owner remains live");
        assert!(inventory.contains(attributes_identity, 1, HostStorageKind::ArcPayload));
        assert!(inventory.contains(payload, length, HostStorageKind::CompleteAllocation));
        drop(attributes);
        let mut inventory = Inventory::new();
        registry
            .visit_owned_storage(&mut inventory)
            .expect("dead weak class is never dereferenced");
        assert!(inventory.contains(attributes_identity, 1, HostStorageKind::ArcPayload));
        assert!(!inventory.contains(payload, 1, HostStorageKind::CompleteAllocation));
    }

    #[test]
    fn retained_host_storage_stream_attributes_outlive_the_parent_com_class() {
        let registry = RetainedComStorage::new();
        let stream =
            create_memory_stream_from_with_metadata(Vec::new(), None, StreamStateType::Project);
        registry
            .register_memory_stream(&stream)
            .expect("register before exposure");
        let attributes_identity = stream.host_attributes().payload_storage().address();
        let stream_interface = stream
            .to_com_ptr::<IStreamAttributes>()
            .expect("actual interface");
        let attributes =
            unsafe { ComRef::<IAttributeList>::from_raw(stream_interface.getAttributes()) }
                .expect("borrowed attributes")
                .to_com_ptr();
        let (payload, length) = write_and_observe_payload(&attributes);
        drop(stream_interface);
        drop(stream);
        let mut inventory = Inventory::new();
        registry
            .visit_owned_storage(&mut inventory)
            .expect("retained attribute owner remains live");
        assert!(inventory.contains(attributes_identity, 1, HostStorageKind::ArcPayload));
        assert!(inventory.contains(payload, length, HostStorageKind::CompleteAllocation));
    }

    #[test]
    fn retained_host_storage_refused_observer_retires_all_temporary_strong_owners() {
        struct RefuseAfterUpgrade(usize);
        impl HostStorageVisitor for RefuseAfterUpgrade {
            fn visit(&mut self, _: HostStorageRef<'_>) -> Result<(), HostStorageError> {
                self.0 += 1;
                if self.0 == 4 {
                    Err(HostStorageError::ObserverRefused)
                } else {
                    Ok(())
                }
            }
        }
        let registry = RetainedComStorage::new();
        let message = create_host_message();
        registry.register_message(&message).expect("register");
        assert_eq!(
            registry.visit_owned_storage(&mut RefuseAfterUpgrade(0)),
            Err(HostStorageError::ObserverRefused)
        );
        assert!(registry
            .observations
            .lock()
            .expect("not poisoned")
            .is_empty());
        registry
            .visit_owned_storage(&mut Inventory::new())
            .expect("same registry remains usable");
    }
}
