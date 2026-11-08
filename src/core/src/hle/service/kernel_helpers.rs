// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/kernel_helpers.h and kernel_helpers.cpp
//! Status: kernel-backed service event ownership ported
//!
//! Contains:
//! - ServiceContext: provides kernel resource management for services (event creation/destruction)
//!
//! Upstream creates real KEvent objects via KernelCore. The service-layer
//! Event wrapper owns the corresponding Rust KEvent/KReadableEvent bridge and
//! also provides the host Condvar used by service threads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_process::{KProcess, ProcessLock};
use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::kernel::k_resource_limit::LimitableResource;
use crate::hle::kernel::k_scoped_resource_reservation::KScopedResourceReservation;
use crate::hle::result::RESULT_SUCCESS;

use super::os::event::Event;

/// Provides kernel resource management for HLE services.
///
/// Corresponds to upstream `Service::KernelHelpers::ServiceContext`.
///
/// Upstream holds `KernelCore& m_kernel` and `KProcess* m_process`, and creates
/// KEvent objects via `KEvent::Create(kernel)`. Rust retains the service-layer
/// Event owner by handle while its bridge keeps the matching kernel objects.
pub struct ServiceContext {
    // Upstream retains system.Kernel(); explicit owners must not depend on
    // whichever guest thread happens to be current when CreateEvent is called.
    system: Option<crate::core::SystemRef>,
    name: String,
    events: BTreeMap<u32, Arc<Event>>,
    reserved_event_handles: BTreeSet<u32>,
    next_handle: u32,
    process: Option<Arc<ProcessLock>>,
    process_created: bool,
}

impl ServiceContext {
    /// Creates a new ServiceContext.
    ///
    /// Upstream: obtains current process from kernel, or creates one if none exists.
    pub fn new(name: String) -> Self {
        Self::new_impl(None, name)
    }

    pub fn new_with_system(system: crate::core::SystemRef, name: String) -> Self {
        Self::new_impl(Some(system), name)
    }

    fn new_impl(system: Option<crate::core::SystemRef>, name: String) -> Self {
        let kernel = match system.as_ref() {
            Some(system) => system.get().kernel(),
            None => crate::hle::kernel::kernel::get_kernel_ref(),
        };
        let current_process =
            crate::hle::kernel::kernel::get_current_thread_pointer().and_then(|thread| {
                thread
                    .lock()
                    .unwrap()
                    .parent
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
            });
        let (process, process_created) = match current_process {
            Some(process) => (Some(process), false),
            None => match kernel {
                Some(kernel) => {
                    let process = Arc::new(ProcessLock::from_value(KProcess::with_hardware_profile(kernel.hardware_profile())));
                    {
                        let mut process_guard = process.lock().unwrap();
                        process_guard.process_id = kernel.create_new_kernel_process_id();
                        let result = process_guard.initialize(
                            &[],
                            0,
                            0,
                            0,
                            0,
                            0,
                            kernel.get_system_resource_limit(),
                            false,
                        );
                        assert_eq!(result, RESULT_SUCCESS.get_inner_value());
                        process_guard.bind_self_reference(&process);
                        if let Some(scheduler) = kernel.scheduler(0) {
                            process_guard.attach_scheduler(scheduler);
                        }
                    }
                    kernel.register_process(Arc::clone(&process));
                    (Some(process), true)
                }
                None => (None, false),
            },
        };

        Self {
            system,
            name,
            events: BTreeMap::new(),
            reserved_event_handles: BTreeSet::new(),
            next_handle: 1, // 0 is reserved as invalid
            process,
            process_created,
        }
    }

    fn create_kernel_backed_event(
        process: &Arc<ProcessLock>,
        event_object_id: u64,
        readable_event_object_id: u64,
    ) -> Arc<Event> {
        let owner_process_id = process.lock().unwrap().get_process_id();
        let event = Arc::new(Mutex::new(KEvent::new()));
        let readable_event = Arc::new(Mutex::new(KReadableEvent::new()));

        event
            .lock()
            .unwrap()
            .initialize(owner_process_id, readable_event_object_id);
        readable_event
            .lock()
            .unwrap()
            .initialize(event_object_id, readable_event_object_id);

        {
            let mut process_guard = process.lock().unwrap();
            process_guard.register_event_object(event_object_id, Arc::clone(&event));
            process_guard.register_readable_event_object(
                readable_event_object_id,
                Arc::clone(&readable_event),
            );
        }

        Arc::new(Event::new_with_kernel_event(
            event,
            readable_event,
            Arc::clone(process),
        ))
    }

    /// Returns the name of this service context.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Creates a new kernel event and returns its handle.
    ///
    /// Upstream: reserves event from process resource limit via
    /// KScopedResourceReservation, creates KEvent via KEvent::Create(kernel),
    /// initializes it, registers it, and returns the event.
    pub fn create_event(&mut self, name: String) -> u32 {
        let handle = self.next_handle;
        self.next_handle += 1;

        let mut event_reservation = self.process.as_ref().map(|process| {
            let resource_limit = process.lock().unwrap().resource_limit.clone();
            KScopedResourceReservation::new(resource_limit, LimitableResource::EventCountMax, 1)
        });
        if event_reservation
            .as_ref()
            .is_some_and(|reservation| !reservation.succeeded())
        {
            log::error!(
                "ServiceContext '{}': event resource limit reached",
                self.name
            );
            return 0;
        }

        let event = match (
            self.process.as_ref(),
            match self.system.as_ref() {
                Some(system) => system.get().kernel(),
                None => crate::hle::kernel::kernel::get_kernel_ref(),
            },
        ) {
            (Some(process), Some(kernel)) => Self::create_kernel_backed_event(
                process,
                kernel.create_new_object_id() as u64,
                kernel.create_new_object_id() as u64,
            ),
            _ => Arc::new(Event::new()),
        };
        self.events.insert(handle, event);
        if let Some(reservation) = event_reservation.as_mut() {
            reservation.commit();
            self.reserved_event_handles.insert(handle);
        }

        log::debug!(
            "ServiceContext '{}': created event '{}' with handle {}",
            self.name,
            name,
            handle,
        );

        handle
    }

