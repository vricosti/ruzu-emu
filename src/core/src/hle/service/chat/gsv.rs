// SPDX-License-Identifier: GPL-3.0-or-later
//! SDK-derived chat graphics transport. No Eden-NX2 counterpart exists.
//! SDK +0x6c1c90: GSV 1201 initialize, 1202 moved-session acquisition,
//! 1203 finalize. SDK +0x6c1a50: session command 1 (wire 17) returns a maximum notification
//! index (<=1023), command 11 (wire 27) returns a moved readable event. +0x6c2040 registers
//! a virtual display by eight-byte name and client slot (command 103, wire 119); +0x6c1000
//! unregisters it by name (command 102, wire 118). No camera frames are fabricated.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{ResponseBuilder, RESULT_NOT_SUPPORTED};
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::nx2_gsv::{
    create_raw_session, drop_native_image_mapping, nx2_calculate_image_layout,
    take_native_image_mapping, tipc_reply_words, Nx2NativeImage, Nx2NativeObjects,
};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

#[derive(Default)]
pub struct ChatGraphicsService {
    initializations: Mutex<u32>,
    image_pools: Mutex<BTreeMap<u32, u32>>,
}

impl ChatGraphicsService {
    // SDK MakeImagePreference(+0x6c0e90) only accepts encoding zero and emits
    // preference kind1, NOT decoder pixel format1 (NV12). MKW 1.8.0 main
    // +0xd322fc selects NVN format0x38 before importing these images and
    // +0xd32344 constructs a texture. Use the native RGBA layout, matching
    // Eglantine ae9d3eb8 nx2_gsv.h CalculateChatImagePoolSize. These images
    // remain in the shared native registry, outside decoder-image ownership.
    fn image_pool_layout(words: &[u32]) -> Option<(u64, u64, u32)> {
        let preference: &[u32; 16] = words[2..18].try_into().ok()?;
        let count = words[18];
        if preference[1] != 1 || count == 0 || count > 256 {
            return None;
        }
        let layout = nx2_calculate_image_layout(preference);
        Some((layout.size, layout.alignment, count))
    }

    pub fn calculate_image_pool_buffer(&self, ctx: &mut HLERequestContext) {
        let Some((stride, alignment, count)) = Self::image_pool_layout(ctx.command_buffer()) else {
            return fail(ctx);
        };
        let size = stride * count as u64;
        tipc_reply_words(
            ctx,
            &[
                size as u32,
                (size >> 32) as u32,
                alignment as u32,
                (alignment >> 32) as u32,
            ],
        );
    }

    pub fn create_image_pool(
        &self,
        ctx: &mut HLERequestContext,
        objects: &Mutex<Nx2NativeObjects>,
    ) {
        let Some((stride, _, count)) = Self::image_pool_layout(ctx.command_buffer()) else {
            return fail(ctx);
        };
        let words = ctx.command_buffer();
        let preference: [u32; 16] = words[2..18].try_into().unwrap();
        let memory_id = words[19];
        let mut pools = self.image_pools.lock().unwrap();
        let mut objects = objects.lock().unwrap();
        if !objects
            .memories
            .get(&memory_id)
            .is_some_and(|memory| memory.size >= stride * count as u64 && memory.address != 0)
        {
            return fail(ctx);
        }
        let first = objects.next_image;
        let Some(next) = first.checked_add(count) else {
            return fail(ctx);
        };
        objects.next_image = next;
        for index in 0..count {
            objects.images.insert(
                first + index,
                Nx2NativeImage {
                    memory: memory_id,
                    preference,
                    size: stride,
                    offset: stride * index as u64,
                    decoder: false,
                    window_pool: false,
                    ..Default::default()
                },
            );
        }
        pools.insert(first, count);
        log::info!("chat:gsv image pool first={first} count={count} stride={stride}");
        tipc_reply_words(ctx, &[first]);
    }

