//! Port of core/hle/service/hid/hidbus.h and hidbus.cpp.

use crate::core::SystemRef;
use crate::core_timing::{self, CoreTiming, EventType, UnscheduleEventType};
use crate::hle::kernel::k_shared_memory::KSharedMemory;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use hid_core::frontend::emulated_controller::EmulatedController;
use hid_core::hid_types::NpadIdType;
use hid_core::hidbus::hidbus_base::{HidbusDevice, HidbusRuntime, JoyPollingMode};
use hid_core::hidbus::{ringcon::RingController, stubbed::HidbusStubbed};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

const MAX_NUMBER_OF_HANDLES: usize = 0x13;
const HIDBUS_UPDATE_NS: Duration = Duration::from_millis(15);

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[repr(transparent)]
struct BusHandle(u64);
impl BusHandle {
    fn internal_index(self) -> usize {
        ((self.0 >> 32) & 0xFF) as usize
    }
    fn player_number(self) -> u8 {
        (self.0 >> 40) as u8
    }
    fn bus_type_id(self) -> u8 {
        (self.0 >> 48) as u8
    }
    fn is_valid(self) -> bool {
        self.0 & (1 << 56) != 0
    }
    fn matches(self, other: Self) -> bool {
        // Upstream compares named bitfields, ignoring the seven reserved bits.
        self.0 & 0x01FF_FFFF_FFFF_FFFF == other.0 & 0x01FF_FFFF_FFFF_FFFF
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct HidbusStatusManagerEntry {
    is_connected: u8,
    padding: [u8; 3],
    is_connected_result: u32,
    is_enabled: u8,
    is_in_focus: u8,
    is_polling_mode: u8,
    reserved: u8,
    polling_mode: JoyPollingMode,
    tail: [u8; 0x70],
}
impl Default for HidbusStatusManagerEntry {
    fn default() -> Self {
        Self {
            is_connected: 0,
            padding: [0; 3],
            is_connected_result: 0,
            is_enabled: 0,
            is_in_focus: 0,
            is_polling_mode: 0,
            reserved: 0,
            polling_mode: JoyPollingMode::default(),
            tail: [0; 0x70],
        }
    }
}
#[repr(C)]
struct HidbusStatusManager {
    entries: [HidbusStatusManagerEntry; MAX_NUMBER_OF_HANDLES],
    padding: [u8; 0x680],
}
const _: () = {
    assert!(std::mem::size_of::<BusHandle>() == 8);
    assert!(std::mem::size_of::<HidbusStatusManagerEntry>() == 0x80);
    assert!(std::mem::size_of::<HidbusStatusManager>() == 0x1000);
};

/// Concrete owner for HidbusBase's event and ApplicationMemory references.
/// This stays in core to avoid a core -> hid_core -> core dependency cycle.
struct DeviceRuntime {
    system: SystemRef,
    context: Arc<StdMutex<ServiceContext>>,
    handle: u32,
    event: Arc<Event>,
}
impl DeviceRuntime {
    fn new(system: SystemRef, context: Arc<StdMutex<ServiceContext>>) -> Arc<Self> {
        let (handle, event) = {
            let mut owner = context.lock().unwrap();
            let handle = owner.create_event("hidbus:SendCommandAsyncEvent".into());
            let event = owner
                .get_event(handle)
                .expect("HID bus event allocation failed");
            (handle, event)
        };
        Arc::new(Self {
            system,
            context,
            handle,
            event,
        })
    }
}
impl HidbusRuntime for DeviceRuntime {
    fn signal_send_command_async_event(&self) {
        self.event.signal();
    }
    fn write_memory(&self, address: u64, bytes: &[u8]) {
        let memory = self
            .system
            .get()
            .memory_shared()
            .expect("HID bus application memory");
        memory.lock().unwrap().write_block(address, bytes);
    }
}
impl Drop for DeviceRuntime {
    fn drop(&mut self) {
        self.context.lock().unwrap().close_event(self.handle);
    }
}

#[derive(Default)]
struct DeviceSlot {
    is_device_initialized: bool,
    handle: BusHandle,
    device: Option<Box<dyn HidbusDevice>>,
    runtime: Option<Arc<DeviceRuntime>>,
}

/// Mutex-protected mutable members of Hidbus, shared with its timer callback.
/// The callback takes a Weak reference, never keeping the service alive.
struct HidbusState {
    system: SystemRef,
    context: Arc<StdMutex<ServiceContext>>,
    input: Arc<Mutex<EmulatedController>>,
    shared_memory: Arc<KSharedMemory>,
    is_hidbus_enabled: bool,
    status: HidbusStatusManager,
    devices: [DeviceSlot; MAX_NUMBER_OF_HANDLES],
}
impl HidbusState {
    fn get_device_index_from_handle(&self, handle: BusHandle) -> Option<usize> {
        self.devices
            .iter()
            .position(|slot| slot.handle.matches(handle))
    }
    fn get_bus_handle(&mut self, npad_id: u32, bus_type: u64) -> (bool, BusHandle) {
        if let Some(slot) = self.devices.iter().find(|slot| {
            slot.handle.is_valid()
                && u64::from(slot.handle.player_number()) == u64::from(npad_id)
                && slot.handle.bus_type_id() == bus_type as u8
        }) {
            return (true, slot.handle);
        }
        if let Some(index) = self.devices.iter().position(|slot| !slot.handle.is_valid()) {
            let handle = BusHandle(
                index as u64
                    | (index as u64) << 32
                    | (npad_id as u8 as u64) << 40
                    | (bus_type as u8 as u64) << 48
                    | 1 << 56,
            );
            self.devices[index].handle = handle;
            return (true, handle);
        }
        (false, self.devices[0].handle)
    }
    /// Upstream MakeDevice<T>; the selector replaces its two instantiations.
    fn make_device(&mut self, handle: BusHandle, is_ring: bool) {
        let Some(index) = self.get_device_index_from_handle(handle) else {
            return;
        };
        let runtime = DeviceRuntime::new(self.system.clone(), self.context.clone());
        let device: Box<dyn HidbusDevice> = if is_ring {
            Box::new(RingController::new(self.input.clone(), runtime.clone()))
        } else {
            Box::new(HidbusStubbed::new(runtime.clone()))
        };
        self.devices[index].device = Some(device);
        self.devices[index].runtime = Some(runtime);
    }
    fn initialize(&mut self, handle: BusHandle, enable_ring: bool) -> ResultCode {
        self.is_hidbus_enabled = true;
        let Some(index) = self.get_device_index_from_handle(handle) else {
            return RESULT_UNKNOWN;
        };
        let is_ring = handle.internal_index() == 0 && enable_ring;
        self.make_device(handle, is_ring);
        self.devices[index].is_device_initialized = true;
        if is_ring {
            self.devices[index]
                .device
                .as_mut()
                .unwrap()
                .activate_device();
        }
        let entry = &mut self.status.entries[self.devices[index].handle.internal_index()];
        entry.is_in_focus = 1;
        entry.is_connected = u8::from(is_ring);
        entry.is_connected_result = 0;
        entry.is_enabled = 0;
        entry.is_polling_mode = 0;
        self.publish_status();
        RESULT_SUCCESS
    }
    fn finalize(&mut self, handle: BusHandle) -> ResultCode {
        let Some(index) = self.get_device_index_from_handle(handle) else {
            return RESULT_UNKNOWN;
        };
        let slot = &mut self.devices[index];
        let Some(device) = slot.device.as_mut() else {
            return RESULT_UNKNOWN;
        };
        slot.is_device_initialized = false;
        device.deactivate_device();
        let entry = &mut self.status.entries[slot.handle.internal_index()];
        entry.is_in_focus = 1;
        entry.is_connected = 0;
        entry.is_connected_result = 0;
        entry.is_enabled = 0;
        entry.is_polling_mode = 0;
        self.publish_status();
        RESULT_SUCCESS
    }
    fn device_mut(&mut self, handle: BusHandle) -> Option<&mut Box<dyn HidbusDevice>> {
        let index = self.get_device_index_from_handle(handle)?;
        self.devices[index].device.as_mut()
    }
    fn publish_status(&self) {
        // Every byte is explicit and initialized, including reserved bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &self.status as *const _ as *const u8,
                self.shared_memory.get_pointer_mut(0),
                std::mem::size_of::<HidbusStatusManager>(),
            );
        }
    }
    fn update_hidbus(&mut self) {
        if !self.is_hidbus_enabled {
            return;
        }
        for slot in &mut self.devices {
            if !slot.is_device_initialized {
                continue;
            }
            let device = slot.device.as_mut().expect("initialized HID bus device");
            device.on_update();
            let index = slot.handle.internal_index();
            let entry = &mut self.status.entries[index];
            entry.is_polling_mode = u8::from(device.is_polling_mode());
            entry.polling_mode = device.get_polling_mode();
            entry.is_enabled = u8::from(device.is_enabled());
            // Eden currently copies entry zero to every slot by taking &status.
            // Publish the selected entry: each bus handle owns its own status.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    entry as *const _ as *const u8,
                    self.shared_memory.get_pointer_mut(index * 0x80),
                    0x80,
                );
            }
        }
    }
}

