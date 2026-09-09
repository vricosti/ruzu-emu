//! Port of zuyu/src/core/hle/service/hid/hidbus.h and hidbus.cpp
//!
//! Hidbus service ("hidbus").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_readable_event::KReadableEvent;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Core-side ownership bridge for HidbusBase's command completion event.
/// Upstream creates/closes this through ServiceContext in HidbusBase. The Rust
/// bridge stays here because hid_core cannot depend on core without a cycle.
pub struct HidbusCommandEvent {
    event: Arc<Event>,
    readable: Arc<Mutex<KReadableEvent>>,
    context: Arc<Mutex<ServiceContext>>,
    handle: u32,
}

impl HidbusCommandEvent {
    pub fn new(context: Arc<Mutex<ServiceContext>>) -> Option<Self> {
        let mut owner = context.lock().unwrap();
        let handle = owner.create_event("hidbus:SendCommandAsyncEvent".into());
        if handle == 0 {
            return None;
        }
        let event = owner.get_event(handle).expect("newly created service event");
        let Some(readable) = event.readable_event() else {
            // A host-only event cannot be returned as a guest readable handle.
            owner.close_event(handle);
            return None;
        };
        drop(owner);
        Some(Self { event, readable, context, handle })
    }

    /// HidbusBase::GetSendCommandAsycEvent, used by the IPC copy-handle response.
    pub fn readable_event(&self) -> Arc<Mutex<KReadableEvent>> {
        Arc::clone(&self.readable)
    }
}

impl hid_core::hidbus::hidbus_base::HidbusCommandEvent for HidbusCommandEvent {
    fn signal(&self) {
        // Do not hold the ServiceContext mutex while waking guest waiters.
        self.event.signal();
    }
}

impl Drop for HidbusCommandEvent {
    fn drop(&mut self) {
        self.context.lock().unwrap().close_event(self.handle);
    }
}

/// Hidbus service for HID bus communication (e.g., Ring-Con).
pub struct Hidbus {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Hidbus {
    fn stub_success_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let cmd = ctx.get_command();
        log::debug!("(STUBBED) hidbus command {}", cmd);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (1, Some(Self::stub_success_handler), "GetBusHandle"),
            (
                2,
                Some(Self::stub_success_handler),
                "IsExternalDeviceConnected",
            ),
            (3, Some(Self::stub_success_handler), "Initialize"),
            (4, Some(Self::stub_success_handler), "Finalize"),
            (5, Some(Self::stub_success_handler), "EnableExternalDevice"),
            (6, Some(Self::stub_success_handler), "GetExternalDeviceId"),
            (7, Some(Self::stub_success_handler), "SendCommandAsync"),
            (
                8,
                Some(Self::stub_success_handler),
                "GetSendCommandAsynceResult",
            ),
            (
                9,
                Some(Self::stub_success_handler),
                "SetEventForSendCommandAsycResult",
            ),
            (
                10,
                Some(Self::stub_success_handler),
                "GetSharedMemoryHandle",
            ),
            (
                11,
                Some(Self::stub_success_handler),
                "EnableJoyPollingReceiveMode",
            ),
            (
                12,
                Some(Self::stub_success_handler),
                "DisableJoyPollingReceiveMode",
            ),
            (13, None, "GetPollingData"),
            (14, Some(Self::stub_success_handler), "SetStatusManagerType"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for Hidbus {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hidbus"
    }
}

impl ServiceFramework for Hidbus {
    fn get_service_name(&self) -> &str {
        "hidbus"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_event_owns_kernel_endpoint_and_service_registration() {
        use super::*;
        use hid_core::hidbus::hidbus_base::HidbusCommandEvent as _;
        use crate::hle::kernel::k_resource_limit::LimitableResource;
        const CHILD: &str = "RUZU_TEST_HIDBUS_KERNEL_EVENT";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::hid::hidbus::tests::command_event_owns_kernel_endpoint_and_service_registration"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        let host_context = Arc::new(Mutex::new(ServiceContext::new("host-only".into())));
        assert!(HidbusCommandEvent::new(Arc::clone(&host_context)).is_none());
        assert!(host_context.lock().unwrap().get_event(1).is_none());

        let mut kernel = crate::hle::kernel::kernel::ScopedKernelForTest::new();
        kernel.kernel_mut().initialize_system_resource_limit(8 * 1024 * 1024, 0);
        let limit = kernel.kernel_mut().get_system_resource_limit().unwrap();
        limit.set_limit_value(LimitableResource::EventCountMax, 1).unwrap();
        let context = Arc::new(Mutex::new(ServiceContext::new("hidbus-test".into())));
        let owner = HidbusCommandEvent::new(Arc::clone(&context)).unwrap();
        assert_eq!(limit.get_current_value(LimitableResource::EventCountMax), 1);
        assert!(HidbusCommandEvent::new(Arc::clone(&context)).is_none());
        let handle = owner.handle;
        let readable = owner.readable_event();
        assert!(Arc::ptr_eq(&readable, &owner.readable_event()));
        assert!(!owner.event.is_signaled());
        // Signal must not reacquire the context mutex.
        let guard = context.lock().unwrap();
        owner.signal();
        assert!(owner.event.is_signaled());
        assert!(readable.lock().unwrap().is_signaled());
        assert!(guard.get_event(handle).is_some());
        drop(guard);
        drop(owner);
        assert!(context.lock().unwrap().get_event(handle).is_none());
        assert_eq!(limit.get_current_value(LimitableResource::EventCountMax), 0);
        let owner = HidbusCommandEvent::new(Arc::clone(&context)).unwrap();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_some());
        drop(owner);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn polling_data_is_registered_without_a_handler() {
        let service = super::Hidbus::new();
        let command = service.handlers.get(&13).unwrap();
        assert_eq!(command.name, "GetPollingData");
        assert!(command.handler_callback.is_none());
    }
}