    pub fn destroy_image_pool(
        &self,
        ctx: &mut HLERequestContext,
        objects: &Mutex<Nx2NativeObjects>,
    ) {
        let first = ctx.command_buffer()[2];
        let Some(count) = self.image_pools.lock().unwrap().remove(&first) else {
            return fail(ctx);
        };
        let mut objects = objects.lock().unwrap();
        let mut mappings = Vec::new();
        for index in 0..count {
            if let Some(mut image) = objects.images.remove(&(first + index)) {
                mappings.push(take_native_image_mapping(&mut image, &objects.memories));
            }
        }
        drop(objects);
        for mapping in mappings {
            drop_native_image_mapping(mapping);
        }
        tipc_reply_words(ctx, &[]);
    }

    pub fn initialize(&self, ctx: &mut HLERequestContext) {
        let mut count = self.initializations.lock().unwrap();
        let Some(next) = count.checked_add(1) else {
            return fail(ctx);
        };
        *count = next;
        log::info!("chat:gsv initialize, references={next}");
        tipc_reply_words(ctx, &[]);
    }

    pub fn open_session(&self, ctx: &mut HLERequestContext) {
        if *self.initializations.lock().unwrap() == 0 {
            return fail(ctx);
        }
        let object = create_raw_session(ctx, Arc::new(ChatGraphicsSession::new()));
        // SDK +0x6c1e94 reads TLS[3] and expects one MOVE (special header 0x20).
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_move_object_id(object);
    }

    pub fn finalize(&self, ctx: &mut HLERequestContext) {
        let mut count = self.initializations.lock().unwrap();
        let Some(next) = count.checked_sub(1) else {
            return fail(ctx);
        };
        *count = next;
        tipc_reply_words(ctx, &[]);
    }
}

fn fail(ctx: &mut HLERequestContext) {
    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
    rb.push_result(RESULT_NOT_SUPPORTED);
}

struct SessionState {
    context: ServiceContext,
    events: BTreeMap<u32, u32>,
    displays: BTreeMap<u64, u32>,
    metadata: BTreeMap<u64, MetadataPool>,
}

struct MetadataPool {
    notification_index: u32,
    // None means no received frame, not a fictitious all-zero metadata record.
    entries: Vec<Option<u64>>,
}

struct ChatGraphicsSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    state: Mutex<SessionState>,
}

impl ChatGraphicsSession {
    // The SDK allocates a fixed 1024-bit notification map and checks this bound
    // at +0x6c1b40. We expose that capacity; original hardware capacity unknown.
    const MAX_NOTIFICATION_INDEX: u32 = 1023;

    fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
            handlers_tipc: build_handler_map(&[
                (
                    1,
                    Some(Self::get_max_notification_index),
                    "GetMaxNotificationIndex",
                ),
                (
                    11,
                    Some(Self::get_notification_event),
                    "GetNotificationEvent",
                ),
                (
                    102,
                    Some(Self::close_virtual_display),
                    "CloseVirtualDisplay",
                ),
                (103, Some(Self::open_virtual_display), "OpenVirtualDisplay"),
                (121, Some(Self::create_metadata_pool), "CreateMetadataPool"),
                (
                    122,
                    Some(Self::destroy_metadata_pool),
                    "DestroyMetadataPool",
                ),
                (231, Some(Self::get_metadata), "GetMetadata"),
            ]),
            state: Mutex::new(SessionState {
                context: ServiceContext::new("chat:gsv".to_owned()),
                events: BTreeMap::new(),
                displays: BTreeMap::new(),
                metadata: BTreeMap::new(),
            }),
        }
    }

    fn get_max_notification_index(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        tipc_reply_words(ctx, &[Self::MAX_NOTIFICATION_INDEX]);
    }

    // SDK0x6c2500: opaque client identity u64, entry count u64, slot u32.
    // No guest pointer is dereferenced: GetMetadata transports records by IPC.
    fn create_metadata_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let w = ctx.command_buffer();
        let identity = w[2] as u64 | ((w[3] as u64) << 32);
        let count = w[4] as u64 | ((w[5] as u64) << 32);
        let slot = w[6];
        let mut state = this.state.lock().unwrap();
        if identity == 0
            || count == 0
            || count > 256
            || slot > Self::MAX_NOTIFICATION_INDEX
            || state.metadata.contains_key(&identity)
            || state.displays.values().any(|&s| s == slot)
            || state
                .metadata
                .values()
                .any(|pool| pool.notification_index == slot)
        {
            return fail(ctx);
        }
        state.metadata.insert(
            identity,
            MetadataPool {
                notification_index: slot,
                entries: vec![None; count as usize],
            },
        );
        log::info!("chat:gsv metadata pool {identity:#x}, entries={count}, slot={slot}");
        tipc_reply_words(ctx, &[]);
    }

    fn destroy_metadata_pool(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let w = ctx.command_buffer();
        let identity = w[2] as u64 | ((w[3] as u64) << 32);
        if this
            .state
            .lock()
            .unwrap()
            .metadata
            .remove(&identity)
            .is_none()
        {
            return fail(ctx);
        }
        tipc_reply_words(ctx, &[]);
    }

    fn get_metadata(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let w = ctx.command_buffer();
        let identity = w[2] as u64 | ((w[3] as u64) << 32);
        let index = (w[4] & 255) as usize;
        let value = this
            .state
            .lock()
            .unwrap()
            .metadata
            .get(&identity)
            .and_then(|pool| pool.entries.get(index))
            .copied()
            .flatten();
        let Some(value) = value else { return fail(ctx) };
        tipc_reply_words(ctx, &[value as u32, (value >> 32) as u32]);
    }

    fn get_notification_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let index = ctx.command_buffer()[2];
        if index > Self::MAX_NOTIFICATION_INDEX {
            return fail(ctx);
        }
        let event = {
            let mut state = this.state.lock().unwrap();
            let handle = match state.events.get(&index) {
                Some(&handle) => handle,
                None => {
                    let handle = state
                        .context
                        .create_event(format!("ChatNotification{index}"));
                    if handle == 0 {
                        return fail(ctx);
                    }
                    state.events.insert(index, handle);
                    handle
                }
            };
            state.context.get_event(handle).unwrap()
        };
        let object = event.copy_object_id(ctx).expect("chat notification owner");
        log::info!("chat:gsv notification event {index}");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_move_object_id(object);
    }

    fn open_virtual_display(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let words = ctx.command_buffer();
        let name = words[2] as u64 | ((words[3] as u64) << 32);
        let slot = words[4];
        let mut state = this.state.lock().unwrap();
        // A virtual display is a named, inactive stream until a chat producer
        // exists. Retain its registration; no image or completion is invented.
        if slot > Self::MAX_NOTIFICATION_INDEX
            || state.displays.contains_key(&name)
            || state.displays.values().any(|&old_slot| old_slot == slot)
            || state
                .metadata
                .values()
                .any(|pool| pool.notification_index == slot)
        {
            return fail(ctx);
        }
        state.displays.insert(name, slot);
        log::info!(
            "chat:gsv open virtual display {:?}, slot={slot}",
            name.to_le_bytes()
        );
        tipc_reply_words(ctx, &[]);
    }

    fn close_virtual_display(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = this.as_any().downcast_ref::<Self>().unwrap();
        let words = ctx.command_buffer();
        let name = words[2] as u64 | ((words[3] as u64) << 32);
        if this.state.lock().unwrap().displays.remove(&name).is_none() {
            return fail(ctx);
        }
        tipc_reply_words(ctx, &[]);
    }
}