pub struct Hidbus {
    state: Arc<Mutex<HidbusState>>,
    shared_memory_id: u64,
    timing: Arc<CoreTiming>,
    update_event: Arc<Mutex<EventType>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl Hidbus {
    pub fn new(system: SystemRef) -> Self {
        let (shared_memory_id, shared_memory) = system
            .get()
            .kernel()
            .unwrap()
            .get_hid_bus_shared_mem()
            .expect("kernel HID bus shared memory");
        let input = system
            .get()
            .hid_core()
            .lock()
            .get_emulated_controller(NpadIdType::Player1);
        let state = Arc::new(Mutex::new(HidbusState {
            system: system.clone(),
            context: Arc::new(StdMutex::new(ServiceContext::new("hidbus".into()))),
            input,
            shared_memory,
            is_hidbus_enabled: false,
            status: HidbusStatusManager {
                entries: [HidbusStatusManagerEntry::default(); MAX_NUMBER_OF_HANDLES],
                padding: [0; 0x680],
            },
            devices: std::array::from_fn(|_| DeviceSlot::default()),
        }));
        Self::from_state(state, shared_memory_id, system.get().core_timing_shared())
    }

    // Mechanical constructor split so tests can supply kernel-owned backing
    // without booting an application. Production always constructs it above.
    fn from_state(
        state: Arc<Mutex<HidbusState>>,
        shared_memory_id: u64,
        timing: Arc<CoreTiming>,
    ) -> Self {
        let weak = Arc::downgrade(&state);
        let update_event = core_timing::create_event(
            "Hidbus::UpdateCallback".into(),
            Box::new(move |_, _| {
                if let Some(state) = weak.upgrade() {
                    state.lock().update_hidbus();
                }
                None
            }),
        );
        timing.schedule_looping_event(HIDBUS_UPDATE_NS, HIDBUS_UPDATE_NS, &update_event, false);
        Self {
            state,
            shared_memory_id,
            timing,
            update_event,
            handlers: build_handler_map(&[
                (1, Some(Self::get_bus_handle), "GetBusHandle"),
                (
                    2,
                    Some(Self::is_external_device_connected),
                    "IsExternalDeviceConnected",
                ),
                (3, Some(Self::initialize), "Initialize"),
                (4, Some(Self::finalize), "Finalize"),
                (
                    5,
                    Some(Self::enable_external_device),
                    "EnableExternalDevice",
                ),
                (6, Some(Self::get_external_device_id), "GetExternalDeviceId"),
                (7, Some(Self::send_command_async), "SendCommandAsync"),
                (
                    8,
                    Some(Self::get_send_command_asynce_result),
                    "GetSendCommandAsynceResult",
                ),
                (
                    9,
                    Some(Self::set_event_for_send_command_asyc_result),
                    "SetEventForSendCommandAsycResult",
                ),
                (
                    10,
                    Some(Self::get_shared_memory_handle),
                    "GetSharedMemoryHandle",
                ),
                (
                    11,
                    Some(Self::enable_joy_polling_receive_mode),
                    "EnableJoyPollingReceiveMode",
                ),
                (
                    12,
                    Some(Self::disable_joy_polling_receive_mode),
                    "DisableJoyPollingReceiveMode",
                ),
                (13, None, "GetPollingData"),
                (
                    14,
                    Some(Self::set_status_manager_type),
                    "SetStatusManagerType",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn get_bus_handle(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let npad = rp.pop_u32();
        rp.pop_u32(); // align BusType to eight bytes
        let bus = rp.pop_u64();
        let _aruid = rp.pop_u64();
        let (valid, handle) = service.state.lock().get_bus_handle(npad, bus);
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(u32::from(valid));
        rb.push_u32(0);
        rb.push_u64(handle.0);
    }
    fn initialize(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let enable = *common::settings::values()
            .enable_ring_controller
            .get_value();
        let result = service.state.lock().initialize(handle, enable);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn finalize(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let result = service.state.lock().finalize(handle);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }
    fn is_external_device_connected(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let value = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.is_device_activated());
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(if value.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
        rb.push_u32(u32::from(value.unwrap_or(false)));
    }
    fn enable_external_device(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        rp.pop_u32();
        let handle = BusHandle(rp.pop_u64());
        let result = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.enable(enabled));
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(if result.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
    }
    fn get_external_device_id(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let id = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.get_device_id());
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(if id.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
        rb.push_u32(id.unwrap_or(0) as u32);
    }
    fn send_command_async(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let data = ctx.read_buffer(0);
        let result = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.set_command(&data));
        // The device signals even unknown commands; the IPC itself succeeds.
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(if result.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
    }
    fn get_send_command_asynce_result(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let mut data = vec![0; ctx.get_write_buffer_size(0)];
        let size = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.get_reply(&mut data));
        // CMIF writes the full output span. Rust initializes its unused tail
        // rather than copying uninitialized temporary-buffer bytes.
        ctx.write_buffer(&data, 0);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(if size.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
        rb.push_u64(size.unwrap_or(0));
    }
    fn set_event_for_send_command_asyc_result(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let event = {
            let state = service.state.lock();
            state
                .get_device_index_from_handle(handle)
                .and_then(|index| state.devices[index].runtime.as_ref())
                .and_then(|runtime| runtime.event.readable_event())
        };
        let object = event.and_then(|event| ctx.register_readable_event_object(event));
        let mut rb = ResponseBuilder::new(ctx, 2, u32::from(object.is_some()), 0);
        rb.push_result(if object.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
        if let Some(object) = object {
            rb.push_copy_object_id(object);
        }
    }
    fn get_shared_memory_handle(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let backing = service.state.lock().shared_memory.clone();
        let object = ctx.owner_process_arc().map(|process| {
            process
                .lock()
                .unwrap()
                .register_shared_memory_object(service.shared_memory_id, backing);
            service.shared_memory_id
        });
        let mut rb = ResponseBuilder::new(ctx, 2, u32::from(object.is_some()), 0);
        rb.push_result(if object.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
        if let Some(object) = object {
            rb.push_copy_object_id(object);
        }
    }
    fn enable_joy_polling_receive_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let size = rp.pop_u32();
        let mode = JoyPollingMode(rp.pop_u32());
        let handle = BusHandle(rp.pop_u64());
        assert_eq!(size, 0x1000, "t_mem_size is not 0x1000 bytes");
        let memory = ctx.owner_process_arc().and_then(|process| {
            let process = process.lock().unwrap();
            let id = process.handle_table.get_object(ctx.get_copy_handle(0))?;
            process.get_transfer_memory_by_object_id(id)
        });
        let result = memory.and_then(|memory| {
            let memory = memory.lock().unwrap();
            assert_eq!(memory.get_size(), size as usize);
            let address = memory.get_source_address();
            drop(memory);
            service.state.lock().device_mut(handle).map(|device| {
                device.set_polling_mode(mode);
                device.set_transfer_memory_address(address);
            })
        });
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(if result.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
    }
    fn disable_joy_polling_receive_mode(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = BusHandle(RequestParser::new(ctx).pop_u64());
        let result = service
            .state
            .lock()
            .device_mut(handle)
            .map(|device| device.disable_polling_mode());
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(if result.is_some() {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        });
    }
    fn set_status_manager_type(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mode = RequestParser::new(ctx).pop_u32();
        log::warn!("(STUBBED) SetStatusManagerType manager_type={mode}");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
}
impl Drop for Hidbus {
    fn drop(&mut self) {
        // Never hold the state mutex while waiting for an in-flight callback.
        self.timing
            .unschedule_event(&self.update_event, UnscheduleEventType::Wait);
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
    use super::*;
    use crate::device_memory::{dram_memory_map, DeviceMemory};
    use crate::hle::kernel::k_memory_manager::Pool;
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    struct Fixture {
        service: Hidbus,
        // Backing must outlive the service and all copied kernel objects.
        _kernel: ScopedKernelForTest,
        _memory: Box<DeviceMemory>,
    }
    impl Fixture {
        fn new() -> Self {
            let memory = Box::new(DeviceMemory::with_size(0x2000));
            let mut kernel = ScopedKernelForTest::new();
            kernel.memory_manager_mut().initialize_pool(
                Pool::SECURE,
                dram_memory_map::BASE,
                0x2000,
            );
            assert!(kernel
                .kernel_mut()
                .initialize_hidbus_shared_memory(&memory)
                .is_success());
            let (id, shared_memory) = kernel.kernel_mut().get_hid_bus_shared_mem().unwrap();
            let state = Arc::new(Mutex::new(HidbusState {
                system: SystemRef::null(),
                context: Arc::new(StdMutex::new(ServiceContext::new("hidbus-test".into()))),
                input: Arc::new(Mutex::new(EmulatedController::new(NpadIdType::Player1))),
                shared_memory,
                is_hidbus_enabled: false,
                status: HidbusStatusManager {
                    entries: [HidbusStatusManagerEntry::default(); MAX_NUMBER_OF_HANDLES],
                    padding: [0; 0x680],
                },
                devices: std::array::from_fn(|_| DeviceSlot::default()),
            }));
            Self {
                service: Hidbus::from_state(state, id, Arc::new(CoreTiming::new())),
                _kernel: kernel,
                _memory: memory,
            }
        }
    }

    #[test]
    fn handles_reuse_named_fields_and_report_exhaustion() {
        let fixture = Fixture::new();
        let mut state = fixture.service.state.lock();
        for index in 0..MAX_NUMBER_OF_HANDLES {
            let (valid, handle) = state.get_bus_handle(index as u32, 1);
            assert!(valid);
            assert_eq!(handle.internal_index(), index);
            assert_eq!(handle.player_number(), index as u8);
            assert_eq!(handle.bus_type_id(), 1);
            assert_eq!(state.get_bus_handle(index as u32, 1), (true, handle));
            assert_eq!(
                state.get_device_index_from_handle(BusHandle(handle.0 | 0xFE00_0000_0000_0000)),
                Some(index)
            );
            assert!(state
                .get_device_index_from_handle(BusHandle(handle.0 ^ (1 << 31)))
                .is_none());
        }
        assert_eq!(
            state.get_bus_handle(99, 1),
            (false, state.devices[0].handle)
        );
    }

    #[test]
    fn ring_setting_lifecycle_native_event_and_per_handle_status() {
        let fixture = Fixture::new();
        let mut state = fixture.service.state.lock();
        let (_, first) = state.get_bus_handle(0, 1);
        let (_, second) = state.get_bus_handle(1, 1);
        assert_eq!(state.initialize(first, false), RESULT_SUCCESS);
        assert_eq!(state.device_mut(first).unwrap().get_device_id(), 0xFF);
        assert!(!state.device_mut(first).unwrap().is_device_activated());
        let old_event = state.devices[0].runtime.as_ref().unwrap().handle;
        assert_eq!(state.initialize(first, true), RESULT_SUCCESS);
        assert!(state.context.lock().unwrap().get_event(old_event).is_none());
        assert_eq!(state.device_mut(first).unwrap().get_device_id(), 0x20);
        assert!(state.device_mut(first).unwrap().is_device_activated());
        assert_eq!(state.initialize(second, true), RESULT_SUCCESS);
        assert_eq!(state.device_mut(second).unwrap().get_device_id(), 0xFF);

        let event = state.devices[0].runtime.as_ref().unwrap().event.clone();
        let readable = event.readable_event().expect("native readable event");
        assert!(!event.is_signaled());
        assert!(state
            .device_mut(first)
            .unwrap()
            .set_command(&0x0002_0000u32.to_le_bytes()));
        assert!(event.is_signaled());
        assert!(Arc::ptr_eq(&readable, &event.readable_event().unwrap()));
        let mut reply = [0; 8];
        assert_eq!(state.device_mut(first).unwrap().get_reply(&mut reply), 8);
        assert_eq!(reply, [0, 0, 0, 0, 0, 0x2C, 0, 0]);

        state.device_mut(first).unwrap().enable(true);
        state
            .device_mut(first)
            .unwrap()
            .set_polling_mode(JoyPollingMode(0xFFFF_FFFE));
        state.update_hidbus(); // No transfer address: no application-memory access.
        let bytes =
            unsafe { std::slice::from_raw_parts(state.shared_memory.get_pointer(0), 0x1000) };
        assert_eq!(&bytes[8..12], &[1, 1, 1, 0]);
        assert_eq!(&bytes[12..16], &0xFFFF_FFFEu32.to_ne_bytes());
        assert_eq!(&bytes[0x80 + 8..0x80 + 12], &[0, 1, 0, 0]);
        assert!(bytes[0x980..].iter().all(|byte| *byte == 0));
        assert_eq!(state.finalize(first), RESULT_SUCCESS);
        assert!(!state.device_mut(first).unwrap().is_device_activated());
        assert_eq!(state.get_bus_handle(0, 1), (true, first));
        assert_eq!(state.initialize(first, true), RESULT_SUCCESS);
        assert!(state.device_mut(first).unwrap().is_device_activated());
    }

    #[test]
    fn get_bus_handle_ipc_uses_eight_byte_alignment() {
        let fixture = Fixture::new();
        let mut ctx = HLERequestContext::new();
        // Parser skips the command id. Deliberately poison the alignment padding.
        ctx.cmd_buf[2..8].copy_from_slice(&[3, 0xDEAD_BEEF, 1, 0, 42, 0]);
        Hidbus::get_bus_handle(&fixture.service, &mut ctx);
        let offset = ctx.data_payload_offset as usize;
        assert_eq!(&ctx.cmd_buf[offset..offset + 4], &[0, 0, 1, 0]);
        let raw = u64::from(ctx.cmd_buf[offset + 4]) | u64::from(ctx.cmd_buf[offset + 5]) << 32;
        assert_eq!(BusHandle(raw).player_number(), 3);
        assert_eq!(BusHandle(raw).bus_type_id(), 1);
        assert!(fixture.service.handlers[&13].handler_callback.is_none());
    }

    #[test]
    fn timer_callback_does_not_own_service_state() {
        let fixture = Fixture::new();
        let weak = Arc::downgrade(&fixture.service.state);
        let event = fixture.service.update_event.clone();
        let timing = fixture.service.timing.clone();
        assert_eq!(timing.advance(), Some(HIDBUS_UPDATE_NS.as_nanos() as i64));
        drop(fixture);
        assert!(weak.upgrade().is_none());
        // Retain the EventType: a missing UnscheduleEvent cannot be hidden by
        // an expired weak event reference in CoreTiming's queue.
        assert_eq!(timing.advance(), None);
        drop(event);
    }

    #[test]
    fn copy_handles_export_persistent_kernel_objects_to_the_client() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;

        let fixture = Fixture::new();
        let handle = {
            let mut state = fixture.service.state.lock();
            let (_, handle) = state.get_bus_handle(0, 1);
            assert_eq!(state.initialize(handle, true), RESULT_SUCCESS);
            handle
        };
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().process_id = 0x1234;
        let mut thread = KThread::new();
        thread.parent = Some(Arc::downgrade(&process));
        let thread = Arc::new(KThreadLock::new(thread));
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            ctx.cmd_buf[2] = handle.0 as u32;
            ctx.cmd_buf[3] = (handle.0 >> 32) as u32;
            Hidbus::set_event_for_send_command_asyc_result(&fixture.service, &mut ctx);
            assert_eq!(ctx.cmd_buf[ctx.data_payload_offset as usize], 0);
            let KAutoObjectRef::ObjectId(id) = ctx.outgoing_copy_objects[0] else {
                panic!("expected event object id");
            };
            assert!(process
                .lock()
                .unwrap()
                .get_readable_event_by_object_id(id)
                .is_some());
            ids.push(id);
        }
        assert_eq!(ids[0], ids[1]);
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        Hidbus::get_shared_memory_handle(&fixture.service, &mut ctx);
        let KAutoObjectRef::ObjectId(id) = ctx.outgoing_copy_objects[0] else {
            panic!("expected shared-memory object id");
        };
        assert_eq!(id, fixture.service.shared_memory_id);
        let actual = process
            .lock()
            .unwrap()
            .get_shared_memory_by_object_id(id)
            .unwrap();
        assert!(Arc::ptr_eq(
            &actual,
            &fixture.service.state.lock().shared_memory
        ));
    }

    #[test]
    fn timer_writes_application_memory_and_stops_after_finalize() {
        use crate::core::System;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::memory::memory::Memory;
        use common::page_table::{PageTable, PageType};

        let device_memory = Box::new(DeviceMemory::with_size(0x4000));
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(32, 12);
        page_table.map_pages(
            1,
            1,
            0x1000,
            PageType::Memory,
            device_memory.buffer.backing_base_pointer() as usize + 0x1000,
        );
        // Stable boxed backing/page table outlive all accesses through Memory.
        let memory = Arc::new(StdMutex::new(unsafe {
            Memory::new(
                SystemRef::null(),
                device_memory.as_ref(),
                &device_memory.buffer,
            )
        }));
        memory
            .lock()
            .unwrap()
            .set_current_page_table(page_table.as_mut(), true);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        process.lock().unwrap().memory = Some(memory.clone());
        let mut system = Box::new(System::new());
        system.set_current_process_arc(process);
        let fixture = Fixture::new();
        let handle = {
            let mut state = fixture.service.state.lock();
            state.system = SystemRef::from_ref(&system);
            let (_, handle) = state.get_bus_handle(0, 1);
            assert_eq!(state.initialize(handle, true), RESULT_SUCCESS);
            let device = state.device_mut(handle).unwrap();
            device.enable(true);
            device.set_polling_mode(JoyPollingMode::SixAxisSensorEnable);
            device.set_transfer_memory_address(0x1000);
            handle
        };
        let tick = || {
            fixture.service.timing.schedule_event(
                Duration::ZERO,
                &fixture.service.update_event,
                false,
            );
            fixture.service.timing.advance();
        };
        let read = || unsafe {
            std::slice::from_raw_parts(
                device_memory.buffer.backing_base_pointer().add(0x1000),
                0x190,
            )
            .to_vec()
        };
        tick();
        let first = read();
        assert_eq!(u64::from_le_bytes(first[0x20..0x28].try_into().unwrap()), 1);
        assert_eq!(
            u64::from_le_bytes(first[0x28..0x30].try_into().unwrap()),
            10
        );
        // Entry one starts at 0x50: both sampling counters must match.
        assert_eq!(u64::from_le_bytes(first[0x50..0x58].try_into().unwrap()), 1);
        assert_eq!(first[0x60], 8);
        assert_eq!(u64::from_le_bytes(first[0x68..0x70].try_into().unwrap()), 1);
        assert_eq!(
            fixture.service.state.lock().finalize(handle),
            RESULT_SUCCESS
        );
        tick();
        assert_eq!(read(), first);
        {
            let mut state = fixture.service.state.lock();
            assert_eq!(state.initialize(handle, true), RESULT_SUCCESS);
            let device = state.device_mut(handle).unwrap();
            device.enable(true);
            device.set_polling_mode(JoyPollingMode::SixAxisSensorEnable);
            device.set_transfer_memory_address(0x1000);
        }
        tick();
        assert_eq!(read(), first); // Newly initialized device starts again at sample one.
        tick();
        assert_eq!(
            u64::from_le_bytes(read()[0x20..0x28].try_into().unwrap()),
            2
        );
    }
}
