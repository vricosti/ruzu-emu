// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii core/hle/service/audio/nx2_audio_device_manager.h (header-only).
use super::{
    errors::*,
    nx2_sysmix::{audio_result, SysmixOutput},
};
use crate::core::SystemRef;
use crate::core_timing::{create_event, EventType, UnscheduleEventType};
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::{
    hle_ipc::{HLERequestContext, SessionRequestHandler},
    ipc_helpers::{RequestParser, ResponseBuilder},
    kernel_helpers::ServiceContext,
    os::event::Event,
    service::{build_handler_map, FunctionInfo, ServiceFramework},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct Nx2SysmixStreams {
    state: Mutex<StreamsState>,
    system: SystemRef,
    timer: Arc<parking_lot::Mutex<EventType>>,
}
struct StreamsState {
    streams: BTreeMap<u32, Arc<SysmixOutput>>,
    next_id: u32,
}
impl Nx2SysmixStreams {
    pub fn new(system: SystemRef) -> Arc<Self> {
        let streams = Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let weak = weak.clone();
            let timer = create_event(
                "sysmix output".into(),
                Box::new(move |_, late| {
                    if let Some(streams) = weak.upgrade() {
                        if let Ok(state) = streams.state.try_lock() {
                            for stream in state.streams.values() {
                                stream.pump();
                            }
                        }
                    }
                    Some(Duration::from_nanos(
                        5_000_000 * (late.as_nanos() as u64 / 5_000_000 + 1),
                    ))
                }),
            );
            Self {
                state: Mutex::new(StreamsState {
                    streams: BTreeMap::new(),
                    next_id: 1,
                }),
                system,
                timer,
            }
        });
        system.get().core_timing().schedule_looping_event(
            Duration::from_millis(5),
            Duration::from_millis(5),
            &streams.timer,
            false,
        );
        streams
    }
    fn set_engaged(&self, id: u32, engaged: bool, process: Option<Arc<ProcessLock>>) -> ResultCode {
        let stream = self.state.lock().unwrap().streams.get(&id).cloned();
        let Some(stream) = stream else {
            return audio_result(RESULT_NOT_FOUND);
        };
        if !process.as_ref().is_some_and(|p| stream.is_owned_by(p)) {
            return audio_result(RESULT_INVALID_HANDLE);
        }
        stream.set_engaged(engaged)
    }
}
impl Drop for Nx2SysmixStreams {
    fn drop(&mut self) {
        self.system
            .get()
            .core_timing()
            .unschedule_event(&self.timer, UnscheduleEventType::NoWait);
    }
}
// Mechanical framework glue only; commands stay owned by their upstream classes.
macro_rules! service_impl {
    ($ty:ty, $name:literal) => {
        impl $ty {
            fn as_self(this: &dyn ServiceFramework) -> &Self {
                unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
            }
        }
        impl SessionRequestHandler for $ty {
            fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
                self.handle_sync_request_impl(ctx)
            }
        }
        impl ServiceFramework for $ty {
            fn get_service_name(&self) -> &str {
                $name
            }
            fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
                &self.handlers
            }
            fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
                &self.handlers_tipc
            }
        }
    };
}
fn reply(ctx: &mut HLERequestContext, result: ResultCode) {
    ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
}
fn copy_event(ctx: &mut HLERequestContext, event: Option<Arc<Event>>, result: ResultCode) {
    let object = if result == RESULT_SUCCESS {
        event.and_then(|event| event.copy_object_id(ctx))
    } else {
        None
    };
    let result = if result == RESULT_SUCCESS && object.is_none() {
        audio_result(RESULT_OUT_OF_SESSIONS)
    } else {
        result
    };
    let mut rb = ResponseBuilder::new(ctx, 2, object.is_some() as u32, 0);
    rb.push_result(result);
    if let Some(object) = object {
        rb.push_copy_object_id(object);
    }
}
pub struct INx2AudioVirtualDeviceManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
    context: ServiceContext,
    state_event: u32,
    streams: Arc<Nx2SysmixStreams>,
}
service_impl!(INx2AudioVirtualDeviceManager, "nx2:auddmg:u:manager");
impl INx2AudioVirtualDeviceManager {
    const NAME_SIZE: usize = 0x100;
    const DEVICE_NAME: &'static [u8] = b"AudioVirtualOutput0";
    const PHYSICAL_DEVICE_NAME: &'static [u8] = b"AudioTvOutput";
    fn new(system: SystemRef, streams: Arc<Nx2SysmixStreams>) -> Self {
        let mut context = ServiceContext::new_with_system(system, "nx2:auddmg:u:manager".into());
        let state_event = context.create_event("nx2:auddmg:u:StateEvent".into());
        Self {
            context,
            state_event,
            streams,
            handlers_tipc: BTreeMap::new(),
            handlers: build_handler_map(&[
                (
                    0,
                    Some(Self::list_virtual_output_devices),
                    "ListVirtualOutputDevices",
                ),
                (
                    100,
                    Some(Self::get_default_virtual_output_device),
                    "GetDefaultVirtualOutputDevice",
                ),
                (
                    300,
                    Some(Self::is_stream_format_supported),
                    "IsStreamFormatSupported",
                ),
                (
                    401,
                    Some(Self::get_virtual_output_device_properties),
                    "GetVirtualOutputDeviceProperties",
                ),
                (
                    500,
                    Some(Self::acquire_state_change_event),
                    "AcquireStateChangeEvent",
                ),
                (700, Some(Self::attach_stream), "AttachStream"),
                (800, Some(Self::detach_stream), "DetachStream"),
            ]),
        }
    }
    fn copy_name(out: &mut [u8], name: &[u8]) {
        let len = name.len().min(Self::NAME_SIZE - 1);
        out[..len].copy_from_slice(&name[..len]);
    }
    fn list_virtual_output_devices(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut names = [0; Self::NAME_SIZE];
        Self::copy_name(&mut names, Self::DEVICE_NAME);
        let count = (ctx.get_write_buffer_size(0) >= Self::NAME_SIZE) as u32;
        if count != 0 {
            ctx.write_buffer(&names, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count);
    }
    fn get_default_virtual_output_device(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut name = [0; Self::NAME_SIZE];
        Self::copy_name(&mut name, Self::DEVICE_NAME);
        ctx.write_buffer(&name, 0);
        reply(ctx, RESULT_SUCCESS);
    }
    fn is_stream_format_supported(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let supported = SysmixOutput::is_format_supported(rp.pop_u32(), rp.pop_u32(), rp.pop_u32());
        let functionality = rp.pop_u32();
        reply(
            ctx,
            if supported && functionality == 1 {
                RESULT_SUCCESS
            } else {
                audio_result(RESULT_NOT_SUPPORTED)
            },
        );
    }
    fn property_block(name: &[u8]) -> [u8; 0x228] {
        let mut block = [0; 0x228];
        Self::copy_name(&mut block, name);
        for (offset, value) in [
            (0x100, 3u32),
            (0x104, 2),
            (0x108, 2),
            (0x10c, 48000),
            (0x220, 1),
        ] {
            block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        Self::copy_name(&mut block[0x110..], Self::PHYSICAL_DEVICE_NAME);
        block[0x218..0x220].copy_from_slice(&10_000_000u64.to_le_bytes());
        block
    }
    fn get_virtual_output_device_properties(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let block = Self::property_block(&ctx.read_buffer(0));
        ctx.write_buffer(&block, 0);
        reply(ctx, RESULT_SUCCESS);
    }
    fn acquire_state_change_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = Self::as_self(this);
        copy_event(
            ctx,
            this.context.get_event(this.state_event),
            RESULT_SUCCESS,
        );
    }
    fn attach_stream(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let id = rp.pop_u32();
        let functionality = rp.pop_u32();
        let result = if functionality == 1 {
            Self::as_self(this)
                .streams
                .set_engaged(id, true, ctx.owner_process_arc())
        } else {
            audio_result(RESULT_NOT_SUPPORTED)
        };
        reply(ctx, result);
    }
    fn detach_stream(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let id = RequestParser::new(ctx).pop_u32();
        let result = Self::as_self(this)
            .streams
            .set_engaged(id, false, ctx.owner_process_arc());
        reply(ctx, result);
    }
}
impl Drop for INx2AudioVirtualDeviceManager {
    fn drop(&mut self) {
        self.context.close_event(self.state_event);
    }
}
pub struct INx2AudioDeviceManagerService {
    system: SystemRef,
    streams: Arc<Nx2SysmixStreams>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
service_impl!(INx2AudioDeviceManagerService, "nx2:auddmg:u");
impl INx2AudioDeviceManagerService {
    pub fn new(system: SystemRef, streams: Arc<Nx2SysmixStreams>) -> Self {
        Self {
            system,
            streams,
            handlers_tipc: BTreeMap::new(),
            handlers: build_handler_map(&[(
                0,
                Some(Self::open_virtual_device_manager),
                "OpenVirtualDeviceManager",
            )]),
        }
    }
    fn open_virtual_device_manager(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = Self::as_self(this);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(INx2AudioVirtualDeviceManager::new(
            this.system,
            this.streams.clone(),
        )));
    }
}
struct INx2SysmixStreamSession {
    streams: Arc<Nx2SysmixStreams>,
    stream: Arc<SysmixOutput>,
    id: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
service_impl!(INx2SysmixStreamSession, "nx2:audsmx:u:stream");
impl INx2SysmixStreamSession {
    fn new(streams: Arc<Nx2SysmixStreams>, stream: Arc<SysmixOutput>, id: u32) -> Self {
        Self {
            streams,
            stream,
            id,
            handlers_tipc: BTreeMap::new(),
            handlers: build_handler_map(&[
                (0, Some(Self::open_stream), "OpenStream"),
                (1, Some(Self::close_stream), "CloseStream"),
                (2, Some(Self::get_stream_state), "GetStreamState"),
                (3, Some(Self::get_stream_gain), "GetStreamGain"),
                (4, Some(Self::set_stream_gain), "SetStreamGain"),
                (
                    5,
                    Some(Self::acquire_state_change_event),
                    "AcquireStreamStateChangeEvent",
                ),
                (6, Some(Self::succeed), "ReleaseStreamStateChangeEvent"),
                (7, Some(Self::acquire_data_event), "AcquireStreamDataEvent"),
                (8, Some(Self::release_data_event), "ReleaseStreamDataEvent"),
                (9, Some(Self::unsupported), "ClockStream"),
                (10, Some(Self::unsupported), "PrimeStream"),
                (11, Some(Self::unsupported), "PurgeStream"),
                (12, Some(Self::succeed), "RenameStream"),
            ]),
        }
    }
    fn open_stream(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let args = [
            rp.pop_u32(),
            rp.pop_u32(),
            rp.pop_u32(),
            rp.pop_u32(),
            rp.pop_u32(),
            rp.pop_u32(),
        ];
        let this = Self::as_self(this);
        let result = this
            .stream
            .open(args[0], args[1], args[2], args[3], args[4], args[5]);
        log::info!("sysmix stream {} open {:?}: {:?}", this.id, args, result);
        reply(ctx, result);
    }
    fn close_stream(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::as_self(this).stream.close();
        reply(ctx, RESULT_SUCCESS);
    }
    fn get_stream_state(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(Self::as_self(this).stream.get_state());
    }
    fn get_stream_gain(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_f32(Self::as_self(this).stream.get_gain());
    }
    fn set_stream_gain(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let gain = rp.pop_f32();
        rp.pop_u32();
        let ramp = rp.pop_i64();
        reply(ctx, Self::as_self(this).stream.set_gain(gain, ramp));
    }
    fn acquire_state_change_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        copy_event(
            ctx,
            Self::as_self(this).stream.get_state_event(),
            RESULT_SUCCESS,
        );
    }
    fn acquire_data_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let mode = rp.pop_u32();
        let threshold = rp.pop_i32();
        let stream = &Self::as_self(this).stream;
        let result = stream.set_data_notification(mode, threshold);
        copy_event(ctx, stream.get_data_event(), result);
    }
    fn release_data_event(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        reply(ctx, Self::as_self(this).stream.set_data_notification(0, 0));
    }
    fn unsupported(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        reply(ctx, audio_result(RESULT_NOT_SUPPORTED));
    }
    fn succeed(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        reply(ctx, RESULT_SUCCESS);
    }
}
impl Drop for INx2SysmixStreamSession {
    fn drop(&mut self) {
        self.streams.state.lock().unwrap().streams.remove(&self.id);
        self.stream.close();
    }
}
struct INx2SysmixUserSession {
    system: SystemRef,
    streams: Arc<Nx2SysmixStreams>,
    process: Arc<ProcessLock>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
service_impl!(INx2SysmixUserSession, "nx2:audsmx:u:session");
impl INx2SysmixUserSession {
    fn new(system: SystemRef, streams: Arc<Nx2SysmixStreams>, process: Arc<ProcessLock>) -> Self {
        Self {
            system,
            streams,
            process,
            handlers_tipc: BTreeMap::new(),
            handlers: build_handler_map(&[
                (1, Some(Self::create_stream), "CreateStream"),
                (2, Some(Self::engage_streams), "EngageStreams"),
                (3, Some(Self::disengage_streams), "DisengageStreams"),
            ]),
        }
    }
    fn create_stream(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let device = rp.pop_u32();
        let flags = rp.pop_u32();
        let _aruid = rp.pop_u64();
        let buffer = rp.pop_u64();
        let size = rp.pop_u64();
        let result = if device == 1
            && ctx
                .owner_process_arc()
                .is_some_and(|p| Arc::ptr_eq(&p, &this.process))
        {
            SysmixOutput::validate_region(&this.process, buffer, size, flags)
        } else {
            RESULT_INVALID_ARGUMENT
        };
        if result != RESULT_SUCCESS {
            reply(ctx, result);
            return;
        }
        let id = {
            let mut state = this.streams.state.lock().unwrap();
            let id = state.next_id;
            state.next_id = id.wrapping_add(1);
            id
        };
        let stream = Arc::new(SysmixOutput::new(
            this.system,
            this.process.clone(),
            id,
            buffer,
            size,
            flags,
        ));
        this.streams
            .state
            .lock()
            .unwrap()
            .streams
            .insert(id, stream.clone());
        log::info!(
            "sysmix stream {} create flags={:#x} buffer={:#x} size={:#x}",
            id,
            flags,
            buffer,
            size
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(id);
        rb.push_ipc_interface(Arc::new(INx2SysmixStreamSession::new(
            this.streams.clone(),
            stream,
            id,
        )));
    }
    fn set_engaged(&self, ctx: &mut HLERequestContext, engaged: bool) {
        let buffer = ctx.read_buffer(0);
        let mut result = RESULT_INVALID_ARGUMENT;
        if buffer.len() % 4 == 0 {
            result = RESULT_SUCCESS;
            for bytes in buffer.chunks_exact(4) {
                result = self.streams.set_engaged(
                    u32::from_le_bytes(bytes.try_into().unwrap()),
                    engaged,
                    ctx.owner_process_arc(),
                );
                if result != RESULT_SUCCESS {
                    break;
                }
            }
        }
        reply(ctx, result);
    }
    fn engage_streams(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::as_self(this).set_engaged(ctx, true);
    }
    fn disengage_streams(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::as_self(this).set_engaged(ctx, false);
    }
}
pub struct INx2AudioSoundMixerService {
    system: SystemRef,
    streams: Arc<Nx2SysmixStreams>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
service_impl!(INx2AudioSoundMixerService, "nx2:audsmx:u");
impl INx2AudioSoundMixerService {
    pub fn new(system: SystemRef, streams: Arc<Nx2SysmixStreams>) -> Self {
        Self {
            system,
            streams,
            handlers_tipc: BTreeMap::new(),
            handlers: build_handler_map(&[(0, Some(Self::open_user_session), "OpenUserSession")]),
        }
    }
    fn open_user_session(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = Self::as_self(this);
        let process = ctx.owner_process_arc();
        let handle = ctx.get_copy_handle(0);
        let valid = process.as_ref().is_some_and(|process| {
            let p = process.lock().unwrap();
            // HLE keeps incoming handles in the calling process handle table.
            handle == crate::hle::kernel::svc_common::PseudoHandle::CurrentProcess as u32
                || p.handle_table.get_object(handle) == Some(p.get_process_id())
        });
        if !valid {
            reply(ctx, audio_result(RESULT_INVALID_HANDLE));
            return;
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(INx2SysmixUserSession::new(
            this.system,
            this.streams.clone(),
            process.unwrap(),
        )));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn virtual_device_properties_have_exact_layout_and_zero_padding() {
        let b = INx2AudioVirtualDeviceManager::property_block(b"AudioVirtualOutput0");
        assert_eq!(&b[..19], b"AudioVirtualOutput0");
        assert!(b[19..0x100].iter().all(|v| *v == 0));
        for (o, v) in [
            (0x100, 3u32),
            (0x104, 2),
            (0x108, 2),
            (0x10c, 48000),
            (0x220, 1),
        ] {
            assert_eq!(&b[o..o + 4], &v.to_le_bytes());
        }
        assert_eq!(&b[0x110..0x11d], b"AudioTvOutput");
        assert!(b[0x11d..0x218].iter().all(|v| *v == 0));
        assert_eq!(&b[0x218..0x220], &10_000_000u64.to_le_bytes());
        assert_eq!(&b[0x224..], &[0; 4]);
    }
}