    /// Get a reference to a created event by handle.
    pub fn get_event(&self, handle: u32) -> Option<Arc<Event>> {
        self.events.get(&handle).cloned()
    }

    /// Closes a kernel event.
    ///
    /// Upstream: closes the readable event and the event itself.
    pub fn close_event(&mut self, event_handle: u32) {
        if let Some(event) = self.events.remove(&event_handle) {
            if self.reserved_event_handles.remove(&event_handle) {
                let resource_limit = self
                    .process
                    .as_ref()
                    .and_then(|process| process.lock().unwrap().resource_limit.clone());
                if let Some(resource_limit) = resource_limit {
                    resource_limit.release(LimitableResource::EventCountMax, 1);
                }
            }
            drop(event);
            log::debug!(
                "ServiceContext '{}': closed event handle {}",
                self.name,
                event_handle,
            );
        }
    }
}

impl Drop for ServiceContext {
    fn drop(&mut self) {
        // Close all remaining events.
        let handles: Vec<u32> = self.events.keys().copied().collect();
        for handle in handles {
            self.close_event(handle);
        }

        if self.process_created {
            if let Some(process) = self.process.as_ref() {
                process.lock().unwrap().finalize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_profile_service_process_inherits_explicit_kernel() {
        let _without_global_kernel = crate::hle::kernel::kernel::ScopedKernelForTest::without_kernel();
        for profile in [crate::hardware_properties::SWITCH1_PROFILE, crate::hardware_properties::NX2_PROFILE] {
            let system = Box::new(crate::core::System::new_for_test_with_hardware_profile(profile));
            let context = ServiceContext::new_with_system(crate::core::SystemRef::from_ref(&system), "profile".into());
            assert!(context.process_created);
            assert_eq!(context.process.as_ref().unwrap().lock().unwrap().hardware_profile(), profile);
        }
    }


    #[test]
    fn explicit_system_keeps_kernel_event_owner_without_current_guest_thread() {
        let _without_global_kernel = crate::hle::kernel::kernel::ScopedKernelForTest::without_kernel();
        let system = crate::core::System::new_boxed_for_test();
        let mut context = ServiceContext::new_with_system(
            crate::core::SystemRef::from_ref(&system),
            "nvdrv".to_owned(),
        );
        let handle = context.create_event("channel".to_owned());
        assert_ne!(handle, 0);
        let event = context.get_event(handle).unwrap();
        let readable = event.readable_event().expect("real kernel readable end");
        let id = readable.lock().unwrap().object_id;
        assert!(context.process.as_ref().unwrap().lock().unwrap()
            .get_readable_event_by_object_id(id).is_some());
        assert!(Arc::ptr_eq(&readable, &event.readable_event().unwrap()));
        context.close_event(handle);
        assert!(context.get_event(handle).is_none());
        // Closing the service's ownership does not invalidate a copied handle.
        assert_eq!(readable.lock().unwrap().object_id, id);
    }

    #[test]
    fn test_service_context_creation() {
        let ctx = ServiceContext::new("test_service".to_string());
        assert_eq!(ctx.name(), "test_service");
    }

    #[test]
    fn test_create_and_close_event() {
        let mut ctx = ServiceContext::new("test".to_string());
        let handle = ctx.create_event("my_event".to_string());
        assert!(handle > 0);
        assert!(ctx.get_event(handle).is_some());

        ctx.close_event(handle);
        assert!(ctx.get_event(handle).is_none());
    }

    #[test]
    fn test_event_signal_clear() {
        let mut ctx = ServiceContext::new("test".to_string());
        let handle = ctx.create_event("sig_event".to_string());
        let event = ctx.get_event(handle).unwrap();

        assert!(!event.is_signaled());
        event.signal();
        assert!(event.is_signaled());
        event.clear();
        assert!(!event.is_signaled());
    }

    #[test]
    fn kernel_backed_event_exposes_persistent_readable_end() {
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 0x1234;

        let event = ServiceContext::create_kernel_backed_event(&process, 0x40, 0x41);
        let first = event.readable_event().unwrap();
        let second = event.readable_event().unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.lock().unwrap().object_id, 0x41);
        assert!(process
            .lock()
            .unwrap()
            .get_event_by_object_id(0x40)
            .is_some());
        assert!(process
            .lock()
            .unwrap()
            .get_readable_event_by_object_id(0x41)
            .is_some());
    }

    #[test]
    fn event_reservation_is_released_when_service_owner_closes() {
        use crate::hle::kernel::k_resource_limit::KResourceLimit;

        let resource_limit = Arc::new(KResourceLimit::new());
        resource_limit.initialize();
        resource_limit
            .set_limit_value(LimitableResource::EventCountMax, 1)
            .unwrap();

        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().resource_limit = Some(Arc::clone(&resource_limit));
        let mut context = ServiceContext {
            system: None,
            name: "test".to_string(),
            events: BTreeMap::new(),
            reserved_event_handles: BTreeSet::new(),
            next_handle: 1,
            process: Some(process),
            process_created: false,
        };

        let first = context.create_event("first".to_string());
        assert_ne!(first, 0);
        assert_eq!(context.create_event("exhausted".to_string()), 0);

        context.close_event(first);
        assert_ne!(context.create_event("after-close".to_string()), 0);
    }
}