impl SessionRequestHandler for ChatGraphicsSession {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for ChatGraphicsSession {
    fn get_service_name(&self) -> &str {
        "chat:gsv"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
    fn report_unimplemented_function(&self, ctx: &mut HLERequestContext, _: Option<&FunctionInfo>) {
        log::error!(
            "Unsupported chat:gsv command {} words={:x?}",
            ctx.get_command(),
            &ctx.command_buffer()[..12]
        );
        fail(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    #[test]
    fn image_pools_reserve_all_images_and_reject_short_backing_memory() {
        use crate::hle::service::nx2_gsv::Nx2NativeMemory;
        let service = ChatGraphicsService::default();
        let objects = Mutex::new(Nx2NativeObjects::default());
        let mut request = [0u32; 20];
        request[0] = 1291 + 16;
        request[1] = 17;
        request[3] = 1; // SDK encoding zero -> native image preference kind1.
        request[4] = 383 | (215 << 16); // Runtime's 384x216 camera image.
        request[18] = 3;
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&request);
        service.calculate_image_pool_buffer(&mut ctx);
        assert_eq!(&ctx.command_buffer()[2..7], &[0, 1179648, 0, 8192, 0]);
        objects.lock().unwrap().memories.insert(
            7,
            Nx2NativeMemory {
                address: 0x100000,
                // Previously allocated NV12 pool: insufficient for three RGBA images.
                size: 442368,
            },
        );
        request[0] = 1211 + 16;
        request[1] = 18;
        request[19] = 7;
        ctx.populate_from_incoming_command_buffer(&request);
        service.create_image_pool(&mut ctx, &objects);
        assert_eq!(
            ctx.command_buffer()[2],
            RESULT_NOT_SUPPORTED.get_inner_value()
        );
        assert!(objects.lock().unwrap().images.is_empty());
        objects.lock().unwrap().memories.get_mut(&7).unwrap().size = 1179648;
        ctx.populate_from_incoming_command_buffer(&request);
        service.create_image_pool(&mut ctx, &objects);
        assert_eq!(&ctx.command_buffer()[2..4], &[0, 1]);
        for (index, image) in objects.lock().unwrap().images.values().enumerate() {
            assert_eq!(
                (image.memory, image.size, image.offset),
                (7, 393216, index as u64 * 393216)
            );
            assert!(!image.decoder);
        }
        assert_eq!(objects.lock().unwrap().images.len(), 3);
        ctx.populate_from_incoming_command_buffer(&[1212 + 16, 1, 1]);
        service.destroy_image_pool(&mut ctx, &objects);
        assert!(objects.lock().unwrap().images.is_empty());
        assert!(objects.lock().unwrap().memories.contains_key(&7));
    }

    #[test]
    fn metadata_pool_uses_full_identity_and_never_reports_an_unreceived_frame() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = ChatGraphicsSession::new();
        let mut ctx = HLERequestContext::new();
        ctx.populate_from_incoming_command_buffer(&[0x89, 5, 0xffabfb00, 0x21, 3, 0, 6]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(ctx.command_buffer()[2], 0);
        let identity = 0x21ffabfb00;
        assert_eq!(
            service.state.lock().unwrap().metadata[&identity]
                .entries
                .len(),
            3
        );
        ctx.populate_from_incoming_command_buffer(&[0xf7, 3, 0xffabfb00, 0x21, 0]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(
            ctx.command_buffer()[2],
            RESULT_NOT_SUPPORTED.get_inner_value()
        );
        ctx.populate_from_incoming_command_buffer(&[0x89, 5, 1, 0, 3, 1, 7]);
        service.handle_sync_request(&mut ctx);
        assert_eq!(
            ctx.command_buffer()[2],
            RESULT_NOT_SUPPORTED.get_inner_value()
        );
        assert_eq!(service.state.lock().unwrap().metadata.len(), 1);
        ctx.populate_from_incoming_command_buffer(&[0x8a, 2, 0xffabfb00, 0x21]);
        service.handle_sync_request(&mut ctx);
        assert!(service.state.lock().unwrap().metadata.is_empty());
    }

    #[test]
    fn sdk_wire_requests_return_moved_session_and_readable_event() {
        use crate::core::SystemRef;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_resource_limit::{KResourceLimit, LimitableResource};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::{KAutoObjectRef, SessionRequestManager};
        use crate::hle::service::server_manager::ServerManager;
        let _scope = ScopedKernelForTest::without_kernel();
        let limit = Arc::new(KResourceLimit::new());
        limit
            .set_limit_value(LimitableResource::SessionCountMax, 2)
            .unwrap();
        let mut process = KProcess::new();
        process.resource_limit = Some(limit.clone());
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let server = ServerManager::new_shared(SystemRef::null());
        let (queue, wakeup) = {
            let guard = server.lock().unwrap();
            (guard.pending_registrations_arc(), guard.wakeup_event_arc())
        };
        let manager = Arc::new(Mutex::new(
            SessionRequestManager::new_with_server_manager_full(
                server.clone(),
                queue.clone(),
                wakeup,
            ),
        ));
        let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
        ctx.set_session_request_manager(manager);
        let service = ChatGraphicsService::default();
        ctx.populate_from_incoming_command_buffer(&[0x4c1, 0]);
        service.initialize(&mut ctx);
        assert_eq!(&ctx.command_buffer()[..3], &[0x4c1, 1, 0]);
        ctx.populate_from_incoming_command_buffer(&[0x4c2, 0]);
        service.open_session(&mut ctx);
        assert_eq!(&ctx.command_buffer()[..3], &[0x4c2, 0x80000001, 0x20]);
        assert!(ctx.outgoing_copy_objects.is_empty());
        let KAutoObjectRef::ObjectId(session_id) = ctx.outgoing_move_objects[0] else {
            panic!("session identity")
        };
        assert!(process
            .lock()
            .unwrap()
            .client_session_objects
            .contains_key(&session_id));
        let child_manager = queue.lock().unwrap()[0].1.clone();
        let child = child_manager
            .lock()
            .unwrap()
            .session_handler()
            .unwrap()
            .clone();
        let mut request = HLERequestContext::new_with_thread(thread, 0);
        request.populate_from_incoming_command_buffer(&[0x11, 0]);
        child.handle_sync_request(&mut request);
        assert_eq!(&request.command_buffer()[..4], &[0x11, 2, 0, 1023]);
        // The transport encodes command+16, not the command number itself.
        // Both replies must transfer real objects in the MOVE array.
        let _kernel = ScopedKernelForTest::new();
        request.populate_from_incoming_command_buffer(&[0x1b, 1, 1023]);
        child.handle_sync_request(&mut request);
        assert_eq!(&request.command_buffer()[..3], &[0x1b, 0x80000001, 0x20]);
        assert!(request.outgoing_copy_objects.is_empty());
        let KAutoObjectRef::ObjectId(event_id) = request.outgoing_move_objects[0] else {
            panic!("event identity")
        };
        assert!(!process
            .lock()
            .unwrap()
            .get_readable_event_by_object_id(event_id)
            .unwrap()
            .lock()
            .unwrap()
            .is_signaled());
        request.populate_from_incoming_command_buffer(&[0x77, 3, 0x74736574, 0, 0]);
        child.handle_sync_request(&mut request);
        assert_eq!(&request.command_buffer()[..3], &[0x77, 1, 0]);
        request.populate_from_incoming_command_buffer(&[0x76, 2, 0x74736574, 0]);
        child.handle_sync_request(&mut request);
        assert_eq!(&request.command_buffer()[..3], &[0x76, 1, 0]);
        process
            .lock()
            .unwrap()
            .unregister_session_object_by_object_id(session_id);
        assert_eq!(
            limit.get_current_value(LimitableResource::SessionCountMax),
            0
        );
    }

    #[test]
    fn virtual_display_lifetime_validates_slots_and_keeps_state() {
        let _scope = ScopedKernelForTest::without_kernel();
        let service = ChatGraphicsSession::new();
        let mut ctx = HLERequestContext::new();
        for (name, slot) in [(1u64, 0u32), (2, 1), (3, 1024), (4, 1), (1, 2)] {
            ctx.command_buffer_mut()[2..5].copy_from_slice(&[
                name as u32,
                (name >> 32) as u32,
                slot,
            ]);
            ChatGraphicsSession::open_virtual_display(&service, &mut ctx);
        }
        assert_eq!(
            service.state.lock().unwrap().displays,
            BTreeMap::from([(1, 0), (2, 1)])
        );
        ctx.command_buffer_mut()[2..4].copy_from_slice(&[1, 0]);
        ChatGraphicsSession::close_virtual_display(&service, &mut ctx);
        assert_eq!(
            service.state.lock().unwrap().displays,
            BTreeMap::from([(2, 1)])
        );
    }

    #[test]
    fn graphics_initialization_is_balanced_and_cannot_underflow() {
        let service = ChatGraphicsService::default();
        let mut ctx = HLERequestContext::new();
        service.finalize(&mut ctx);
        assert_eq!(*service.initializations.lock().unwrap(), 0);
        assert_eq!(
            ctx.command_buffer()[6],
            RESULT_NOT_SUPPORTED.get_inner_value()
        );
        service.initialize(&mut ctx);
        service.initialize(&mut ctx);
        service.finalize(&mut ctx);
        assert_eq!(*service.initializations.lock().unwrap(), 1);
    }
}
