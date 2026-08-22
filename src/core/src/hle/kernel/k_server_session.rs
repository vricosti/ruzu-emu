//! Port of zuyu/src/core/hle/kernel/k_server_session.h / k_server_session.cpp
//! Status: Partial (structural port)
//!
//! KServerSession: the server endpoint of a session, handles incoming requests.
//!
//! In upstream, the ServerManager links a KServerSession to a SessionRequestManager
//! via RegisterSession(). When a client sends a request, the server session
//! dispatches to the handler via the manager.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::k_process::ProcessLock;
use super::k_process_page_table::KProcessPageTable;
use crate::hle::kernel::k_memory_block::{
    KMemoryAttribute, KMemoryPermission, KMemoryState, PAGE_SIZE,
};
use crate::hle::kernel::k_session_request::KSessionRequest;
use crate::hle::kernel::k_synchronization_object;
use crate::hle::kernel::k_synchronization_object::SynchronizationObjectState;
use crate::hle::kernel::k_thread::KThreadLock;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::message_buffer::{MessageBuffer, MESSAGE_BUFFER_SIZE};
use crate::hle::kernel::svc::svc_results::{
    RESULT_INVALID_COMBINATION, RESULT_INVALID_CURRENT_MEMORY, RESULT_INVALID_STATE,
    RESULT_MESSAGE_TOO_LARGE, RESULT_NOT_FOUND, RESULT_RECEIVE_LIST_BROKEN, RESULT_SESSION_CLOSED,
    RESULT_TERMINATION_REQUESTED,
};
use crate::hle::kernel::svc_common::INVALID_HANDLE;

/// `RUZU_TRACE_CURREQ=1` — log every KServerSession `current_request` SET
/// (receive_request_hle) and CLEAR (send_reply / clear_current_request_and_notify),
/// keyed by session pointer. At a wedge, a session with a SET but no matching
/// CLEAR is one whose handler never replied — its `is_signaled()` is stuck false
/// so later IPC on it blocks forever.
fn curreq_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUZU_TRACE_CURREQ").is_some())
}
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestManager};
use crate::hle::service::os::event::Event;

const POINTER_TRANSFER_BUFFER_ALIGNMENT: usize = 0x10;

struct ReceiveList {
    recv_list_count: u32,
    data: Vec<u32>,
    msg_buffer_end: u64,
    msg_buffer_space_end: u64,
}

impl ReceiveList {
    fn get_entry_count(header: &crate::hle::kernel::message_buffer::MessageHeader) -> usize {
        match header.get_receive_list_count() {
            x if x == crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32 => 0,
            x if x
                == crate::hle::kernel::message_buffer::ReceiveListCountType::ToMessageBuffer
                    as u32 =>
            {
                0
            }
            x if x
                == crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer
                    as u32 =>
            {
                1
            }
            x => {
                (x - crate::hle::kernel::message_buffer::RECEIVE_LIST_COUNT_TYPE_COUNT_OFFSET)
                    as usize
            }
        }
    }

    fn new(
        dst_words: &[u32],
        dst_address: u64,
        dst_header: &crate::hle::kernel::message_buffer::MessageHeader,
        dst_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        msg_size: usize,
        out_offset: usize,
    ) -> Self {
        let recv_list_count = dst_header.get_receive_list_count();
        let msg_buffer_end = dst_address + (std::mem::size_of::<u32>() * out_offset) as u64;
        let msg_buffer_space_end = dst_address + msg_size as u64;
        let recv_list_index =
            crate::hle::kernel::message_buffer::MessageBuffer::get_receive_list_index(
                dst_header,
                dst_special_header,
            );
        let entry_words = Self::get_entry_count(dst_header)
            * (crate::hle::kernel::message_buffer::ReceiveListEntry::DATA_SIZE
                / std::mem::size_of::<u32>());
        let data = if entry_words == 0 {
            Vec::new()
        } else {
            dst_words[recv_list_index..recv_list_index + entry_words].to_vec()
        };
        Self {
            recv_list_count,
            data,
            msg_buffer_end,
            msg_buffer_space_end,
        }
    }

    fn is_index(&self) -> bool {
        self.recv_list_count
            > crate::hle::kernel::message_buffer::RECEIVE_LIST_COUNT_TYPE_COUNT_OFFSET
    }

    fn is_to_message_buffer(&self) -> bool {
        self.recv_list_count
            == crate::hle::kernel::message_buffer::ReceiveListCountType::ToMessageBuffer as u32
    }

    fn get_buffer(&self, size: usize, key: &mut i32) -> u64 {
        match self.recv_list_count {
            x if x == crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32 => 0,
            x if x
                == crate::hle::kernel::message_buffer::ReceiveListCountType::ToMessageBuffer
                    as u32 =>
            {
                let buf = (self.msg_buffer_end + (*key as u64))
                    .next_multiple_of(POINTER_TRANSFER_BUFFER_ALIGNMENT as u64);
                if buf < buf.saturating_add(size as u64)
                    && buf.saturating_add(size as u64) <= self.msg_buffer_space_end
                {
                    *key = (buf + size as u64 - self.msg_buffer_end) as i32;
                    buf
                } else {
                    0
                }
            }
            x if x
                == crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer
                    as u32 =>
            {
                let entry = crate::hle::kernel::message_buffer::ReceiveListEntry::from_raw(
                    self.data[0],
                    self.data[1],
                );
                let buf = (entry.get_address() + (*key as u64))
                    .next_multiple_of(POINTER_TRANSFER_BUFFER_ALIGNMENT as u64);
                let entry_addr = entry.get_address();
                let entry_size = entry.get_size() as u64;
                if buf < buf.saturating_add(size as u64)
                    && entry_addr < entry_addr.saturating_add(entry_size)
                    && buf.saturating_add(size as u64) <= entry_addr.saturating_add(entry_size)
                {
                    *key = (buf + size as u64 - entry_addr) as i32;
                    buf
                } else {
                    0
                }
            }
            _ => {
                let index = *key as usize;
                let max_entries = (self.recv_list_count
                    - crate::hle::kernel::message_buffer::RECEIVE_LIST_COUNT_TYPE_COUNT_OFFSET)
                    as usize;
                if index >= max_entries {
                    return 0;
                }
                let entry = crate::hle::kernel::message_buffer::ReceiveListEntry::from_raw(
                    self.data[index * 2],
                    self.data[index * 2 + 1],
                );
                if entry.get_address() < entry.get_address().saturating_add(entry.get_size() as u64)
                    && entry.get_size() >= size
                {
                    entry.get_address()
                } else {
                    0
                }
            }
        }
    }
}

/// The server session object.
/// Matches upstream `KServerSession` class (k_server_session.h).
pub struct KServerSession {
    /// Parent KSession ID.
    pub parent_id: Option<u64>,
    /// Pending request list (upstream: intrusive list of KSessionRequest).
    pub request_list: VecDeque<Arc<Mutex<KSessionRequest>>>,
    /// Currently being processed request.
    pub current_request: Option<Arc<Mutex<KSessionRequest>>>,
    /// Whether the client side of the parent session has been closed.
    ///
    /// Upstream reaches this via `m_parent->IsClientClosed()` because
    /// `KServerSession` owns an inline parent pointer. Rust stores the closure
    /// state locally to avoid re-entering process/session registries from
    /// `is_signaled()`, which is called while higher-level wait code may
    /// already hold process/session locks.
    pub client_closed: bool,
    /// The session request manager linked via ServerManager::RegisterSession.
    /// Matches upstream where ServerManager stores the Session wrapper that
    /// pairs the KServerSession with a SessionRequestManager.
    pub manager: Option<Arc<Mutex<SessionRequestManager>>>,
    /// Waitable synchronization state owned by KSynchronizationObject upstream.
    pub sync_object: SynchronizationObjectState,
    /// Weak back-pointer to the owning ServerManager's wakeup event. The
    /// session holder is temporarily unlinked while its request is handled, so
    /// this event closes the interval in which a new request cannot notify a
    /// waiter on the session object itself.
    pub manager_wakeup: Option<std::sync::Weak<Event>>,
    /// Rust bridge for endpoint-close ownership. Closure notifications return
    /// to the owning ServerManager through this queue so destruction remains
    /// on the same owner as upstream.
    manager_close_queue: Option<std::sync::Weak<Mutex<Vec<u64>>>>,
    manager_close_wakeup: Option<std::sync::Weak<Event>>,
}

impl KServerSession {
    fn record_ipc_phase_count(label: &'static str) {
        if std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some() {
            crate::hle::kernel::svc::svc_ipc::record_ipc_phase(label, std::time::Duration::ZERO);
        }
    }

    fn get_map_alias_memory_state(attribute: u32) -> Option<KMemoryState> {
        match attribute {
            x if x == crate::hle::kernel::message_buffer::MapAliasAttribute::Ipc as u32 => {
                Some(KMemoryState::IPC)
            }
            x if x
                == crate::hle::kernel::message_buffer::MapAliasAttribute::NonSecureIpc as u32 =>
            {
                Some(KMemoryState::NON_SECURE_IPC)
            }
            x if x
                == crate::hle::kernel::message_buffer::MapAliasAttribute::NonDeviceIpc as u32 =>
            {
                Some(KMemoryState::NON_DEVICE_IPC)
            }
            _ => None,
        }
    }

    fn get_map_alias_test_state_and_attribute_mask(
        state: KMemoryState,
    ) -> Option<(KMemoryState, KMemoryAttribute)> {
        match state {
            KMemoryState::IPC => Some((
                KMemoryState::FLAG_CAN_USE_IPC,
                KMemoryAttribute::UNCACHED
                    | KMemoryAttribute::DEVICE_SHARED
                    | KMemoryAttribute::LOCKED,
            )),
            KMemoryState::NON_SECURE_IPC => Some((
                KMemoryState::FLAG_CAN_USE_NON_SECURE_IPC,
                KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
            )),
            KMemoryState::NON_DEVICE_IPC => Some((
                KMemoryState::FLAG_CAN_USE_NON_DEVICE_IPC,
                KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
            )),
            _ => None,
        }
    }

    fn encode_map_alias_descriptor(address: u64, size: usize, attribute: u32) -> [u32; 3] {
        [
            size as u32,
            address as u32,
            (attribute & 0x3)
                | ((((address >> 36) & 0x7) as u32) << 2)
                | ((((size as u64 >> 32) & 0xF) as u32) << 24)
                | ((((address >> 32) & 0xF) as u32) << 28),
        ]
    }

    fn process_receive_message_pointer_descriptors(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_header: &crate::hle::kernel::message_buffer::MessageHeader,
        dst_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_address: usize,
        dst_size: usize,
        dst_user: bool,
        dst_page_table: &KProcessPageTable,
        src_page_table: &KProcessPageTable,
    ) -> u32 {
        let src_end_offset = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            src_header,
            src_special_header,
        ) + src_header.get_raw_count() as usize;
        let dst_recv_list = ReceiveList::new(
            dst_words,
            dst_address as u64,
            dst_header,
            dst_special_header,
            dst_size,
            src_end_offset,
        );
        let mut dst_message = MessageBuffer::new(dst_words);
        let mut pointer_key = 0;
        let mut offset =
            crate::hle::kernel::message_buffer::MessageBuffer::get_pointer_descriptor_index(
                src_header,
                src_special_header,
            );

        for _ in 0..src_header.get_pointer_count() {
            let cur_offset = offset;
            let src_desc = crate::hle::kernel::message_buffer::PointerDescriptor::from_raw([
                src_words[cur_offset],
                src_words[cur_offset + 1],
            ]);
            offset += crate::hle::kernel::message_buffer::PointerDescriptor::DATA_SIZE
                / std::mem::size_of::<u32>();

            let recv_size = src_desc.get_size();
            let mut recv_pointer = 0u64;
            if recv_size > 0 {
                if dst_recv_list.is_index() {
                    pointer_key = src_desc.get_index();
                }
                recv_pointer = dst_recv_list.get_buffer(recv_size, &mut pointer_key);
                if recv_pointer == 0 {
                    return crate::hle::kernel::svc::svc_results::RESULT_OUT_OF_RESOURCE
                        .get_inner_value();
                }

                let rc = if dst_user && dst_recv_list.is_to_message_buffer() {
                    src_page_table.copy_memory_from_heap_to_heap_without_check_destination(
                        dst_page_table,
                        KProcessAddress::new(recv_pointer),
                        recv_size,
                        KMemoryState::FLAG_REFERENCE_COUNTED,
                        KMemoryState::FLAG_REFERENCE_COUNTED,
                        KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                        KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
                        KMemoryAttribute::LOCKED,
                        KProcessAddress::new(src_desc.get_address()),
                        KMemoryState::FLAG_LINEAR_MAPPED,
                        KMemoryState::FLAG_LINEAR_MAPPED,
                        KMemoryPermission::USER_READ,
                        KMemoryAttribute::UNCACHED,
                        KMemoryAttribute::NONE,
                    )
                } else {
                    src_page_table.copy_memory_from_linear_to_user(
                        KProcessAddress::new(recv_pointer),
                        recv_size,
                        KProcessAddress::new(src_desc.get_address()),
                        KMemoryState::FLAG_LINEAR_MAPPED,
                        KMemoryState::FLAG_LINEAR_MAPPED,
                        KMemoryPermission::USER_READ,
                        KMemoryAttribute::UNCACHED,
                        KMemoryAttribute::NONE,
                    )
                };
                if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return rc;
                }
            }

            dst_message.set(
                cur_offset,
                &[
                    ((src_desc.get_index() as u32) & 0xF)
                        | ((((recv_pointer >> 32) & 0xF) as u32) << 12)
                        | (((recv_size as u32) & 0xFFFF) << 16)
                        | ((((recv_pointer >> 36) & 0x7) as u32) << 6),
                    recv_pointer as u32,
                ],
            );
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn process_send_message_pointer_descriptors(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_header: &crate::hle::kernel::message_buffer::MessageHeader,
        dst_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_address: usize,
        dst_size: usize,
        dst_user: bool,
        dst_page_table: &KProcessPageTable,
        _src_page_table: &KProcessPageTable,
    ) -> u32 {
        let src_end_offset = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            src_header,
            src_special_header,
        ) + src_header.get_raw_count() as usize;
        let dst_recv_list = ReceiveList::new(
            dst_words,
            dst_address as u64,
            dst_header,
            dst_special_header,
            dst_size,
            src_end_offset,
        );
        let mut dst_message = MessageBuffer::new(dst_words);
        let mut pointer_key = 0;
        let mut offset =
            crate::hle::kernel::message_buffer::MessageBuffer::get_pointer_descriptor_index(
                src_header,
                src_special_header,
            );

        for _ in 0..src_header.get_pointer_count() {
            let cur_offset = offset;
            let src_desc = crate::hle::kernel::message_buffer::PointerDescriptor::from_raw([
                src_words[cur_offset],
                src_words[cur_offset + 1],
            ]);
            offset += crate::hle::kernel::message_buffer::PointerDescriptor::DATA_SIZE
                / std::mem::size_of::<u32>();

            let recv_size = src_desc.get_size();
            let mut recv_pointer = 0u64;
            if recv_size > 0 {
                if dst_recv_list.is_index() {
                    pointer_key = src_desc.get_index();
                }
                recv_pointer = dst_recv_list.get_buffer(recv_size, &mut pointer_key);
                if recv_pointer == 0 {
                    return crate::hle::kernel::svc::svc_results::RESULT_OUT_OF_RESOURCE
                        .get_inner_value();
                }

                let dst_heap = dst_user && dst_recv_list.is_to_message_buffer();
                let dst_state = if dst_heap {
                    KMemoryState::FLAG_REFERENCE_COUNTED
                } else {
                    KMemoryState::FLAG_LINEAR_MAPPED
                };
                let dst_perm = if dst_heap {
                    KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE
                } else {
                    KMemoryPermission::USER_READ_WRITE
                };
                let rc = dst_page_table.copy_memory_from_user_to_linear(
                    KProcessAddress::new(recv_pointer),
                    recv_size,
                    dst_state,
                    dst_state,
                    dst_perm,
                    KMemoryAttribute::UNCACHED,
                    KMemoryAttribute::NONE,
                    KProcessAddress::new(src_desc.get_address()),
                );
                if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return rc;
                }
            }

            dst_message.set(
                cur_offset,
                &[
                    ((src_desc.get_index() as u32) & 0xF)
                        | ((((recv_pointer >> 32) & 0xF) as u32) << 12)
                        | (((recv_size as u32) & 0xFFFF) << 16)
                        | ((((recv_pointer >> 36) & 0x7) as u32) << 6),
                    recv_pointer as u32,
                ],
            );
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn process_receive_message_map_alias_descriptors(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        request: &mut KSessionRequest,
        dst_page_table: &mut KProcessPageTable,
        src_page_table: &mut KProcessPageTable,
    ) -> u32 {
        let mut dst_message = MessageBuffer::new(dst_words);
        let map_alias_index =
            crate::hle::kernel::message_buffer::MessageBuffer::get_map_alias_descriptor_index(
                src_header,
                src_special_header,
            );

        for i in 0..src_header.get_map_alias_count() {
            let cur_offset = map_alias_index
                + i as usize
                    * (crate::hle::kernel::message_buffer::MapAliasDescriptor::DATA_SIZE
                        / std::mem::size_of::<u32>());
            let src_desc = crate::hle::kernel::message_buffer::MapAliasDescriptor::from_raw([
                src_words[cur_offset],
                src_words[cur_offset + 1],
                src_words[cur_offset + 2],
            ]);
            let src_address = KProcessAddress::new(src_desc.get_address());
            let size = src_desc.get_size() as usize;
            let Some(dst_state) = Self::get_map_alias_memory_state(src_desc.get_attribute()) else {
                return RESULT_INVALID_COMBINATION.get_inner_value();
            };
            let perm = if i >= src_header.get_send_count() {
                KMemoryPermission::USER_READ_WRITE
            } else {
                KMemoryPermission::USER_READ
            };
            let send = i < src_header.get_send_count()
                || i >= src_header.get_send_count() + src_header.get_receive_count();

            let mut dst_address = KProcessAddress::new(0);
            let rc = dst_page_table.setup_for_ipc(
                &mut dst_address,
                size,
                src_address,
                src_page_table,
                perm,
                dst_state,
                send,
            );
            crate::hle::kernel::svc::svc_memory_history::record_ipc_map_alias(
                src_address.get(),
                dst_address.get(),
                size as u64,
                rc,
                dst_state.bits(),
                perm.bits() as u32,
                src_desc.get_attribute() as u32,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }

            let push_rc = if perm == KMemoryPermission::USER_READ {
                request
                    .mappings
                    .push_send(src_address, dst_address, size, dst_state)
            } else if send {
                request
                    .mappings
                    .push_exchange(src_address, dst_address, size, dst_state)
            } else {
                request
                    .mappings
                    .push_receive(src_address, dst_address, size, dst_state)
            };
            if push_rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                if size > 0 {
                    let _ = dst_page_table.cleanup_for_ipc_server(dst_address, size, dst_state);
                    let _ = src_page_table.cleanup_for_ipc_client(src_address, size, dst_state);
                }
                return push_rc;
            }

            dst_message.set(
                cur_offset,
                &Self::encode_map_alias_descriptor(
                    dst_address.get(),
                    size,
                    src_desc.get_attribute(),
                ),
            );
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn process_send_message_receive_mapping(
        src_page_table: &KProcessPageTable,
        dst_page_table: &KProcessPageTable,
        client_address: KProcessAddress,
        server_address: KProcessAddress,
        size: usize,
        src_state: KMemoryState,
    ) -> u32 {
        if size == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let Some((test_state, test_attr_mask)) =
            Self::get_map_alias_test_state_and_attribute_mask(src_state)
        else {
            return RESULT_INVALID_COMBINATION.get_inner_value();
        };

        let client_address = client_address.get();
        let server_address = server_address.get();
        let Some(source_bytes) = src_page_table
            .get_base()
            .read_block_from_own_page_table(server_address as usize, size)
        else {
            return RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let aligned_dst_start = client_address & !((PAGE_SIZE as u64) - 1);
        let aligned_dst_end = (client_address + size as u64).next_multiple_of(PAGE_SIZE as u64);
        let mapping_dst_start = client_address.next_multiple_of(PAGE_SIZE as u64);
        let mapping_dst_end = (client_address + size as u64) & !((PAGE_SIZE as u64) - 1);

        if aligned_dst_start != mapping_dst_start {
            debug_assert!(client_address < mapping_dst_start);
            let copy_size = std::cmp::min(size, (mapping_dst_start - client_address) as usize);
            let rc = dst_page_table.copy_memory_from_kernel_to_linear(
                KProcessAddress::new(client_address),
                copy_size,
                test_state,
                test_state,
                KMemoryPermission::USER_READ_WRITE,
                test_attr_mask,
                KMemoryAttribute::NONE,
                source_bytes.as_ptr() as usize,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }

        if mapping_dst_end < aligned_dst_end
            && (aligned_dst_start == mapping_dst_start || aligned_dst_start < mapping_dst_end)
        {
            let copy_size = (client_address + size as u64 - mapping_dst_end) as usize;
            let source_offset = mapping_dst_end.saturating_sub(client_address) as usize;
            let rc = dst_page_table.copy_memory_from_kernel_to_linear(
                KProcessAddress::new(mapping_dst_end),
                copy_size,
                test_state,
                test_state,
                KMemoryPermission::USER_READ_WRITE,
                test_attr_mask,
                KMemoryAttribute::NONE,
                unsafe { source_bytes.as_ptr().add(source_offset) as usize },
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn process_send_message_receive_mappings(
        request: &KSessionRequest,
        src_page_table: &KProcessPageTable,
        dst_page_table: &KProcessPageTable,
    ) -> u32 {
        for index in 0..request.mappings.get_receive_count() {
            let mapping = request.mappings.get_receive_mapping(index);
            let rc = Self::process_send_message_receive_mapping(
                src_page_table,
                dst_page_table,
                mapping.client_address,
                mapping.server_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        for index in 0..request.mappings.get_exchange_count() {
            let mapping = request.mappings.get_exchange_mapping(index);
            let rc = Self::process_send_message_receive_mapping(
                src_page_table,
                dst_page_table,
                mapping.client_address,
                mapping.server_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn process_send_message_raw_data(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_page_table: &KProcessPageTable,
        dst_message_buffer: usize,
        dst_user: bool,
        src_message_buffer: usize,
        src_user: bool,
    ) -> u32 {
        let raw_count = src_header.get_raw_count();
        if raw_count == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let raw_data_index = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            src_header,
            src_special_header,
        );
        let offset_words = raw_data_index * std::mem::size_of::<u32>();
        let raw_size = raw_count as usize * std::mem::size_of::<u32>();
        let raw_end = raw_data_index + raw_count as usize;

        if !dst_user && !src_user {
            dst_words[raw_data_index..raw_end].copy_from_slice(&src_words[raw_data_index..raw_end]);
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let result = if src_user {
            let max_fast_size = std::cmp::min(offset_words + raw_size, PAGE_SIZE);
            let fast_size = max_fast_size - offset_words;
            let dst_state = if dst_user {
                KMemoryState::FLAG_REFERENCE_COUNTED
            } else {
                KMemoryState::FLAG_LINEAR_MAPPED
            };
            let dst_perm = if dst_user {
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE
            } else {
                KMemoryPermission::USER_READ_WRITE
            };

            let src_ptr = unsafe { src_words.as_ptr().add(raw_data_index) as usize };
            let mut rc = dst_page_table.copy_memory_from_kernel_to_linear(
                KProcessAddress::new((dst_message_buffer + offset_words) as u64),
                fast_size,
                dst_state,
                dst_state,
                dst_perm,
                KMemoryAttribute::UNCACHED,
                KMemoryAttribute::NONE,
                src_ptr,
            );

            if rc == crate::hle::result::RESULT_SUCCESS.get_inner_value() && fast_size < raw_size {
                rc = dst_page_table.copy_memory_from_heap_to_heap(
                    dst_page_table,
                    KProcessAddress::new((dst_message_buffer + max_fast_size) as u64),
                    raw_size - fast_size,
                    dst_state,
                    dst_state,
                    dst_perm,
                    KMemoryAttribute::UNCACHED,
                    KMemoryAttribute::NONE,
                    KProcessAddress::new((src_message_buffer + max_fast_size) as u64),
                    KMemoryState::FLAG_REFERENCE_COUNTED,
                    KMemoryState::FLAG_REFERENCE_COUNTED,
                    KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ,
                    KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
                    KMemoryAttribute::LOCKED,
                );
            }
            rc
        } else {
            dst_page_table.copy_memory_from_user_to_linear(
                KProcessAddress::new((dst_message_buffer + offset_words) as u64),
                raw_size,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                KMemoryAttribute::UNCACHED,
                KMemoryAttribute::NONE,
                KProcessAddress::new((src_message_buffer + offset_words) as u64),
            )
        };

        if result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            // The current Rust transport still flushes `dst_words` at the end of
            // SendReply, so keep the local mirror coherent until the final
            // message-buffer owner is moved fully to the page-table path.
            dst_words[raw_data_index..raw_end].copy_from_slice(&src_words[raw_data_index..raw_end]);
        }

        result
    }

    fn process_receive_message_raw_data(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_page_table: &KProcessPageTable,
        dst_message_buffer: usize,
        dst_user: bool,
        src_page_table: &KProcessPageTable,
        src_message_buffer: usize,
        src_user: bool,
    ) -> u32 {
        let raw_count = src_header.get_raw_count();
        if raw_count == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let raw_data_index = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            src_header,
            src_special_header,
        );
        let offset_words = raw_data_index * std::mem::size_of::<u32>();
        let raw_size = raw_count as usize * std::mem::size_of::<u32>();
        let raw_end = raw_data_index + raw_count as usize;

        if !dst_user && !src_user {
            dst_words[raw_data_index..raw_end].copy_from_slice(&src_words[raw_data_index..raw_end]);
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let result = if dst_user {
            let max_fast_size = std::cmp::min(offset_words + raw_size, PAGE_SIZE);
            let fast_size = max_fast_size - offset_words;
            let src_state = if src_user {
                KMemoryState::FLAG_REFERENCE_COUNTED
            } else {
                KMemoryState::FLAG_LINEAR_MAPPED
            };
            let src_perm = if src_user {
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ
            } else {
                KMemoryPermission::USER_READ
            };

            let dst_ptr = unsafe { dst_words.as_mut_ptr().add(raw_data_index) as usize };
            let mut rc = src_page_table.copy_memory_from_linear_to_kernel(
                dst_ptr,
                fast_size,
                KProcessAddress::new((src_message_buffer + offset_words) as u64),
                src_state,
                src_state,
                src_perm,
                KMemoryAttribute::UNCACHED,
                KMemoryAttribute::NONE,
            );

            if rc == crate::hle::result::RESULT_SUCCESS.get_inner_value() && fast_size < raw_size {
                rc = src_page_table.copy_memory_from_heap_to_heap(
                    dst_page_table,
                    KProcessAddress::new((dst_message_buffer + max_fast_size) as u64),
                    raw_size - fast_size,
                    KMemoryState::FLAG_REFERENCE_COUNTED,
                    KMemoryState::FLAG_REFERENCE_COUNTED,
                    KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                    KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
                    KMemoryAttribute::LOCKED,
                    KProcessAddress::new((src_message_buffer + max_fast_size) as u64),
                    src_state,
                    src_state,
                    src_perm,
                    KMemoryAttribute::UNCACHED,
                    KMemoryAttribute::NONE,
                );
            }
            rc
        } else {
            src_page_table.copy_memory_from_linear_to_user(
                KProcessAddress::new((dst_message_buffer + offset_words) as u64),
                raw_size,
                KProcessAddress::new((src_message_buffer + offset_words) as u64),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ,
                KMemoryAttribute::UNCACHED,
                KMemoryAttribute::NONE,
            )
        };

        if result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            dst_words[raw_data_index..raw_end].copy_from_slice(&src_words[raw_data_index..raw_end]);
        }

        result
    }

    fn cleanup_special_data(
        dst_process: &mut crate::hle::kernel::k_process::KProcess,
        dst_words: &mut [u32],
        dst_buffer_size: usize,
    ) {
        if dst_words.len() < 2 {
            return;
        }

        let header = crate::hle::kernel::message_buffer::MessageHeader::from_raw([
            dst_words[0],
            dst_words[1],
        ]);
        let special_header = if header.get_has_special_header() && dst_words.len() > 2 {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(
                (dst_words[2] & 1) != 0,
                ((dst_words[2] >> 1) & 0xF) as i32,
                ((dst_words[2] >> 5) & 0xF) as i32,
                true,
            )
        } else {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(false, 0, 0, false)
        };

        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &header,
            &special_header,
        ) > dst_buffer_size
        {
            return;
        }

        if !header.get_has_special_header() {
            return;
        }

        let mut message = MessageBuffer::new(dst_words);
        let mut offset = crate::hle::kernel::message_buffer::MessageBuffer::get_special_data_index(
            &special_header,
        );
        if special_header.get_has_process_id() {
            offset = message.set_process_id(offset, 0);
        }
        let handle_count =
            special_header.get_copy_handle_count() + special_header.get_move_handle_count();
        for _ in 0..handle_count {
            let handle = message.get_handle(offset);
            if handle != INVALID_HANDLE {
                dst_process.handle_table.remove(handle);
            }
            offset = message.set_handle(offset, INVALID_HANDLE);
        }
    }

    fn restore_destination_header(
        dst_words: &mut [u32],
        dst_header: &crate::hle::kernel::message_buffer::MessageHeader,
        dst_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
    ) {
        if dst_words.len() < 2 {
            return;
        }
        dst_words[0] = dst_header.get_data()[0];
        dst_words[1] = dst_header.get_data()[1];
        if dst_header.get_has_special_header() && dst_words.len() > 2 {
            dst_words[2] = (dst_special_header.get_has_process_id() as u32)
                | ((dst_special_header.get_copy_handle_count() as u32) << 1)
                | ((dst_special_header.get_move_handle_count() as u32) << 5);
        }
    }

    fn set_message_header_and_special_header(
        dst_words: &mut [u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
    ) {
        if dst_words.len() < 2 {
            return;
        }

        let mut message = MessageBuffer::new(dst_words);
        let _ = message.set_message_header(src_header);
        if src_header.get_has_special_header() && message.get_buffer_size() > 2 {
            let _ = message.set(
                2,
                &[src_special_header.get_has_process_id() as u32
                    | ((src_special_header.get_copy_handle_count() as u32) << 1)
                    | ((src_special_header.get_move_handle_count() as u32) << 5)],
            );
        }
    }

    fn mark_receive_list_broken(current_end_offset: usize, dst_recv_list_idx: usize) -> bool {
        current_end_offset > dst_recv_list_idx
    }

    fn process_message_special_data(
        dst_words: &mut [u32],
        src_words: &[u32],
        src_header: &crate::hle::kernel::message_buffer::MessageHeader,
        src_special_header: &crate::hle::kernel::message_buffer::SpecialHeader,
        dst_process: &mut crate::hle::kernel::k_process::KProcess,
        src_process: &mut crate::hle::kernel::k_process::KProcess,
        move_handle_allowed: bool,
    ) -> u32 {
        if !src_header.get_has_special_header() {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let mut src_words = src_words.to_vec();
        let src_message = MessageBuffer::new(&mut src_words);
        let mut dst_message = MessageBuffer::new(dst_words);
        let mut offset = crate::hle::kernel::message_buffer::MessageBuffer::get_special_data_index(
            src_special_header,
        );

        if src_special_header.get_has_process_id() {
            offset = dst_message.set_process_id(offset, src_process.process_id);
        }

        for _ in 0..src_special_header.get_copy_handle_count() {
            let src_handle = src_message.get_handle(offset);
            let dst_handle = if src_handle != INVALID_HANDLE {
                let Some(object_id) = src_process.handle_table.get_object(src_handle) else {
                    if crate::hle::kernel::handle_forensics::enabled() {
                        eprintln!(
                            "[SPECIAL_DATA_FAIL] kind=copy src_handle=0x{:X} src_pid={} dst_pid={}",
                            src_handle, src_process.process_id, dst_process.process_id
                        );
                        crate::hle::kernel::handle_forensics::dump_for_handle(src_handle as u64);
                    }
                    return crate::hle::kernel::svc::svc_results::RESULT_INVALID_HANDLE
                        .get_inner_value();
                };
                match dst_process.handle_table.add(object_id) {
                    Ok(handle) => handle,
                    Err(rc) => return rc,
                }
            } else {
                INVALID_HANDLE
            };
            offset = dst_message.set_handle(offset, dst_handle);
        }

        if src_special_header.get_move_handle_count() != 0 && !move_handle_allowed {
            return RESULT_INVALID_COMBINATION.get_inner_value();
        }

        for _ in 0..src_special_header.get_move_handle_count() {
            let src_handle = src_message.get_handle(offset);
            let dst_handle = if src_handle != INVALID_HANDLE {
                let Some(object_id) = src_process.handle_table.get_object(src_handle) else {
                    if crate::hle::kernel::handle_forensics::enabled() {
                        eprintln!(
                            "[SPECIAL_DATA_FAIL] kind=move src_handle=0x{:X} src_pid={} dst_pid={}",
                            src_handle, src_process.process_id, dst_process.process_id
                        );
                        crate::hle::kernel::handle_forensics::dump_for_handle(src_handle as u64);
                    }
                    return crate::hle::kernel::svc::svc_results::RESULT_INVALID_HANDLE
                        .get_inner_value();
                };
                let dst_handle = match dst_process.handle_table.add(object_id) {
                    Ok(handle) => handle,
                    Err(rc) => return rc,
                };
                src_process.handle_table.remove(src_handle);
                dst_handle
            } else {
                INVALID_HANDLE
            };
            offset = dst_message.set_handle(offset, dst_handle);
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn message_words_from_process(
        process: &crate::hle::kernel::k_process::KProcess,
        message_address: usize,
        buffer_size: usize,
    ) -> Result<Vec<u32>, u32> {
        let page_table = process.page_table.get_base();
        let bytes = if page_table.has_own_page_table_memory() {
            let Some(bytes) =
                page_table.read_block_from_own_page_table(message_address, buffer_size)
            else {
                return Err(RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
            };
            bytes
        } else if let Some(memory) = process.get_memory() {
            let mut bytes = vec![0u8; buffer_size];
            if !memory
                .lock()
                .unwrap()
                .read_block(message_address as u64, &mut bytes)
            {
                return Err(RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
            }
            bytes
        } else {
            let memory = process.process_memory.read().unwrap();
            memory.read_bytes(message_address as u64, buffer_size)
        };

        let mut words = vec![0u32; buffer_size / std::mem::size_of::<u32>()];
        for (index, chunk) in bytes.chunks_exact(4).enumerate() {
            words[index] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Ok(words)
    }

    fn message_words_from_process_physical(
        process: &crate::hle::kernel::k_process::KProcess,
        message_paddr: u64,
        buffer_size: usize,
    ) -> Option<Vec<u32>> {
        let memory = process.get_memory()?;
        let mut bytes = vec![0u8; buffer_size];
        if !memory
            .lock()
            .unwrap()
            .read_phys_block(message_paddr, &mut bytes)
        {
            return None;
        }

        let mut words = vec![0u32; buffer_size / std::mem::size_of::<u32>()];
        for (index, chunk) in bytes.chunks_exact(4).enumerate() {
            words[index] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Some(words)
    }

    fn write_message_words_to_process(
        process: &mut crate::hle::kernel::k_process::KProcess,
        message_address: usize,
        words: &[u32],
        byte_size: usize,
    ) -> bool {
        let bytes: Vec<u8> = words[..byte_size / std::mem::size_of::<u32>()]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();

        let page_table = process.page_table.get_base();
        if page_table.has_own_page_table_memory() {
            return page_table.write_block_to_own_page_table(message_address, &bytes);
        }

        if let Some(memory) = process.get_memory() {
            return memory
                .lock()
                .unwrap()
                .write_block(message_address as u64, &bytes);
        }

        process
            .process_memory
            .write()
            .unwrap()
            .write_block(message_address as u64, &bytes);
        true
    }

    fn write_message_words_to_process_physical(
        process: &crate::hle::kernel::k_process::KProcess,
        message_paddr: u64,
        words: &[u32],
        byte_size: usize,
    ) -> bool {
        let Some(memory) = process.get_memory() else {
            return false;
        };
        let bytes: Vec<u8> = words[..byte_size / std::mem::size_of::<u32>()]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        let written = memory
            .lock()
            .unwrap()
            .write_phys_block(message_paddr, &bytes);
        written
    }

    fn parse_message_headers(
        words: &[u32],
    ) -> (
        crate::hle::kernel::message_buffer::MessageHeader,
        crate::hle::kernel::message_buffer::SpecialHeader,
    ) {
        let header =
            crate::hle::kernel::message_buffer::MessageHeader::from_raw([words[0], words[1]]);
        let special_header = if header.get_has_special_header() && words.len() > 2 {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(
                (words[2] & 1) != 0,
                ((words[2] >> 1) & 0xF) as i32,
                ((words[2] >> 5) & 0xF) as i32,
                true,
            )
        } else {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(false, 0, 0, false)
        };
        (header, special_header)
    }

    fn request_message_address_and_size(
        thread: &crate::hle::kernel::k_thread::KThread,
        requested_message: usize,
        requested_size: usize,
    ) -> (usize, usize) {
        if requested_message != 0 {
            (requested_message, requested_size)
        } else {
            (
                thread.get_tls_address().get() as usize,
                MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
            )
        }
    }

    fn try_receive_message_raw(
        server_message: usize,
        server_buffer_size: usize,
        server_message_paddr: u64,
        request: &Arc<Mutex<KSessionRequest>>,
    ) -> Option<(u32, bool)> {
        let current_thread = crate::hle::kernel::kernel::get_current_thread_pointer()?;
        let server_process = {
            let thread = current_thread.lock().unwrap();
            thread.parent.as_ref()?.upgrade()?
        };
        let client_thread = Self::resolve_request_client_thread(request)?;
        let client_process = {
            let thread = client_thread.lock().unwrap();
            thread.parent.as_ref()?.upgrade()?
        };

        let (dst_address, dst_size, dst_user) = {
            let thread = current_thread.lock().unwrap();
            let (address, size) =
                Self::request_message_address_and_size(&thread, server_message, server_buffer_size);
            (address, size, server_message != 0)
        };
        let (src_address, src_size, src_user) = {
            let request = request.lock().unwrap();
            if request.get_address() != 0 {
                (request.get_address(), request.get_size(), true)
            } else {
                let thread = client_thread.lock().unwrap();
                let (address, size) = Self::request_message_address_and_size(&thread, 0, 0);
                (address, size, false)
            }
        };

        let src_words = {
            let process = client_process.lock().unwrap();
            match Self::message_words_from_process(&process, src_address, src_size) {
                Ok(words) => words,
                Err(rc) => return Some((rc, false)),
            }
        };
        let dst_word_capacity = dst_size / std::mem::size_of::<u32>();
        let original_dst_words = {
            let process = server_process.lock().unwrap();
            if dst_user && server_message_paddr != 0 {
                match Self::message_words_from_process_physical(
                    &process,
                    server_message_paddr,
                    dst_size,
                ) {
                    Some(words) => words,
                    None => return Some((RESULT_INVALID_CURRENT_MEMORY.get_inner_value(), false)),
                }
            } else {
                match Self::message_words_from_process(&process, dst_address, dst_size) {
                    Ok(words) => words,
                    Err(rc) => return Some((rc, false)),
                }
            }
        };
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let (dst_header, dst_special_header) = Self::parse_message_headers(&original_dst_words);

        let dst_recv_list_idx =
            crate::hle::kernel::message_buffer::MessageBuffer::get_receive_list_index(
                &dst_header,
                &dst_special_header,
            );
        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &src_header,
            &src_special_header,
        ) > src_size
        {
            return Some((RESULT_INVALID_COMBINATION.get_inner_value(), false));
        }
        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &dst_header,
            &dst_special_header,
        ) > dst_size
        {
            return Some((RESULT_INVALID_COMBINATION.get_inner_value(), false));
        }
        if src_header.get_has_special_header() && src_special_header.get_move_handle_count() != 0 {
            return Some((RESULT_INVALID_COMBINATION.get_inner_value(), false));
        }
        if dst_header.get_receive_list_offset() != 0
            && (dst_header.get_receive_list_offset() as usize)
                < crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
                    &dst_header,
                    &dst_special_header,
                ) + dst_header.get_raw_count() as usize
        {
            return Some((RESULT_INVALID_COMBINATION.get_inner_value(), false));
        }
        let src_end_offset = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            &src_header,
            &src_special_header,
        ) + src_header.get_raw_count() as usize;
        let copy_size = src_end_offset * std::mem::size_of::<u32>();
        if dst_size < copy_size {
            return Some((RESULT_MESSAGE_TOO_LARGE.get_inner_value(), false));
        }
        let mut recv_list_broken = false;

        let mut dst_words = original_dst_words;
        if dst_words.len() < dst_word_capacity {
            dst_words.resize(dst_word_capacity, 0);
        }
        Self::set_message_header_and_special_header(
            &mut dst_words,
            &src_header,
            &src_special_header,
        );
        let mut server_process = server_process.lock().unwrap();
        let mut client_process = client_process.lock().unwrap();
        let processed_special_data = src_header.get_has_special_header();
        let special_result = Self::process_message_special_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &mut server_process,
            &mut client_process,
            false,
        );
        if processed_special_data {
            recv_list_broken = Self::mark_receive_list_broken(
                crate::hle::kernel::message_buffer::MessageBuffer::get_pointer_descriptor_index(
                    &src_header,
                    &src_special_header,
                ),
                dst_recv_list_idx,
            );
        }
        if special_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut server_process, &mut dst_words, dst_size);
            }
            if !recv_list_broken {
                Self::restore_destination_header(&mut dst_words, &dst_header, &dst_special_header);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some((special_result, recv_list_broken));
        }
        let pointer_result = Self::process_receive_message_pointer_descriptors(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &dst_header,
            &dst_special_header,
            dst_address,
            dst_size,
            dst_user,
            &server_process.page_table,
            &client_process.page_table,
        );
        if src_header.get_pointer_count() > 0 {
            recv_list_broken = Self::mark_receive_list_broken(
                crate::hle::kernel::message_buffer::MessageBuffer::get_map_alias_descriptor_index(
                    &src_header,
                    &src_special_header,
                ),
                dst_recv_list_idx,
            );
        }
        if pointer_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut server_process, &mut dst_words, dst_size);
            }
            if !recv_list_broken {
                Self::restore_destination_header(&mut dst_words, &dst_header, &dst_special_header);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some((pointer_result, recv_list_broken));
        }
        let map_alias_result = {
            let current_request = request.lock().unwrap();
            drop(current_request);
            let mut request = request.lock().unwrap();
            Self::process_receive_message_map_alias_descriptors(
                &mut dst_words,
                &src_words,
                &src_header,
                &src_special_header,
                &mut request,
                &mut server_process.page_table,
                &mut client_process.page_table,
            )
        };
        if src_header.get_map_alias_count() > 0 {
            recv_list_broken = Self::mark_receive_list_broken(
                crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
                    &src_header,
                    &src_special_header,
                ),
                dst_recv_list_idx,
            );
        }
        if map_alias_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut server_process, &mut dst_words, dst_size);
            }
            if !recv_list_broken {
                Self::restore_destination_header(&mut dst_words, &dst_header, &dst_special_header);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some((map_alias_result, recv_list_broken));
        }
        let raw_result = Self::process_receive_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &server_process.page_table,
            dst_address,
            dst_user,
            &client_process.page_table,
            src_address,
            src_user,
        );
        if raw_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut server_process, &mut dst_words, dst_size);
            }
            if !recv_list_broken {
                Self::restore_destination_header(&mut dst_words, &dst_header, &dst_special_header);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some((raw_result, recv_list_broken));
        }
        if dst_user && server_message_paddr != 0 {
            if !Self::write_message_words_to_process_physical(
                &server_process,
                server_message_paddr,
                &dst_words,
                copy_size,
            ) {
                return Some((
                    RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    recv_list_broken,
                ));
            }
        } else {
            if !Self::write_message_words_to_process(
                &mut server_process,
                dst_address,
                &dst_words,
                copy_size,
            ) {
                return Some((
                    RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    recv_list_broken,
                ));
            }
        }
        if src_header.get_raw_count() > 0 {
            recv_list_broken = Self::mark_receive_list_broken(src_end_offset, dst_recv_list_idx);
        }
        Some((
            crate::hle::result::RESULT_SUCCESS.get_inner_value(),
            recv_list_broken,
        ))
    }

    fn try_send_message_raw(
        server_message: usize,
        server_buffer_size: usize,
        request: &Arc<Mutex<KSessionRequest>>,
    ) -> Option<u32> {
        let current_thread = crate::hle::kernel::kernel::get_current_thread_pointer()?;
        let server_process = {
            let thread = current_thread.lock().unwrap();
            thread.parent.as_ref()?.upgrade()?
        };
        let client_thread = Self::resolve_request_client_thread(request)?;
        let client_process = {
            let thread = client_thread.lock().unwrap();
            thread.parent.as_ref()?.upgrade()?
        };

        let (src_address, src_size, src_user) = {
            let thread = current_thread.lock().unwrap();
            let (address, size) =
                Self::request_message_address_and_size(&thread, server_message, server_buffer_size);
            (address, size, server_message != 0)
        };
        let (dst_address, dst_size, dst_user) = {
            let request = request.lock().unwrap();
            if request.get_address() != 0 {
                (request.get_address(), request.get_size(), true)
            } else {
                let thread = client_thread.lock().unwrap();
                let (address, size) = Self::request_message_address_and_size(&thread, 0, 0);
                (address, size, false)
            }
        };

        let src_words = {
            let process = server_process.lock().unwrap();
            match Self::message_words_from_process(&process, src_address, src_size) {
                Ok(words) => words,
                Err(rc) => return Some(rc),
            }
        };
        let dst_word_capacity = dst_size / std::mem::size_of::<u32>();
        let original_dst_words = {
            let process = client_process.lock().unwrap();
            match Self::message_words_from_process(&process, dst_address, dst_size) {
                Ok(words) => words,
                Err(rc) => return Some(rc),
            }
        };
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let (dst_header, dst_special_header) = Self::parse_message_headers(&original_dst_words);

        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &src_header,
            &src_special_header,
        ) > src_size
        {
            return Some(RESULT_INVALID_COMBINATION.get_inner_value());
        }
        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &dst_header,
            &dst_special_header,
        ) > dst_size
        {
            return Some(RESULT_INVALID_COMBINATION.get_inner_value());
        }
        if src_header.get_send_count() != 0
            || src_header.get_receive_count() != 0
            || src_header.get_exchange_count() != 0
        {
            return Some(RESULT_INVALID_COMBINATION.get_inner_value());
        }
        if dst_header.get_receive_list_offset() != 0
            && (dst_header.get_receive_list_offset() as usize)
                < crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
                    &dst_header,
                    &dst_special_header,
                ) + dst_header.get_raw_count() as usize
        {
            return Some(RESULT_INVALID_COMBINATION.get_inner_value());
        }
        let src_end_offset = crate::hle::kernel::message_buffer::MessageBuffer::get_raw_data_index(
            &src_header,
            &src_special_header,
        ) + src_header.get_raw_count() as usize;
        let copy_size = src_end_offset * std::mem::size_of::<u32>();
        if dst_size < copy_size {
            return Some(RESULT_MESSAGE_TOO_LARGE.get_inner_value());
        }

        let mut dst_words = original_dst_words;
        if dst_words.len() < dst_word_capacity {
            dst_words.resize(dst_word_capacity, 0);
        }
        Self::set_message_header_and_special_header(
            &mut dst_words,
            &src_header,
            &src_special_header,
        );
        let mut client_process = client_process.lock().unwrap();
        let mut server_process = server_process.lock().unwrap();
        let processed_special_data = src_header.get_has_special_header();
        let special_result = Self::process_message_special_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &mut client_process,
            &mut server_process,
            true,
        );
        if special_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some(special_result);
        }
        let pointer_result = Self::process_send_message_pointer_descriptors(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &dst_header,
            &dst_special_header,
            dst_address,
            dst_size,
            dst_user,
            &client_process.page_table,
            &server_process.page_table,
        );
        if pointer_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some(pointer_result);
        }
        let receive_mapping_result = {
            let request_guard = request.lock().unwrap();
            Self::process_send_message_receive_mappings(
                &request_guard,
                &server_process.page_table,
                &client_process.page_table,
            )
        };
        if receive_mapping_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some(receive_mapping_result);
        }
        {
            let map_alias_index =
                crate::hle::kernel::message_buffer::MessageBuffer::get_map_alias_descriptor_index(
                    &src_header,
                    &src_special_header,
                );
            for i in 0..src_header.get_map_alias_count() {
                let cur_offset = map_alias_index
                    + i as usize
                        * (crate::hle::kernel::message_buffer::MapAliasDescriptor::DATA_SIZE
                            / std::mem::size_of::<u32>());
                let mut dst_message = MessageBuffer::new(&mut dst_words);
                dst_message.set(cur_offset, &[0, 0, 0]);
            }
        }
        let raw_result = Self::process_send_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &client_process.page_table,
            dst_address,
            dst_user,
            src_address,
            src_user,
        );
        if raw_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
            }
            let _ = Self::cleanup_server_map(request, Some(&mut server_process));
            let _ = Self::cleanup_client_map(request, Some(&mut client_process));
            return Some(raw_result);
        }
        if !Self::write_message_words_to_process(
            &mut client_process,
            dst_address,
            &dst_words,
            copy_size,
        ) {
            return Some(RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
        }
        let cleanup_server_map = Self::cleanup_server_map(request, Some(&mut server_process));
        if cleanup_server_map != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
                let _ = Self::write_message_words_to_process(
                    &mut client_process,
                    dst_address,
                    &dst_words,
                    copy_size,
                );
            }
            return Some(cleanup_server_map);
        }
        let cleanup_client_map = Self::cleanup_client_map(request, Some(&mut client_process));
        if cleanup_client_map != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            if processed_special_data {
                Self::cleanup_special_data(&mut client_process, &mut dst_words, dst_size);
                let _ = Self::write_message_words_to_process(
                    &mut client_process,
                    dst_address,
                    &dst_words,
                    copy_size,
                );
            }
            return Some(cleanup_client_map);
        }
        Some(crate::hle::result::RESULT_SUCCESS.get_inner_value())
    }

    fn cleanup_server_handles(message: usize, mut buffer_size: usize, message_paddr: u64) -> u32 {
        let Some(thread) = crate::hle::kernel::kernel::get_current_thread_pointer() else {
            return RESULT_INVALID_STATE.get_inner_value();
        };

        let (process, tls_address) = {
            let thread = thread.lock().unwrap();
            let Some(process) = thread.parent.as_ref().and_then(|process| process.upgrade()) else {
                return RESULT_INVALID_STATE.get_inner_value();
            };
            (process, thread.get_tls_address().get() as usize)
        };

        let message_address = if message != 0 { message } else { tls_address };
        if message == 0 {
            buffer_size = MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>();
        }

        let mut words = {
            let process = process.lock().unwrap();
            if message != 0 && message_paddr != 0 {
                let Some(words) =
                    Self::message_words_from_process_physical(&process, message_paddr, buffer_size)
                else {
                    return RESULT_INVALID_STATE.get_inner_value();
                };
                words
            } else {
                match Self::message_words_from_process(&process, message_address, buffer_size) {
                    Ok(words) => words,
                    Err(_) => return RESULT_INVALID_STATE.get_inner_value(),
                }
            }
        };

        let header =
            crate::hle::kernel::message_buffer::MessageHeader::from_raw([words[0], words[1]]);
        let special_header = if header.get_has_special_header() && words.len() > 2 {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(
                (words[2] & 1) != 0,
                ((words[2] >> 1) & 0xF) as i32,
                ((words[2] >> 5) & 0xF) as i32,
                true,
            )
        } else {
            crate::hle::kernel::message_buffer::SpecialHeader::new_with_flag(false, 0, 0, false)
        };

        if crate::hle::kernel::message_buffer::MessageBuffer::get_message_buffer_size(
            &header,
            &special_header,
        ) > buffer_size
        {
            return RESULT_INVALID_COMBINATION.get_inner_value();
        }

        if header.get_has_special_header() {
            let message = MessageBuffer::new(&mut words);
            let mut offset =
                crate::hle::kernel::message_buffer::MessageBuffer::get_special_data_index(
                    &special_header,
                );
            if special_header.get_has_process_id() {
                offset += std::mem::size_of::<u64>() / std::mem::size_of::<u32>();
            }
            if special_header.get_copy_handle_count() > 0 {
                offset += (std::mem::size_of::<crate::hle::kernel::svc_common::Handle>()
                    * special_header.get_copy_handle_count() as usize)
                    / std::mem::size_of::<u32>();
            }

            let mut process = process.lock().unwrap();
            for _ in 0..special_header.get_move_handle_count() {
                let handle = message.get_handle(offset);
                if handle != INVALID_HANDLE {
                    process.handle_table.remove(handle);
                }
                offset += std::mem::size_of::<crate::hle::kernel::svc_common::Handle>()
                    / std::mem::size_of::<u32>();
            }
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn resolve_request_server_process(
        request: &Arc<Mutex<KSessionRequest>>,
    ) -> Option<Arc<ProcessLock>> {
        let server_process_id = request.lock().unwrap().get_server_process_id()?;
        let kernel = crate::hle::kernel::kernel::get_kernel_ref()?;
        kernel.get_process_by_id(server_process_id)
    }

    fn resolve_request_client_process(
        request: &Arc<Mutex<KSessionRequest>>,
    ) -> Option<Arc<ProcessLock>> {
        let client_process_id = request.lock().unwrap().get_client_process_id()?;
        let kernel = crate::hle::kernel::kernel::get_kernel_ref()?;
        kernel.get_process_by_id(client_process_id)
    }

    fn cleanup_server_map(
        request: &Arc<Mutex<KSessionRequest>>,
        server_process: Option<&mut crate::hle::kernel::k_process::KProcess>,
    ) -> u32 {
        let Some(server_process) = server_process else {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        };

        let request = request.lock().unwrap();
        for index in 0..request.mappings.get_send_count() {
            let mapping = request.mappings.get_send_mapping(index);
            let rc = server_process.page_table.cleanup_for_ipc_server(
                mapping.server_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        for index in 0..request.mappings.get_receive_count() {
            let mapping = request.mappings.get_receive_mapping(index);
            let rc = server_process.page_table.cleanup_for_ipc_server(
                mapping.server_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        for index in 0..request.mappings.get_exchange_count() {
            let mapping = request.mappings.get_exchange_mapping(index);
            let rc = server_process.page_table.cleanup_for_ipc_server(
                mapping.server_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn cleanup_client_map(
        request: &Arc<Mutex<KSessionRequest>>,
        client_process: Option<&mut crate::hle::kernel::k_process::KProcess>,
    ) -> u32 {
        let Some(client_process) = client_process else {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        };

        let request = request.lock().unwrap();
        for index in 0..request.mappings.get_send_count() {
            let mapping = request.mappings.get_send_mapping(index);
            let rc = client_process.page_table.cleanup_for_ipc_client(
                mapping.client_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        for index in 0..request.mappings.get_receive_count() {
            let mapping = request.mappings.get_receive_mapping(index);
            let rc = client_process.page_table.cleanup_for_ipc_client(
                mapping.client_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }
        for index in 0..request.mappings.get_exchange_count() {
            let mapping = request.mappings.get_exchange_mapping(index);
            let rc = client_process.page_table.cleanup_for_ipc_client(
                mapping.client_address,
                mapping.size,
                mapping.state,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn cleanup_map(request: &Arc<Mutex<KSessionRequest>>) -> u32 {
        if let Some(server_process) = Self::resolve_request_server_process(request) {
            let mut server_process = server_process.lock().unwrap();
            let rc = Self::cleanup_server_map(request, Some(&mut server_process));
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }
        }

        if let Some(client_process) = Self::resolve_request_client_process(request) {
            let mut client_process = client_process.lock().unwrap();
            return Self::cleanup_client_map(request, Some(&mut client_process));
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn clear_current_request_and_notify(&mut self) {
        if curreq_trace_enabled() {
            eprintln!(
                "[CURREQ] CLEAR session={:p} pending={}",
                self as *const Self,
                self.request_list.len()
            );
        }
        self.current_request = None;
        if !self.request_list.is_empty() {
            self.notify_available(crate::hle::result::RESULT_SUCCESS.get_inner_value());
        }
    }

    fn finalize_request(request: &Arc<Mutex<KSessionRequest>>) {
        request.lock().unwrap().finalize();
    }

    fn async_cleanup_result(cleanup_result: u32) -> ResultCode {
        if cleanup_result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            RESULT_SESSION_CLOSED
        } else {
            ResultCode::new(cleanup_result)
        }
    }

    fn complete_aborted_request(request: &Arc<Mutex<KSessionRequest>>, result: ResultCode) {
        let client_thread = Self::resolve_request_client_thread(request);
        let (event_id, client_process_id, client_message, client_buffer_size) = {
            let request = request.lock().unwrap();
            (
                request.get_event_id(),
                request.get_client_process_id(),
                request.get_address(),
                request.get_size(),
            )
        };

        if let Some(event_id) = event_id {
            Self::record_ipc_phase_count("reply_01_async_event");
            if let Some(client_process_id) = client_process_id {
                if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() {
                    if let Some(process_arc) = kernel.get_process_by_id(client_process_id) {
                        let mut process = process_arc.lock().unwrap();
                        Self::reply_async_error(
                            &mut process,
                            client_message,
                            client_buffer_size,
                            result,
                        );
                        let _ = process.page_table.unlock_for_ipc_user_buffer(
                            KProcessAddress::new(client_message as u64),
                            client_buffer_size,
                        );
                        if let Some(event) = process.get_event_by_object_id(event_id) {
                            let _ = event.lock().unwrap().signal(&process);
                        }
                    }
                }
            }
        } else if let Some(client_thread) = client_thread {
            let mut client_thread = client_thread.lock().unwrap();
            if !client_thread.is_termination_requested() {
                client_thread.end_wait(result.get_inner_value());
            }
        }
    }

    fn fail_receive_request(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
        result: ResultCode,
    ) -> u32 {
        self.fail_receive_request_with_server_result(request, result, result.get_inner_value())
    }

    fn fail_receive_request_with_server_result(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
        client_result: ResultCode,
        server_result: u32,
    ) -> u32 {
        self.clear_current_request_and_notify();
        Self::complete_aborted_request(&request, client_result);
        Self::finalize_request(&request);
        server_result
    }

    fn resolve_request_client_thread(
        request: &Arc<Mutex<KSessionRequest>>,
    ) -> Option<Arc<KThreadLock>> {
        request.lock().unwrap().get_thread()
    }

    fn reply_async_error(
        process: &mut crate::hle::kernel::k_process::KProcess,
        message_address: usize,
        _message_size: usize,
        result: ResultCode,
    ) {
        let mut words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut words);
            message.set_async_result(result);
        }

        let mut bytes = [0u8; 12];
        for (chunk, word) in bytes.chunks_exact_mut(4).zip(words[..3].iter().copied()) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        if let Some(memory) = process.get_memory() {
            memory
                .lock()
                .unwrap()
                .write_block(message_address as u64, &bytes);
        } else {
            process
                .process_memory
                .write()
                .unwrap()
                .write_block(message_address as u64, &bytes);
        }
    }

    pub fn new() -> Self {
        Self {
            parent_id: None,
            request_list: VecDeque::new(),
            current_request: None,
            client_closed: false,
            manager: None,
            sync_object: SynchronizationObjectState::new(),
            manager_wakeup: None,
            manager_close_queue: None,
            manager_close_wakeup: None,
        }
    }

    /// Wire the ServerManager's wakeup_event so that `notify_available` reacts
    /// in microseconds instead of the host-thread loop's 100 ms idle timeout.
    /// Called by `ServerManager::register_session`.
    pub fn set_manager_wakeup(&mut self, wakeup: std::sync::Weak<Event>) {
        self.manager_wakeup = Some(wakeup);
    }

    pub fn set_manager_close_notification(
        &mut self,
        queue: std::sync::Weak<Mutex<Vec<u64>>>,
        wakeup: std::sync::Weak<Event>,
    ) {
        self.manager_close_queue = Some(queue);
        self.manager_close_wakeup = Some(wakeup);
        if self.client_closed {
            self.notify_manager_closed();
        }
    }

    fn notify_manager_closed(&self) {
        let Some(parent_id) = self.parent_id else {
            return;
        };
        let Some(queue) = self
            .manager_close_queue
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        else {
            return;
        };
        let mut queue = queue.lock().unwrap();
        if !queue.contains(&parent_id) {
            queue.push(parent_id);
        }
        drop(queue);
        if let Some(wakeup) = self
            .manager_close_wakeup
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            wakeup.signal();
        }
    }

    /// Initialize with a parent session.
    pub fn initialize(&mut self, parent_id: u64) {
        self.parent_id = Some(parent_id);
        self.current_request = None;
        self.client_closed = false;
        self.request_list.clear();
        debug_assert!(self.sync_object.is_empty());
    }

    /// Set the session request manager.
    /// Called by ServerManager::RegisterSession to link the server session
    /// to its HLE handler.
    pub fn set_manager(&mut self, manager: Arc<Mutex<SessionRequestManager>>) {
        self.manager = Some(manager);
    }

    /// Get the session request manager.
    pub fn get_manager(&self) -> Option<&Arc<Mutex<SessionRequestManager>>> {
        self.manager.as_ref()
    }

    pub fn get_parent_id(&self) -> Option<u64> {
        self.parent_id
    }

    /// Is the server session signaled (has pending requests)?
    /// Matches upstream `KServerSession::IsSignaled`.
    pub fn is_signaled(&self) -> bool {
        self.client_closed || (!self.request_list.is_empty() && self.current_request.is_none())
    }

    /// Called when the client side is closed.
    /// Port of upstream `KServerSession::OnClientClosed`.
    /// Upstream signals the session and wakes waiting threads.
    ///
    /// Callers should migrate to `on_client_closed_with_process(...)` when they
    /// already hold the owner `KProcess`. This fallback path re-enters the
    /// kernel registry to rediscover the owner process and is unsafe under a
    /// held process lock.
    pub fn on_client_closed(&mut self) {
        self.on_client_closed_impl();
    }

    /// Port of upstream `KServerSession::OnClientClosed`, when the owner
    /// process is already known by the caller.
    pub fn on_client_closed_with_process(
        &mut self,
        _process: &mut crate::hle::kernel::k_process::KProcess,
    ) {
        self.on_client_closed_impl();
    }

    fn on_client_closed_impl(&mut self) {
        self.client_closed = true;

        // Eden keeps the request currently being dispatched in
        // `m_current_request`. If its client thread is terminating, only the
        // request's thread/event references are cleared. `SendReplyHLE` must
        // still consume the request and report ResultSessionClosed.
        if let Some(current_request) = self.current_request.as_ref() {
            let client_thread = Self::resolve_request_client_thread(current_request);
            if client_thread
                .as_ref()
                .is_some_and(|thread| thread.lock().unwrap().is_termination_requested())
            {
                let mut request = current_request.lock().unwrap();
                request.clear_thread();
                request.clear_event();
            }
        }

        // Pending requests have not been received by the server and therefore
        // have no IPC mappings. Eden only replies to asynchronous requests;
        // synchronous callers belong to the closing client process.
        while let Some(request) = self.request_list.pop_front() {
            if request.lock().unwrap().get_event_id().is_some() {
                Self::complete_aborted_request(&request, RESULT_SESSION_CLOSED);
            }
            Self::finalize_request(&request);
        }

        self.notify_available(RESULT_SESSION_CLOSED.get_inner_value());
        self.notify_manager_closed();
    }

    /// Enqueue a request.
    /// Port of upstream `KServerSession::OnRequest`.
    ///
    /// Callers should migrate to `on_request_with_process(...)` when they
    /// already hold the owner `KProcess`. This fallback path re-enters the
    /// kernel registry to rediscover the owner process and is unsafe under a
    /// held process lock.
    pub fn on_request(&mut self, request: Arc<Mutex<KSessionRequest>>) -> u32 {
        self.on_request_impl(request)
    }

    /// Port of upstream `KServerSession::OnRequest`, when the owner process is
    /// already known by the caller.
    pub fn on_request_with_process(
        &mut self,
        _process: &mut crate::hle::kernel::k_process::KProcess,
        request: Arc<Mutex<KSessionRequest>>,
    ) -> u32 {
        self.on_request_impl(request)
    }

    /// Host-thread IPC variant of `OnRequest`.
    ///
    /// Upstream drops `KScopedSchedulerLock` at the end of
    /// `KServerSession::OnRequest`, where no host mutex is held. The Rust port
    /// enters this method while holding `Mutex<KServerSession>`. For
    /// synchronous requests, the scheduler unlock may switch fibers; returning
    /// the guard lets `KSession` drop the server mutex first and then unlock
    /// scheduling, preserving upstream ordering without holding a Rust mutex
    /// across the fiber switch.
    pub fn on_request_defer_scheduler_unlock(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
    ) -> (
        u32,
        Option<super::k_scheduler_lock::KScopedSchedulerLock<'static>>,
    ) {
        self.on_request_impl_defer_scheduler_unlock(request)
    }

    fn on_request_impl(&mut self, request: Arc<Mutex<KSessionRequest>>) -> u32 {
        let _scheduler_lock = crate::hle::kernel::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);

        self.on_request_impl_locked(request)
    }

    fn on_request_impl_defer_scheduler_unlock(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
    ) -> (
        u32,
        Option<super::k_scheduler_lock::KScopedSchedulerLock<'static>>,
    ) {
        let profile_phases = std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some();
        let mut phase_last = profile_phases.then(std::time::Instant::now);
        let record_phase = |label: &'static str, last: &mut Option<std::time::Instant>| {
            if let Some(t) = last {
                crate::hle::kernel::svc::svc_ipc::record_ipc_phase(label, t.elapsed());
                *last = Some(std::time::Instant::now());
            }
        };

        let scheduler_lock = crate::hle::kernel::kernel::scheduler_lock()
            .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
        record_phase("on_request_defer_01_scheduler_lock", &mut phase_last);

        let result = self.on_request_impl_locked(request);
        record_phase("on_request_defer_02_impl_locked", &mut phase_last);
        if result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            (result, scheduler_lock)
        } else {
            (result, None)
        }
    }

    fn on_request_impl_locked(&mut self, request: Arc<Mutex<KSessionRequest>>) -> u32 {
        let profile_phases = std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some();
        let mut phase_last = profile_phases.then(std::time::Instant::now);
        let record_phase = |label: &'static str, last: &mut Option<std::time::Instant>| {
            if let Some(t) = last {
                crate::hle::kernel::svc::svc_ipc::record_ipc_phase(label, t.elapsed());
                *last = Some(std::time::Instant::now());
            }
        };

        if self.client_closed {
            return RESULT_SESSION_CLOSED.get_inner_value();
        }

        let (request_thread, is_sync_request) = {
            let request = request.lock().unwrap();
            (request.get_thread(), request.get_event_id().is_none())
        };
        record_phase("on_request_01_request_lock", &mut phase_last);

        if let Some(current_thread) = &request_thread {
            if current_thread.lock().unwrap().is_termination_requested() {
                return RESULT_TERMINATION_REQUESTED.get_inner_value();
            }
        }
        record_phase("on_request_02_termination_check", &mut phase_last);

        let was_empty = self.request_list.is_empty();
        self.request_list.push_back(request);
        record_phase("on_request_03_enqueue", &mut phase_last);
        if was_empty {
            self.notify_available(crate::hle::result::RESULT_SUCCESS.get_inner_value());
        }
        record_phase("on_request_04_notify_available", &mut phase_last);

        if is_sync_request {
            if let Some(current_thread) = request_thread {
                if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[39, 0]);
                }
                let mut thread = current_thread.lock().unwrap();
                record_phase("on_request_05_thread_lock", &mut phase_last);
                if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[40, 0]);
                }
                thread.set_wait_reason_for_debugging(
                    super::k_thread::ThreadWaitReasonForDebugging::Ipc,
                );
                record_phase("on_request_06_set_wait_reason", &mut phase_last);
                thread.begin_wait();
                record_phase("on_request_07_begin_wait", &mut phase_last);
                if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[41, 0]);
                }
            }
        }
        record_phase("on_request_08_done", &mut phase_last);

        0 // ResultSuccess
    }

    /// Send a reply to the current request.
    /// Port of upstream `KServerSession::SendReply`.
    ///
    /// Rust now keeps the `server_message` / `server_buffer_size` /
    /// `server_message_paddr` transport boundary in this owner file, even
    /// though the full upstream message-buffer translation still remains to be
    /// ported.
    pub fn send_reply_with_message(
        &mut self,
        server_message: usize,
        server_buffer_size: usize,
        _server_message_paddr: u64,
        is_hle: bool,
    ) -> u32 {
        let trace_reply = is_hle && std::env::var_os("RUZU_LOG_SYNC_REPLY").is_some();
        let Some(request) = self.current_request.take() else {
            return RESULT_INVALID_STATE.get_inner_value();
        };
        if trace_reply {
            log::info!("KServerSession::send_reply_with_message stage=took_current_request");
        }
        if self.current_request.is_none() {
            self.clear_current_request_and_notify();
        }
        if trace_reply {
            log::info!("KServerSession::send_reply_with_message stage=cleared_current_request");
        }

        let client_thread = Self::resolve_request_client_thread(&request);
        let (event_id, client_process_id, client_message, client_buffer_size) = {
            let request = request.lock().unwrap();
            (
                request.get_event_id(),
                request.get_client_process_id(),
                request.get_address(),
                request.get_size(),
            )
        };
        if trace_reply {
            log::info!(
                "KServerSession::send_reply_with_message stage=resolved_request client_thread={} event_id={:?} client_process_id={:?} addr={:#x} size={:#x}",
                client_thread.is_some(),
                event_id,
                client_process_id,
                client_message,
                client_buffer_size
            );
        }
        let closed = client_thread.is_none() || self.client_closed;

        let mut result = crate::hle::result::RESULT_SUCCESS.get_inner_value();
        if !closed && !is_hle {
            if let Some(send_result) =
                Self::try_send_message_raw(server_message, server_buffer_size, &request)
            {
                result = send_result;
            }
        } else if closed && !is_hle {
            result = Self::cleanup_server_handles(
                server_message,
                server_buffer_size,
                _server_message_paddr,
            );
            let cleanup_map_result = Self::cleanup_map(&request);
            if result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                result = cleanup_map_result;
            }
        }

        let mut client_result = result;
        if closed {
            if result == crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                result = RESULT_SESSION_CLOSED.get_inner_value();
                client_result = RESULT_SESSION_CLOSED.get_inner_value();
            } else {
                result = crate::hle::result::RESULT_SUCCESS.get_inner_value();
            }
        } else {
            result = crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }
        if trace_reply {
            log::info!(
                "KServerSession::send_reply_with_message stage=selected_results closed={} client_result={:#x} result={:#x}",
                closed,
                client_result,
                result
            );
        }

        if let Some(event_id) = event_id {
            if let Some(client_process_id) = client_process_id {
                if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() {
                    if let Some(process_arc) = kernel.get_process_by_id(client_process_id) {
                        let _lo_p = common::lock_order::guard("process");
                        let mut process = process_arc.lock().unwrap();
                        if client_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                            Self::reply_async_error(
                                &mut process,
                                client_message,
                                client_buffer_size,
                                ResultCode::new(client_result),
                            );
                        }
                        let _ = process.page_table.unlock_for_ipc_user_buffer(
                            KProcessAddress::new(client_message as u64),
                            client_buffer_size,
                        );
                        if let Some(event) = process.get_event_by_object_id(event_id) {
                            if trace_reply {
                                log::info!(
                                    "KServerSession::send_reply_with_message stage=signal_async_event"
                                );
                            }
                            let _ = event.lock().unwrap().signal(&process);
                        }
                    }
                }
            }
        } else if let Some(client_thread) = &client_thread {
            Self::record_ipc_phase_count("reply_02_sync_client");
            if trace_reply {
                log::info!(
                    "KServerSession::send_reply_with_message stage=before_lock_client_thread"
                );
            }
            // Take the kernel scheduler lock around the WAITING-state check
            // and the EndWait that flips it. `KThreadCell::lock()` is a no-op
            // (the docs explicitly require the scheduler spin-lock be held by
            // the caller as the real synchronization), so without it two host
            // fibers can both observe `state == WAITING` and both call
            // EndWait. The first EndWait sets state=RUNNABLE and clears
            // `wait_queue`; the second then re-enters `end_wait`, finds
            // state still cached as WAITING (atomic read may race), and asserts
            // `wait_queue is None while state=Waiting` — the lost-wakeup that
            // makes host-thread routing 1/16 unreliable.
            //
            // Upstream wraps this whole sequence in `KScopedSchedulerLock`
            // inside `KServerSession::SendReplyHLE`.
            let _scheduler_lock = crate::hle::kernel::kernel::scheduler_lock()
                .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
            let mut client_thread = client_thread.lock().unwrap();
            if common::trace::is_enabled(common::trace::cat::IPC_REPLY_WAKE) {
                static IPC_REPLY_WAKE_SEQ: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let seq = IPC_REPLY_WAKE_SEQ
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .wrapping_add(1);
                common::trace::emit_raw(
                    common::trace::cat::IPC_REPLY_WAKE,
                    &[
                        seq,
                        client_thread.get_thread_id(),
                        client_thread.get_state().bits() as u64,
                        client_thread.wait_reason_for_debugging as u64,
                        client_thread.wait_queue.is_some() as u64,
                        client_result as u64,
                        client_message as u64,
                        client_buffer_size as u64,
                    ],
                );
            }
            if trace_reply {
                log::info!(
                    "KServerSession::send_reply_with_message stage=locked_client_thread state={:?}",
                    client_thread.get_state()
                );
            }
            if !client_thread.is_termination_requested() {
                if trace_reply {
                    log::info!(
                        "KServerSession::send_reply_with_message stage=before_end_wait state={:?}",
                        client_thread.get_state()
                    );
                }
                Self::record_ipc_phase_count("reply_03_end_wait_call");
                client_thread.end_wait(client_result);
                if trace_reply {
                    log::info!("KServerSession::send_reply_with_message stage=after_end_wait");
                }
            } else if trace_reply {
                Self::record_ipc_phase_count("reply_05_skip_termination");
                log::info!(
                    "KServerSession::send_reply_with_message stage=skip_end_wait_terminating state={:?}",
                    client_thread.get_state(),
                );
            }
        }

        {
            let mut request = request.lock().unwrap();
            request.clear_thread();
            request.clear_event();
            request.finalize();
        }
        if trace_reply {
            log::info!("KServerSession::send_reply_with_message stage=finalized_request");
        }

        result
    }

    /// HLE convenience wrapper matching upstream `SendReplyHLE()`.
    pub fn send_reply(&mut self) -> u32 {
        self.send_reply_with_message(0, 0, 0, true)
    }

    /// HLE reply helper for host-thread ServerManager dispatch.
    ///
    /// Upstream `KServerSession::SendReplyHLE` holds the session light-lock
    /// while taking `KScopedSchedulerLock`, but `KServerSession::OnRequest`
    /// does not take that same light-lock. In the Rust port the outer
    /// `Mutex<KServerSession>` was standing in for both pieces of state, so
    /// holding it across `EndWait` created an ABBA with concurrent
    /// `SendSyncRequest`: reply path held `server_session` then waited for the
    /// scheduler lock, while a client held the scheduler lock then waited for
    /// `server_session`. Split the operation at the Rust mutex boundary: take
    /// the current request and compute the result under the session lock, then
    /// drop it before waking the client.
    pub fn send_reply_hle_unlocked(session: &Arc<Mutex<Self>>) -> u32 {
        let trace_reply = std::env::var_os("RUZU_LOG_SYNC_REPLY").is_some();
        let (
            request,
            client_thread,
            event_id,
            client_process_id,
            client_message,
            client_buffer_size,
            client_result,
            result,
        ) = {
            let mut server = session.lock().unwrap();
            let Some(request) = server.current_request.take() else {
                return RESULT_INVALID_STATE.get_inner_value();
            };
            if trace_reply {
                log::info!("KServerSession::send_reply_hle_unlocked stage=took_current_request");
            }
            if server.current_request.is_none() {
                server.clear_current_request_and_notify();
            }

            let client_thread = Self::resolve_request_client_thread(&request);
            let (event_id, client_process_id, client_message, client_buffer_size) = {
                let request = request.lock().unwrap();
                (
                    request.get_event_id(),
                    request.get_client_process_id(),
                    request.get_address(),
                    request.get_size(),
                )
            };
            let closed = client_thread.is_none() || server.client_closed;
            let mut result = crate::hle::result::RESULT_SUCCESS.get_inner_value();
            let mut client_result = result;
            if closed {
                result = RESULT_SESSION_CLOSED.get_inner_value();
                client_result = RESULT_SESSION_CLOSED.get_inner_value();
            }
            (
                request,
                client_thread,
                event_id,
                client_process_id,
                client_message,
                client_buffer_size,
                client_result,
                result,
            )
        };

        if let Some(event_id) = event_id {
            Self::record_ipc_phase_count("reply_hle_01_async_event");
            if let Some(client_process_id) = client_process_id {
                if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() {
                    if let Some(process_arc) = kernel.get_process_by_id(client_process_id) {
                        let _lo_p = common::lock_order::guard("process");
                        let mut process = process_arc.lock().unwrap();
                        if client_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                            Self::reply_async_error(
                                &mut process,
                                client_message,
                                client_buffer_size,
                                ResultCode::new(client_result),
                            );
                        }
                        let _ = process.page_table.unlock_for_ipc_user_buffer(
                            KProcessAddress::new(client_message as u64),
                            client_buffer_size,
                        );
                        if let Some(event) = process.get_event_by_object_id(event_id) {
                            let _ = event.lock().unwrap().signal(&process);
                        }
                    }
                }
            }
        } else if let Some(client_thread) = &client_thread {
            Self::record_ipc_phase_count("reply_hle_02_sync_client");
            let _scheduler_lock = crate::hle::kernel::kernel::scheduler_lock()
                .map(super::k_scheduler_lock::KScopedSchedulerLock::new);
            let mut client_thread = client_thread.lock().unwrap();
            if !client_thread.is_termination_requested() {
                Self::record_ipc_phase_count("reply_hle_03_end_wait_call");
                client_thread.end_wait(client_result);
            } else {
                Self::record_ipc_phase_count("reply_hle_05_skip_termination");
            }
        }

        {
            let mut request = request.lock().unwrap();
            request.clear_thread();
            request.clear_event();
            request.finalize();
        }
        if trace_reply {
            log::info!("KServerSession::send_reply_hle_unlocked stage=finalized_request");
        }

        result
    }

    /// Receive the next pending request.
    /// Port of upstream `KServerSession::ReceiveRequest`.
    ///
    /// Rust now keeps the `server_message` / `server_buffer_size` /
    /// `server_message_paddr` transport boundary in this owner file, even
    /// though the full upstream message-buffer translation and
    /// `HLERequestContext` construction still remain to be ported.
    pub fn receive_request_with_message(
        &mut self,
        server_message: usize,
        server_buffer_size: usize,
        _server_message_paddr: u64,
    ) -> u32 {
        if self.client_closed {
            return RESULT_SESSION_CLOSED.get_inner_value();
        }
        if self.current_request.is_some() {
            return RESULT_NOT_FOUND.get_inner_value();
        }

        let Some(request) = self.request_list.pop_front() else {
            return RESULT_NOT_FOUND.get_inner_value();
        };

        if Self::resolve_request_client_thread(&request).is_none() {
            return self.fail_receive_request(request, RESULT_SESSION_CLOSED);
        }

        if let Some(current_thread) = crate::hle::kernel::kernel::get_current_thread_pointer() {
            let process_id = current_thread
                .lock()
                .unwrap()
                .parent
                .as_ref()
                .and_then(|process| process.upgrade())
                .map(|process| process.lock().unwrap().process_id);
            if let Some(process_id) = process_id {
                request.lock().unwrap().set_server_process(process_id);
            }
        }
        self.current_request = Some(Arc::clone(&request));
        let (receive_result, recv_list_broken) = Self::try_receive_message_raw(
            server_message,
            server_buffer_size,
            _server_message_paddr,
            &request,
        )
        .unwrap_or((crate::hle::result::RESULT_SUCCESS.get_inner_value(), false));
        if receive_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            return self.fail_receive_request_with_server_result(
                request,
                ResultCode::new(receive_result),
                if recv_list_broken {
                    RESULT_RECEIVE_LIST_BROKEN.get_inner_value()
                } else {
                    RESULT_NOT_FOUND.get_inner_value()
                },
            );
        }

        self.current_request = Some(request);
        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    /// HLE receive path corresponding to upstream
    /// `ReceiveRequest(..., out_context, manager)`.
    pub fn receive_request_hle(
        &mut self,
        manager: Arc<Mutex<SessionRequestManager>>,
    ) -> Result<(HLERequestContext, Arc<Mutex<SessionRequestManager>>, u64), u32> {
        let trace = std::env::var_os("RUZU_TRACE_RECEIVE_REQUEST_HLE").is_some();
        if trace {
            eprintln!("[RECEIVE_HLE] enter");
        }
        if self.client_closed {
            return Err(RESULT_SESSION_CLOSED.get_inner_value());
        }
        if self.current_request.is_some() {
            return Err(RESULT_NOT_FOUND.get_inner_value());
        }

        let Some(current_request) = self.request_list.pop_front() else {
            return Err(RESULT_NOT_FOUND.get_inner_value());
        };
        if trace {
            eprintln!("[RECEIVE_HLE] popped_request");
        }
        let Some(client_thread) = Self::resolve_request_client_thread(&current_request) else {
            {
                let mut request = current_request.lock().unwrap();
                request.finalize();
            }
            if !self.request_list.is_empty() {
                self.notify_available(crate::hle::result::RESULT_SUCCESS.get_inner_value());
            }
            return Err(RESULT_SESSION_CLOSED.get_inner_value());
        };
        if trace {
            let tid = client_thread.lock().unwrap().thread_id;
            eprintln!("[RECEIVE_HLE] resolved_client_thread tid={}", tid);
        }

        self.current_request = Some(Arc::clone(&current_request));
        if curreq_trace_enabled() {
            eprintln!(
                "[CURREQ] SET session={:p} pending_after={}",
                self as *const Self,
                self.request_list.len()
            );
        }
        let request_message_address = {
            let request = current_request.lock().unwrap();
            let request_address = request.get_address() as u64;
            if request_address != 0 {
                request_address
            } else {
                client_thread.lock().unwrap().get_tls_address().get()
            }
        };
        if trace {
            eprintln!(
                "[RECEIVE_HLE] request_message_address=0x{:X}",
                request_message_address
            );
        }

        let mut context =
            HLERequestContext::new_with_thread(client_thread, request_message_address);
        if trace {
            eprintln!("[RECEIVE_HLE] context_created");
        }
        context.set_session_request_manager(Arc::clone(&manager));
        if trace {
            eprintln!("[RECEIVE_HLE] manager_set");
        }
        context.populate_from_incoming_command_buffer(&[]);
        if trace {
            eprintln!(
                "[RECEIVE_HLE] populated cmd={} type={:?}",
                context.get_command(),
                context.get_command_type()
            );
        }
        Ok((context, manager, request_message_address))
    }

    /// Rust inline-dispatch helper.
    ///
    /// Upstream never consumes a just-enqueued sync request on the caller's
    /// thread: `KClientSession::SendSyncRequest` parks the client and the
    /// owning `ServerManager` receives the request. Ruzu's legacy inline
    /// fallback is intentionally non-upstream, but it must not expose the
    /// request to the host `ServerManager` and then consume it itself. Doing so
    /// lets two dispatch paths race on the same session, which can leave an old
    /// SFCO reply in TLS to be parsed as a fresh request. Push the request
    /// without `NotifyAvailable`, then receive it immediately while the caller
    /// still holds the `KServerSession` mutex.
    pub fn receive_inline_request_hle(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
        manager: Arc<Mutex<SessionRequestManager>>,
    ) -> Result<(HLERequestContext, Arc<Mutex<SessionRequestManager>>, u64), u32> {
        self.enqueue_inline_request_hle(Arc::clone(&request))?;
        self.receive_queued_inline_request_hle(&request, &manager)
    }

    /// Queue an inline-dispatched request without notifying the owning
    /// ServerManager.
    ///
    /// Upstream `OnRequest` appends to `m_request_list` before the client
    /// waits, even when another request is currently being handled. The legacy
    /// inline path still must preserve that FIFO property; otherwise two guest
    /// threads sharing one service session can be reordered by host lock timing.
    pub fn enqueue_inline_request_hle(
        &mut self,
        request: Arc<Mutex<KSessionRequest>>,
    ) -> Result<(), u32> {
        if self.client_closed {
            return Err(RESULT_SESSION_CLOSED.get_inner_value());
        }
        self.request_list.push_back(request);
        Ok(())
    }

    /// Receive the next inline-dispatched request only when it is ready.
    ///
    /// Returns `ResultNotFound` while the session is busy or while an earlier
    /// queued request is still ahead of the caller. The caller can wait/retry
    /// without enqueueing duplicates.
    pub fn receive_queued_inline_request_hle(
        &mut self,
        request: &Arc<Mutex<KSessionRequest>>,
        manager: &Arc<Mutex<SessionRequestManager>>,
    ) -> Result<(HLERequestContext, Arc<Mutex<SessionRequestManager>>, u64), u32> {
        if self.client_closed {
            return Err(RESULT_SESSION_CLOSED.get_inner_value());
        }
        if self.current_request.is_some() || self.request_list.is_empty() {
            return Err(RESULT_NOT_FOUND.get_inner_value());
        }
        if !self
            .request_list
            .front()
            .is_some_and(|front| Arc::ptr_eq(front, request))
        {
            return Err(RESULT_NOT_FOUND.get_inner_value());
        }
        self.receive_request_hle(Arc::clone(manager))
    }

    /// HLE convenience wrapper matching upstream `ReceiveRequestHLE()`.
    pub fn receive_request(&mut self) -> u32 {
        self.receive_request_with_message(0, 0, 0)
    }

    pub fn get_current_request(&self) -> Option<Arc<Mutex<KSessionRequest>>> {
        self.current_request.clone()
    }

    /// Clean up pending requests.
    pub fn cleanup_requests(&mut self) {
        if let Some(request) = self.current_request.take() {
            let cleanup_result = Self::cleanup_map(&request);
            Self::complete_aborted_request(&request, Self::async_cleanup_result(cleanup_result));
            Self::finalize_request(&request);
        }
        while let Some(request) = self.request_list.pop_front() {
            let cleanup_result = Self::cleanup_map(&request);
            Self::complete_aborted_request(&request, Self::async_cleanup_result(cleanup_result));
            Self::finalize_request(&request);
        }
        self.request_list.clear();
    }

    /// Walk the waiter list and wake every thread waiting on this session.
    /// Mirrors upstream `KSynchronizationObject::NotifyAvailable`. Caller must
    /// hold the scheduler lock (the convention for every signal-style entry).
    fn notify_available(&self, result: u32) -> bool {
        let Some(object_id) = self.parent_id else {
            return false;
        };
        if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
            common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[27, object_id]);
        }
        let woke_any = unsafe {
            k_synchronization_object::notify_waiters_on_state(&self.sync_object, object_id, result)
        };
        if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
            common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[28, object_id]);
        }
        // Wake the owning ServerManager through its kernel-readable event too.
        // Normally the session object's waiter is enough. While a request is
        // being handled, however, ServerManager temporarily unlinks that
        // session holder before relinking it through the deferred list. A new
        // request in that interval has no session waiter to notify; signaling
        // only Event's host Condvar is insufficient because MultiWait blocks
        // on kernel synchronization objects.
        if let Some(weak) = self.manager_wakeup.as_ref() {
            if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[29, object_id]);
            }
            if let Some(event) = weak.upgrade() {
                if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[30, object_id]);
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[31, object_id]);
                }
                event.signal();
                if common::trace::is_enabled(common::trace::cat::HOST_THREAD_IPC) {
                    common::trace::emit_raw(common::trace::cat::HOST_THREAD_IPC, &[32, object_id]);
                }
            }
        }
        woke_any
    }

    /// Destroy the server session.
    pub fn destroy(&mut self) {
        let Some(parent_id) = self.parent_id else {
            self.cleanup_requests();
            return;
        };
        let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
            self.cleanup_requests();
            self.parent_id = None;
            return;
        };
        let Some(owner_process_id) = kernel.get_session_owner_process_id(parent_id) else {
            self.cleanup_requests();
            self.parent_id = None;
            return;
        };
        let Some(process) = kernel.get_process_by_id(owner_process_id) else {
            self.cleanup_requests();
            self.parent_id = None;
            return;
        };
        self.destroy_with_process(&mut process.lock().unwrap());
    }

    /// Port of upstream `KServerSession::Destroy` when the owner process is
    /// already locked by the caller.
    pub fn destroy_with_process(&mut self, process: &mut crate::hle::kernel::k_process::KProcess) {
        let Some(parent_id) = self.parent_id.take() else {
            self.cleanup_requests();
            return;
        };

        let should_finalize = process
            .get_session_by_object_id(parent_id)
            .is_some_and(|parent| {
                let mut parent = parent.lock().unwrap();
                parent.on_server_closed();
                parent.close_server_endpoint()
            });
        self.cleanup_requests();
        if should_finalize {
            process.unregister_session_object_by_object_id(parent_id);
        }
    }
}

impl Default for KServerSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::{dram_memory_map, DeviceMemory};
    use crate::hle::kernel::k_memory_block::{KMemoryAttribute, KMemoryBlockDisableMergeAttribute};
    use crate::hle::kernel::k_port::KPort;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_readable_event::KReadableEvent;
    use crate::hle::kernel::k_scheduler::KScheduler;
    use crate::hle::kernel::k_session::KSession;
    use crate::hle::kernel::k_session_request::KSessionRequest;
    use crate::hle::kernel::k_thread::{KThread, ThreadState, ThreadWaitReasonForDebugging};
    use crate::memory::memory::Memory;

    struct SessionPageTableMemoryForTest {
        _device_memory: Box<DeviceMemory>,
        memory: Arc<Mutex<Memory>>,
    }

    fn make_port_session(
        system: &mut crate::core::System,
    ) -> (
        Arc<crate::hle::kernel::k_process::ProcessLock>,
        Arc<Mutex<KPort>>,
        u64,
        Arc<Mutex<crate::hle::kernel::k_client_session::KClientSession>>,
        Arc<Mutex<KServerSession>>,
    ) {
        let process = Arc::new(crate::hle::kernel::k_process::ProcessLock::from_value(
            KProcess::new(),
        ));
        {
            let mut process = process.lock().unwrap();
            process.process_id = 1;
            process.initialize_handle_table();
        }
        system.set_current_process_arc(process.clone());

        let port_id = 0x1234;
        let port = Arc::new(Mutex::new(KPort::new()));
        port.lock().unwrap().initialize(1, false, 0x66);
        process
            .lock()
            .unwrap()
            .register_client_port_object(port_id, port.clone());

        let kernel = system.kernel().unwrap();
        let (session_id, client_id, server_session) = {
            let port = port.lock().unwrap();
            let (session_id, client_id) = port
                .client
                .create_session(
                    &mut process.lock().unwrap(),
                    kernel,
                    Some(port_id),
                    port.get_name(),
                )
                .unwrap();
            let server_session = process
                .lock()
                .unwrap()
                .get_server_session_by_object_id(session_id)
                .unwrap();
            (session_id, client_id, server_session)
        };

        let client_session = process
            .lock()
            .unwrap()
            .get_client_session_by_object_id(client_id)
            .unwrap();

        (process, port, session_id, client_session, server_session)
    }

    #[test]
    fn client_then_server_close_releases_parent_port_session_slot() {
        let mut system = crate::core::System::new_for_test();
        let (process, port, session_id, client_session, server_session) =
            make_port_session(&mut system);

        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 1);
        client_session
            .lock()
            .unwrap()
            .destroy_with_process(&mut process.lock().unwrap());
        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 1);
        server_session
            .lock()
            .unwrap()
            .destroy_with_process(&mut process.lock().unwrap());
        assert!(process
            .lock()
            .unwrap()
            .get_session_by_object_id(session_id)
            .is_none());
        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 0);
    }

    #[test]
    fn server_then_client_close_releases_parent_port_session_slot() {
        let mut system = crate::core::System::new_for_test();
        let (process, port, session_id, client_session, server_session) =
            make_port_session(&mut system);

        server_session
            .lock()
            .unwrap()
            .destroy_with_process(&mut process.lock().unwrap());
        assert!(process
            .lock()
            .unwrap()
            .get_session_by_object_id(session_id)
            .is_some());
        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 1);

        client_session
            .lock()
            .unwrap()
            .destroy_with_process(&mut process.lock().unwrap());
        assert!(process
            .lock()
            .unwrap()
            .get_session_by_object_id(session_id)
            .is_none());
        assert_eq!(port.lock().unwrap().client.get_num_sessions(), 0);
    }

    impl SessionPageTableMemoryForTest {
        fn new(backing_size: usize) -> Self {
            let device_memory = Box::new(DeviceMemory::with_size(backing_size));
            let buffer_ptr = &device_memory.buffer as *const common::host_memory::HostMemory;
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(
                    SystemRef::null(),
                    device_memory.as_ref() as *const _,
                    buffer_ptr,
                )
            }));

            Self {
                _device_memory: device_memory,
                memory,
            }
        }

        fn write_phys(&mut self, phys_addr: u64, bytes: &[u8]) {
            let offset = phys_addr
                .checked_sub(dram_memory_map::BASE)
                .expect("test physical address must be in dram") as usize;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self._device_memory
                        .buffer
                        .backing_base_pointer()
                        .add(offset),
                    bytes.len(),
                );
            }
        }

        fn read_phys(&self, phys_addr: u64, size: usize) -> Vec<u8> {
            let offset = phys_addr
                .checked_sub(dram_memory_map::BASE)
                .expect("test physical address must be in dram") as usize;
            let mut bytes = vec![0u8; size];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self._device_memory
                        .buffer
                        .backing_base_pointer()
                        .add(offset),
                    bytes.as_mut_ptr(),
                    size,
                );
            }
            bytes
        }
    }

    #[test]
    fn on_request_sync_parks_client_thread_for_ipc_wait() {
        let client_thread = Arc::new(KThreadLock::new(KThread::new()));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 0x55;
            thread.set_state(ThreadState::RUNNABLE);
        }
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&client_thread));

        let mut request = KSessionRequest::new();
        request.initialize(None, 0x2395000, 0x80);
        let request = Arc::new(Mutex::new(request));

        let mut server = KServerSession::new();
        assert_eq!(
            server.on_request(request),
            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        );

        {
            let thread = client_thread.lock().unwrap();
            assert_eq!(thread.get_state(), ThreadState::WAITING);
            assert_eq!(
                thread.get_wait_reason_for_debugging(),
                ThreadWaitReasonForDebugging::Ipc
            );
        }

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    fn attach_process_page_table_for_session_test(
        process: &mut KProcess,
        memory: Arc<Mutex<Memory>>,
        start: usize,
        end: usize,
        state: KMemoryState,
    ) {
        let page_table = process.page_table.get_base_mut();
        page_table.m_address_space_width = 32;
        page_table.m_address_space_start = start;
        page_table.m_address_space_end = end;
        page_table.m_memory = Some(memory);
        page_table.initialize_impl();
        page_table
            .m_memory_block_manager
            .initialize(start, end, None)
            .expect("test memory block manager initialization must succeed");
        page_table.m_memory_block_manager.update(
            start,
            (end - start) / PAGE_SIZE,
            state,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
    }

    fn map_process_page_for_session_test(process: &mut KProcess, addr: usize, phys_addr: u64) {
        let page_table = process.page_table.get_base_mut();
        let memory = page_table
            .m_memory
            .as_ref()
            .expect("test page table memory must be attached")
            .clone();
        let impl_pt = page_table
            .m_impl
            .as_mut()
            .expect("test page table backend must be initialized");
        memory.lock().unwrap().map_memory_region(
            impl_pt,
            addr as u64,
            PAGE_SIZE as u64,
            phys_addr,
            crate::memory::memory::MemoryPermission::READ_WRITE,
            false,
        );
    }

    fn set_current_page_table_for_session_test(process: &mut KProcess) {
        let page_table = process.page_table.get_base_mut();
        let memory = page_table
            .m_memory
            .as_ref()
            .expect("test page table memory must be attached")
            .clone();
        let impl_pt = page_table
            .m_impl
            .as_mut()
            .expect("test page table backend must be initialized");
        memory
            .lock()
            .unwrap()
            .set_current_page_table(impl_pt.as_mut() as *mut _, true);
    }

    fn bind_request_client_thread_for_session_test(
        request: &mut KSessionRequest,
        client_thread: &Arc<KThreadLock>,
    ) {
        let (thread_id, client_process_id) = {
            let thread = client_thread.lock().unwrap();
            (
                thread.thread_id,
                thread
                    .parent
                    .as_ref()
                    .and_then(|process| process.upgrade())
                    .map(|process| process.lock().unwrap().process_id),
            )
        };
        request.thread = Some(Arc::downgrade(client_thread));
        request.thread_id = Some(thread_id);
        request.client_process_id = client_process_id;
    }

    fn prepare_process_memory_for_session_test(
        process: &mut crate::hle::kernel::k_process::KProcess,
    ) {
        process.allocate_code_memory(0, 0x10000);
        process.process_memory.write().unwrap().allocate(0, 0x10000);
    }

    fn park_client_thread_for_ipc_session_test(client_thread: &Arc<KThreadLock>) {
        let mut thread = client_thread.lock().unwrap();
        thread.set_wait_reason_for_debugging(ThreadWaitReasonForDebugging::Ipc);
        thread.begin_wait();
    }

    #[test]
    fn message_words_use_process_page_table_not_memory_current_page_table() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x40000);
        let mut lhs = KProcess::new();
        let mut rhs = KProcess::new();
        attach_process_page_table_for_session_test(
            &mut lhs,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut rhs,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );

        let lhs_phys = dram_memory_map::BASE + 0x10000;
        let rhs_phys = dram_memory_map::BASE + 0x20000;
        map_process_page_for_session_test(&mut lhs, 0x4000, lhs_phys);
        map_process_page_for_session_test(&mut rhs, 0x4000, rhs_phys);
        set_current_page_table_for_session_test(&mut rhs);

        page_table_memory.write_phys(lhs_phys, &[1, 2, 3, 4, 5, 6, 7, 8]);
        page_table_memory.write_phys(rhs_phys, &[0xAA; 8]);

        let words = KServerSession::message_words_from_process(&lhs, 0x4000, 8).unwrap();
        assert_eq!(words, vec![0x0403_0201, 0x0807_0605]);

        assert!(KServerSession::write_message_words_to_process(
            &mut lhs,
            0x4000,
            &[0x1122_3344, 0x5566_7788],
            8,
        ));
        assert_eq!(
            page_table_memory.read_phys(lhs_phys, 8),
            vec![0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]
        );
        assert_eq!(page_table_memory.read_phys(rhs_phys, 8), vec![0xAA; 8]);
        assert!(!KServerSession::write_message_words_to_process(
            &mut lhs,
            0x9000,
            &[0xAABB_CCDD],
            4,
        ));
    }

    #[test]
    fn is_signaled_requires_no_current_request() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);

        assert!(!server.is_signaled());
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        server.on_request(request.clone());
        assert!(server.is_signaled());

        server.current_request = Some(request);
        assert!(!server.is_signaled());
    }

    #[test]
    fn is_signaled_when_client_closed() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);

        assert!(!server.is_signaled());
        server.on_client_closed();
        assert!(server.is_signaled());
    }

    #[test]
    fn client_close_preserves_active_request_for_session_closed_reply() {
        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        client_thread.lock().unwrap().request_terminate();

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        bind_request_client_thread_for_session_test(&mut request.lock().unwrap(), &client_thread);

        let server = Arc::new(Mutex::new(KServerSession::new()));
        {
            let mut server = server.lock().unwrap();
            server.initialize(0x1000);
            server.current_request = Some(Arc::clone(&request));
            server.on_client_closed();

            assert!(server.current_request.is_some());
            assert!(request.lock().unwrap().get_thread().is_none());
        }

        assert_eq!(
            KServerSession::send_reply_hle_unlocked(&server),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
        assert!(server.lock().unwrap().current_request.is_none());
    }

    #[test]
    fn request_signals_server_manager_kernel_wakeup_event() {
        let mut process = KProcess::new();
        process.process_id = 7;

        let event_id = 0x2000;
        let readable_id = 0x2001;
        let event_owner = Arc::new(Mutex::new(crate::hle::kernel::k_event::KEvent::new()));
        event_owner
            .lock()
            .unwrap()
            .initialize(process.process_id, readable_id);
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(event_id, readable_id);
        process.register_readable_event_object(readable_id, Arc::clone(&readable));

        let process = Arc::new(ProcessLock::from_value(process));
        let wakeup = Arc::new(Event::new_with_kernel_event(
            event_owner,
            Arc::clone(&readable),
            Arc::clone(&process),
        ));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.set_manager_wakeup(Arc::downgrade(&wakeup));
        server.on_request(Arc::new(Mutex::new(KSessionRequest::new())));

        assert!(wakeup.is_signaled());
        assert!(readable.lock().unwrap().is_signaled());
    }

    // `link_waiter_uses_parent_session_object_id` removed: the handle-indirect
    // `link_waiter(&mut KProcess, thread_id)` API was deleted as part of the
    // upstream-faithful sync-object refactor. The new intrusive-list path is
    // exercised by the KSynchronizationObject unit tests.

    #[test]
    fn cleanup_requests_finalizes_pending_and_current_requests() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);

        let pending = Arc::new(Mutex::new(KSessionRequest::new()));
        pending.lock().unwrap().thread_id = Some(10);
        let current = Arc::new(Mutex::new(KSessionRequest::new()));
        current.lock().unwrap().thread_id = Some(11);

        server.request_list.push_back(Arc::clone(&pending));
        server.current_request = Some(Arc::clone(&current));

        server.cleanup_requests();

        assert!(server.request_list.is_empty());
        assert!(server.current_request.is_none());
        assert_eq!(pending.lock().unwrap().get_thread_id(), None);
        assert_eq!(current.lock().unwrap().get_thread_id(), None);
    }

    #[test]
    fn send_reply_signals_async_request_event() {
        let mut process = KProcess::new();
        process.process_id = 7;
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let event_id = 0x4000;
        let readable_id = 0x4001;
        let event = Arc::new(Mutex::new(crate::hle::kernel::k_event::KEvent::new()));
        event
            .lock()
            .unwrap()
            .initialize(process.process_id, readable_id);
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(event_id, readable_id);
        process.register_event_object(event_id, Arc::clone(&event));
        process.register_readable_event_object(readable_id, Arc::clone(&readable));

        let mut system = crate::core::System::new_for_test();
        system.set_scheduler_arc(Arc::clone(&scheduler));
        system.set_current_process_arc(Arc::new(ProcessLock::from_value(process)));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            request_guard.event_id = Some(event_id);
            request_guard.client_process_id = Some(7);
        }
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0x2000, 0x100, 0, true), 0);
        let process = system.current_process_arc();
        let process = process.lock().unwrap();
        let readable = process
            .get_readable_event_by_object_id(readable_id)
            .unwrap();
        assert!(readable.lock().unwrap().is_signaled());
    }

    #[test]
    fn send_reply_writes_async_error_result_for_event_request() {
        let mut setup_system = crate::core::System::new_for_test();

        let mut process = KProcess::new();
        process.process_id = 7;
        process.create_memory(&setup_system);
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let event_id = 0x4000;
        let readable_id = 0x4001;
        let event = Arc::new(Mutex::new(crate::hle::kernel::k_event::KEvent::new()));
        event
            .lock()
            .unwrap()
            .initialize(process.process_id, readable_id);
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(event_id, readable_id);
        process.register_event_object(event_id, Arc::clone(&event));
        process.register_readable_event_object(readable_id, Arc::clone(&readable));

        let process = Arc::new(ProcessLock::from_value(process));
        setup_system.set_scheduler_arc(Arc::clone(&scheduler));
        setup_system.set_current_process_arc(Arc::clone(&process));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.client_closed = true;
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            request_guard.event_id = Some(event_id);
            request_guard.client_process_id = Some(7);
            request_guard.address = 0x2395000;
            request_guard.size = 0x1000;
        }
        server.current_request = Some(request);

        assert_eq!(
            server.send_reply_with_message(0x2000, 0x100, 0, false),
            RESULT_SESSION_CLOSED.get_inner_value()
        );

        let process = process.lock().unwrap();
        let memory = process.get_memory().unwrap();
        let mut words = {
            let memory = memory.lock().unwrap();
            [
                memory.read_32(0x2395000),
                memory.read_32(0x2395004),
                memory.read_32(0x2395008),
            ]
        };
        let message = MessageBuffer::new(&mut words);
        assert_eq!(
            message.get_async_result().get_inner_value(),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
    }

    #[test]
    fn receive_request_with_message_promotes_pending_request() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        request.lock().unwrap().initialize(None, 0x2395000, 0x80);
        server.request_list.push_back(Arc::clone(&request));

        assert_eq!(server.receive_request_with_message(0x2000, 0x100, 0), 0);
        let current_request = server.get_current_request().expect("current request");
        let current_request = current_request.lock().unwrap();
        assert_eq!(current_request.get_address(), 0x2395000);
        assert_eq!(current_request.get_size(), 0x80);
    }

    #[test]
    fn send_reply_with_message_requires_current_request() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);
        assert_eq!(
            server.send_reply_with_message(0x2000, 0x100, 0, false),
            RESULT_INVALID_STATE.get_inner_value()
        );
    }

    #[test]
    fn receive_request_with_message_rejects_client_closed() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.client_closed = true;
        assert_eq!(
            server.receive_request_with_message(0x2000, 0x100, 0),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
    }

    #[test]
    fn receive_request_with_message_rejects_reentrant_receive() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(Arc::new(Mutex::new(KSessionRequest::new())));
        assert_eq!(
            server.receive_request_with_message(0x2000, 0x100, 0),
            RESULT_NOT_FOUND.get_inner_value()
        );
    }

    #[test]
    fn receive_request_with_message_finalizes_dead_request_and_notifies_next() {
        // The waiter-wake assertion this test used to cover is now the
        // responsibility of the KSynchronizationObject intrusive-list path
        // (exercised by unit tests in k_synchronization_object and by the
        // assertion, which is server-local behavior.
        let mut server = KServerSession::new();
        server.initialize(0x1000);

        let dead_request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = dead_request.lock().unwrap();
            request_guard.thread_id = Some(77);
            request_guard.client_process_id = Some(88);
        }
        let next_request = Arc::new(Mutex::new(KSessionRequest::new()));

        server.request_list.push_back(Arc::clone(&dead_request));
        server.request_list.push_back(next_request);

        assert_eq!(
            server.receive_request_with_message(0x2000, 0x100, 0),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
        assert_eq!(dead_request.lock().unwrap().get_thread_id(), None);
    }

    #[test]
    fn receive_request_hle_builds_context_from_request_address() {
        let mut system = crate::core::System::new_for_test();
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        process.initialize_handle_table();
        process.create_memory(&system);
        process.allocate_code_memory(0x200000, 0x40000);
        let process = Arc::new(ProcessLock::from_value(process));

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.tls_address =
                crate::hle::kernel::k_typed_address::KProcessAddress::new(0x2395000);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));

        system.set_current_process_arc(Arc::clone(&process));

        let mut request_words = [0u32; crate::hle::ipc::COMMAND_BUFFER_LENGTH];
        let mut raw_high = 0u32;
        raw_high |= 4;
        request_words[0] = crate::hle::ipc::CommandType::Request as u32;
        request_words[1] = raw_high;
        request_words[4] = 0x4943_4653;
        request_words[6] = 0x1111_2222;
        request_words[7] = 0x0003;
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        process
            .lock()
            .unwrap()
            .write_block(0x2395800, &request_bytes);

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request_guard, &thread);
            request_guard.address = 0x2395800;
            request_guard.size = 0x80;
        }
        server.request_list.push_back(request);

        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        let (context, returned_manager, request_message_address) =
            server.receive_request_hle(Arc::clone(&manager)).unwrap();
        assert_eq!(request_message_address, 0x2395800);
        assert!(Arc::ptr_eq(&returned_manager, &manager));
        assert_eq!(
            context.command_buffer()[0],
            crate::hle::ipc::CommandType::Request as u32
        );
        assert_eq!(context.command_buffer()[6], 0x1111_2222);
    }

    #[test]
    fn receive_inline_request_hle_consumes_without_pending_signal_window() {
        let mut system = crate::core::System::new_for_test();
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        process.initialize_handle_table();
        process.create_memory(&system);
        process.allocate_code_memory(0x200000, 0x40000);
        let process = Arc::new(ProcessLock::from_value(process));

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.tls_address =
                crate::hle::kernel::k_typed_address::KProcessAddress::new(0x2395000);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));

        system.set_current_process_arc(Arc::clone(&process));

        let mut request_words = [0u32; crate::hle::ipc::COMMAND_BUFFER_LENGTH];
        request_words[0] = crate::hle::ipc::CommandType::Request as u32;
        request_words[1] = 4;
        request_words[4] = 0x4943_4653;
        request_words[6] = 0x5555_6666;
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        process
            .lock()
            .unwrap()
            .write_block(0x2395400, &request_bytes);

        let mut request = KSessionRequest::new();
        request.initialize_with_process(&process.lock().unwrap(), None, 0x2395400, 0);
        let request = Arc::new(Mutex::new(request));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        let (context, _, request_message_address) = server
            .receive_inline_request_hle(request, Arc::clone(&manager))
            .unwrap();

        assert_eq!(request_message_address, 0x2395400);
        assert_eq!(context.command_buffer()[6], 0x5555_6666);
        assert!(server.request_list.is_empty());
        assert!(server.current_request.is_some());
    }

    #[test]
    fn receive_inline_request_hle_preserves_fifo_while_busy() {
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.tls_address =
                crate::hle::kernel::k_typed_address::KProcessAddress::new(0x2395000);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));
        let make_request = |address: u64| {
            let mut request = KSessionRequest::new();
            request.initialize_with_process(&process.lock().unwrap(), None, address as usize, 0);
            Arc::new(Mutex::new(request))
        };

        let first = make_request(0x2395400);
        let second = make_request(0x2395800);

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(Arc::new(Mutex::new(KSessionRequest::new())));
        server
            .enqueue_inline_request_hle(Arc::clone(&first))
            .unwrap();
        server
            .enqueue_inline_request_hle(Arc::clone(&second))
            .unwrap();

        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        server.current_request = None;
        match server.receive_queued_inline_request_hle(&second, &manager) {
            Err(code) => assert_eq!(code, RESULT_NOT_FOUND.get_inner_value()),
            Ok(_) => panic!("second inline request must not bypass first queued request"),
        }

        assert!(server.current_request.is_none());
        assert_eq!(server.request_list.len(), 2);
        assert!(Arc::ptr_eq(server.request_list.front().unwrap(), &first));
    }

    #[test]
    fn receive_request_hle_ignores_zero_request_size_for_tls_backed_sync_ipc() {
        let mut system = crate::core::System::new_for_test();
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        process.initialize_handle_table();
        process.create_memory(&system);
        process.allocate_code_memory(0x200000, 0x40000);
        let process = Arc::new(ProcessLock::from_value(process));

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.tls_address =
                crate::hle::kernel::k_typed_address::KProcessAddress::new(0x2395000);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&thread));

        system.set_current_process_arc(Arc::clone(&process));

        let mut request_words = [0u32; crate::hle::ipc::COMMAND_BUFFER_LENGTH];
        let mut raw_high = 0u32;
        raw_high |= 4;
        request_words[0] = crate::hle::ipc::CommandType::Request as u32;
        request_words[1] = raw_high;
        request_words[4] = 0x4943_4653;
        request_words[6] = 0x3333_4444;
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        process
            .lock()
            .unwrap()
            .write_block(0x2395200, &request_bytes);

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request_guard, &thread);
            request_guard.address = 0x2395200;
            request_guard.size = 0;
        }
        server.request_list.push_back(request);

        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        let (context, _, request_message_address) =
            server.receive_request_hle(Arc::clone(&manager)).unwrap();
        assert_eq!(request_message_address, 0x2395200);
        assert_eq!(
            context.command_buffer()[0],
            crate::hle::ipc::CommandType::Request as u32
        );
        assert_eq!(context.command_buffer()[6], 0x3333_4444);
    }

    #[test]
    fn send_reply_with_message_ends_sync_client_wait() {
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        let process = Arc::new(ProcessLock::from_value(process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = client_thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&process));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request_guard, &client_thread);
        }
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0x2000, 0x100, 0, true), 0);
        let client_thread = client_thread.lock().unwrap();
        assert_eq!(
            client_thread.get_wait_result(),
            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        );
    }

    // Prior tests `send_reply_with_message_notifies_next_waiting_server_request`
    // and `clear_current_request_and_notify_is_shared_by_receive_failure_path`
    // were removed: they asserted that `server.link_waiter(&mut process, ...)`
    // routed the wake-up to a registered waiter thread. The link_waiter API
    // was deleted in the sync-object refactor — waiter storage is now on an
    // intrusive list populated by `wait()` through fiber-suspended stack
    // allocation, which can't be driven from a standalone unit test. The
    // unit tests in k_synchronization_object.

    #[test]
    fn cleanup_requests_ends_sync_client_wait_with_session_closed() {
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 9;
        let process = Arc::new(ProcessLock::from_value(process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread_guard = client_thread.lock().unwrap();
            thread_guard.thread_id = 7;
            thread_guard.object_id = 8;
            thread_guard.parent = Some(Arc::downgrade(&process));
            thread_guard.begin_wait();
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&process));

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request_guard = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request_guard, &client_thread);
        }
        server.current_request = Some(request);

        server.cleanup_requests();

        let client_thread = client_thread.lock().unwrap();
        assert_eq!(
            client_thread.get_wait_result(),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
    }

    #[test]
    fn cleanup_map_succeeds_without_resolved_processes() {
        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.set_server_process(0x1234);
            request.mappings.push_send(
                KProcessAddress::new(0x1000),
                KProcessAddress::new(0x2000),
                0x40,
                crate::hle::kernel::k_memory_block::KMemoryState::NORMAL,
            );
        }

        assert_eq!(
            KServerSession::cleanup_map(&request),
            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        );
    }

    #[test]
    fn cleanup_server_handles_removes_only_move_handles_from_current_process() {
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 7;
        assert_eq!(process.initialize_handle_table(), 0);

        let copy_handle = process.handle_table.add(0x1111).unwrap();
        let move_handle = process.handle_table.add(0x2222).unwrap();

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x4000);
        }

        let process = Arc::new(ProcessLock::from_value(process));
        {
            thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
            process
                .lock()
                .unwrap()
                .register_thread_object(Arc::clone(&thread));
        }

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&thread));

        let mut words = [0u32; MESSAGE_BUFFER_SIZE];
        let mut message = MessageBuffer::new(&mut words);
        let header = crate::hle::kernel::message_buffer::MessageHeader::new(
            0,
            true,
            0,
            0,
            0,
            0,
            0,
            crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
        );
        let mut offset = message.set_message_header(&header);
        offset = message.set(offset, &[1u32 << 1 | 1u32 << 5]);
        offset = message.set_handle(offset, copy_handle);
        let _ = message.set_handle(offset, move_handle);

        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        process.lock().unwrap().write_block(0x4000, &bytes);

        assert_eq!(KServerSession::cleanup_server_handles(0, 0, 0), 0);
        let process = process.lock().unwrap();
        assert!(process.handle_table.get_object(copy_handle).is_some());
        assert!(process.handle_table.get_object(move_handle).is_none());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn cleanup_server_handles_reads_user_message_from_physical_address() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x30000);
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 7;
        assert_eq!(process.initialize_handle_table(), 0);
        attach_process_page_table_for_session_test(
            &mut process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let virtual_phys = dram_memory_map::BASE + 0x10000;
        let message_phys = dram_memory_map::BASE + 0x20000;
        map_process_page_for_session_test(&mut process, 0x4000, virtual_phys);
        set_current_page_table_for_session_test(&mut process);

        let copy_handle = process.handle_table.add(0x1111).unwrap();
        let move_handle = process.handle_table.add(0x2222).unwrap();

        let thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
        }

        let process = Arc::new(ProcessLock::from_value(process));
        {
            thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
            process
                .lock()
                .unwrap()
                .register_thread_object(Arc::clone(&thread));
        }

        let mut virtual_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut virtual_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0]);
            let _ = message.set_handle(offset, copy_handle);
        }
        let virtual_bytes: Vec<u8> = virtual_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(virtual_phys, &virtual_bytes);

        let mut physical_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut physical_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 << 5]);
            let _ = message.set_handle(offset, move_handle);
        }
        let physical_bytes: Vec<u8> = physical_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(message_phys, &physical_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&thread));

        assert_eq!(
            KServerSession::cleanup_server_handles(
                0x4000,
                MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
                message_phys,
            ),
            0
        );
        let process = process.lock().unwrap();
        assert!(process.handle_table.get_object(copy_handle).is_some());
        assert!(process.handle_table.get_object(move_handle).is_none());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_copies_raw_request_payload_for_simple_message() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        client_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x6000);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        server_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x6000);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0xAAAA_BBBB, 0xCCCC_DDDD]);
            assert_eq!(offset, 4);
        }
        let request_bytes: Vec<u8> = request_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);
        server_process.lock().unwrap().write_block(
            0x5000,
            &vec![0u8; MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>()],
        );

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.thread = Some(Arc::downgrade(&client_thread));
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
            request.address = 0;
            request.size = 0;
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(server.receive_request_with_message(0, 0, 0), 0);

        let copied = server_process
            .lock()
            .unwrap()
            .read_block(0x5000, 16)
            .to_vec();
        assert_eq!(copied, request_bytes);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_uses_server_message_physical_address() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        client_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x6000);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x40000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        server_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x8000);
        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let server_message_paddr = dram_memory_map::BASE + 0x10000;
        let server_virtual_phys = dram_memory_map::BASE + 0x20000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_virtual_phys);
        set_current_page_table_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            assert_eq!(message.set_message_header(&header), 2);
        }
        let request_bytes: Vec<u8> = request_words[..2]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let virtual_sentinel = [0xDDu8; 8];
        page_table_memory.write_phys(server_virtual_phys, &virtual_sentinel);
        page_table_memory.write_phys(
            server_message_paddr,
            &vec![0u8; MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>()],
        );

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.thread = Some(Arc::downgrade(&client_thread));
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
            request.address = 0;
            request.size = 0;
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(
            server.receive_request_with_message(
                0x5000,
                MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
                server_message_paddr,
            ),
            0
        );

        assert_eq!(
            page_table_memory.read_phys(server_message_paddr, 8),
            request_bytes
        );
        assert_eq!(
            page_table_memory.read_phys(server_virtual_phys, 8),
            virtual_sentinel
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_does_not_fallback_when_physical_address_is_invalid() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        client_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x6000);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x30000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        server_process
            .process_memory
            .write()
            .unwrap()
            .allocate(0, 0x8000);
        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let server_virtual_phys = dram_memory_map::BASE + 0x10000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_virtual_phys);
        set_current_page_table_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            assert_eq!(message.set_message_header(&header), 2);
        }
        let request_bytes: Vec<u8> = request_words[..2]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let virtual_sentinel = [0xDDu8; 8];
        page_table_memory.write_phys(server_virtual_phys, &virtual_sentinel);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.thread = Some(Arc::downgrade(&client_thread));
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
            request.address = 0;
            request.size = 0;
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        let invalid_server_message_paddr = dram_memory_map::BASE + 0x40000;
        assert_eq!(
            server.receive_request_with_message(
                0x5000,
                MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
                invalid_server_message_paddr,
            ),
            RESULT_NOT_FOUND.get_inner_value()
        );
        assert_eq!(
            page_table_memory.read_phys(server_virtual_phys, 8),
            virtual_sentinel
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_copies_raw_reply_payload_for_simple_message() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x1111_2222, 0x3333_4444]);
            assert_eq!(offset, 4);
        }
        let reply_bytes: Vec<u8> = reply_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
            request.address = 0;
            request.size = 0;
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);

        let copied = client_process
            .lock()
            .unwrap()
            .read_block(0x3000, 16)
            .to_vec();
        assert_eq!(copied, reply_bytes);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_translates_copy_handles_for_simple_special_header() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        assert_eq!(client_process.initialize_handle_table(), 0);
        let copied_object_id = 0xABCD;
        let copy_handle = client_process.handle_table.add(copied_object_id).unwrap();
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        assert_eq!(server_process.initialize_handle_table(), 0);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let special = crate::hle::kernel::message_buffer::SpecialHeader::new(true, 1, 0);
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 | (1u32 << 1)]);
            offset = message.set_process_id(offset, 0);
            let _ = message.set_handle(offset, copy_handle);
            let _ = special;
        }
        let request_bytes: Vec<u8> = request_words[..5]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(server.receive_request_with_message(0, 0, 0), 0);

        let copied_words = {
            let copied = server_process
                .lock()
                .unwrap()
                .read_block(0x5000, 20)
                .to_vec();
            copied
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect::<Vec<_>>()
        };
        let mut copied_words_for_message = copied_words.clone();
        let copied_message = MessageBuffer::new(&mut copied_words_for_message);
        assert_eq!(copied_message.get_process_id(3), 7);
        let translated_handle = copied_message.get_handle(5);
        let server_process = server_process.lock().unwrap();
        assert_eq!(
            server_process.handle_table.get_object(translated_handle),
            Some(copied_object_id)
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_translates_move_handles_for_simple_special_header() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        assert_eq!(client_process.initialize_handle_table(), 0);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        assert_eq!(server_process.initialize_handle_table(), 0);
        let moved_object_id = 0xDCBA;
        let move_handle = server_process.handle_table.add(moved_object_id).unwrap();
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 << 5]);
            let _ = message.set_handle(offset, move_handle);
        }
        let reply_bytes: Vec<u8> = reply_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);

        let copied_words = {
            let copied = client_process
                .lock()
                .unwrap()
                .read_block(0x3000, 16)
                .to_vec();
            copied
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect::<Vec<_>>()
        };
        let mut copied_words_for_message = copied_words.clone();
        let copied_message = MessageBuffer::new(&mut copied_words_for_message);
        let translated_handle = copied_message.get_handle(3);
        let client_process = client_process.lock().unwrap();
        assert_eq!(
            client_process.handle_table.get_object(translated_handle),
            Some(moved_object_id)
        );
        drop(client_process);
        assert!(server_process
            .lock()
            .unwrap()
            .handle_table
            .get_object(move_handle)
            .is_none());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    #[ignore = "reduced HLE fixture cannot switch Memory's current page table between client/server; direct page-table helper tests cover this path"]
    fn receive_request_with_message_copies_pointer_descriptor_payload() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let client_msg_phys = dram_memory_map::BASE + 0x10000;
        map_process_page_for_session_test(&mut client_process, 0x3000, client_msg_phys);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        page_table_memory.write_phys(client_msg_phys + 0x10, &[0xAA, 0xBB, 0xCC, 0xDD]);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let server_msg_phys = dram_memory_map::BASE + 0x20000;
        let server_recv_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_msg_phys);
        map_process_page_for_session_test(&mut server_process, 0x6000, server_recv_phys);
        let server_process = Arc::new(ProcessLock::from_value(server_process));
        set_current_page_table_for_session_test(&mut server_process.lock().unwrap());

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let request_words = [1u32 << 16, 0, (4u32 << 16), 0x3010];
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(client_msg_phys, &request_bytes);

        let server_buffer_words = [
            0u32,
            ((crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer as u32)
                << 10)
                | (4u32 << 20),
            0,
            0,
            0x6000,
            4u32 << 16,
        ];
        let server_buffer_bytes: Vec<u8> = server_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(server_msg_phys, &server_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.thread = Some(Arc::downgrade(&client_thread));
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(server.receive_request_with_message(0, 0, 0), 0);
        assert_eq!(
            page_table_memory.read_phys(server_recv_phys, 4),
            vec![0xAA, 0xBB, 0xCC, 0xDD]
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn process_send_message_pointer_descriptors_copies_through_page_table() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );

        let server_phys = dram_memory_map::BASE + 0x10000;
        let client_msg_phys = dram_memory_map::BASE + 0x20000;
        let client_recv_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_phys);
        map_process_page_for_session_test(&mut client_process, 0x3000, client_msg_phys);
        map_process_page_for_session_test(&mut client_process, 0x7000, client_recv_phys);
        set_current_page_table_for_session_test(&mut server_process);

        page_table_memory.write_phys(server_phys + 0x10, &[0x11, 0x22, 0x33, 0x44]);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        src_words[0] = 1u32 << 16;
        src_words[2] = 4u32 << 16;
        src_words[3] = 0x5010;
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);

        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        dst_words[0] = 0;
        dst_words[1] = ((crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer
            as u32)
            << 10)
            | (4u32 << 20);
        dst_words[4] = 0x7000;
        dst_words[5] = 4u32 << 16;
        let (dst_header, dst_special_header) = KServerSession::parse_message_headers(&dst_words);

        let rc = KServerSession::process_send_message_pointer_descriptors(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &dst_header,
            &dst_special_header,
            0x3000,
            MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
            false,
            &client_process.page_table,
            &server_process.page_table,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(client_recv_phys, 4),
            vec![0x11, 0x22, 0x33, 0x44]
        );
    }

    #[test]
    fn process_receive_message_pointer_descriptors_linear_to_user_uses_page_table_copy() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );

        let server_msg_phys = dram_memory_map::BASE + 0x10000;
        let server_recv_phys = dram_memory_map::BASE + 0x20000;
        let client_msg_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_msg_phys);
        map_process_page_for_session_test(&mut server_process, 0x6000, server_recv_phys);
        map_process_page_for_session_test(&mut client_process, 0x3000, client_msg_phys);
        set_current_page_table_for_session_test(&mut server_process);

        page_table_memory.write_phys(client_msg_phys + 0x10, &[0xAA, 0xBB, 0xCC, 0xDD]);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        src_words[0] = 1u32 << 16;
        src_words[2] = 4u32 << 16;
        src_words[3] = 0x3010;
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);

        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        dst_words[1] = ((crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer
            as u32)
            << 10)
            | (4u32 << 20);
        dst_words[4] = 0x6000;
        dst_words[5] = 4u32 << 16;
        let (dst_header, dst_special_header) = KServerSession::parse_message_headers(&dst_words);

        let rc = KServerSession::process_receive_message_pointer_descriptors(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &dst_header,
            &dst_special_header,
            0x5000,
            MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
            false,
            &server_process.page_table,
            &client_process.page_table,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(server_recv_phys, 4),
            vec![0xAA, 0xBB, 0xCC, 0xDD]
        );
    }

    #[test]
    fn process_receive_message_pointer_descriptors_to_message_buffer_uses_heap_to_heap() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        server_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x5000,
                1,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                KMemoryAttribute::LOCKED,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );

        let server_msg_phys = dram_memory_map::BASE + 0x10000;
        let client_msg_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_msg_phys);
        map_process_page_for_session_test(&mut client_process, 0x3000, client_msg_phys);
        set_current_page_table_for_session_test(&mut server_process);

        page_table_memory.write_phys(client_msg_phys + 0x10, &[0x10, 0x20, 0x30, 0x40]);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        src_words[0] = 1u32 << 16;
        src_words[2] = 4u32 << 16;
        src_words[3] = 0x3010;
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);

        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        dst_words[1] = (crate::hle::kernel::message_buffer::ReceiveListCountType::ToMessageBuffer
            as u32)
            << 10;
        let (dst_header, dst_special_header) = KServerSession::parse_message_headers(&dst_words);

        let rc = KServerSession::process_receive_message_pointer_descriptors(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &dst_header,
            &dst_special_header,
            0x5000,
            MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
            true,
            &server_process.page_table,
            &client_process.page_table,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        let recv_pointer = 0x5010u64;
        assert_eq!(
            page_table_memory.read_phys(server_msg_phys + 0x10, 4),
            vec![0x10, 0x20, 0x30, 0x40]
        );
        assert_eq!(dst_words[2] & 0xFFFF_0000, 4u32 << 16);
        assert_eq!(dst_words[3], recv_pointer as u32);
    }

    #[test]
    fn process_send_message_raw_data_from_user_source_uses_page_table_copy() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x40000);
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );

        let client_phys = dram_memory_map::BASE + 0x20000;
        map_process_page_for_session_test(&mut client_process, 0x3000, client_phys);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x1111_2222, 0x3333_4444]);
            assert_eq!(offset, 4);
        }
        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];

        let rc = KServerSession::process_send_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &client_process.page_table,
            0x3000,
            false,
            0x5000,
            true,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(client_phys + 8, 8),
            src_words[2..4]
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn process_send_message_raw_data_user_to_user_uses_heap_slow_path() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x70000);
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        client_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x3000,
                2,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );
        client_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x5000,
                2,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ,
                KMemoryAttribute::LOCKED,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );

        let dst_phys = dram_memory_map::BASE + 0x20000;
        let src_phys = dram_memory_map::BASE + 0x40000;
        for page in 0..2 {
            map_process_page_for_session_test(
                &mut client_process,
                0x3000 + page * PAGE_SIZE,
                dst_phys + (page * PAGE_SIZE) as u64,
            );
            map_process_page_for_session_test(
                &mut client_process,
                0x5000 + page * PAGE_SIZE,
                src_phys + (page * PAGE_SIZE) as u64,
            );
        }

        let raw_count = 1023usize;
        let raw_size = raw_count * std::mem::size_of::<u32>();
        let fast_size = PAGE_SIZE - 2 * std::mem::size_of::<u32>();
        let slow_size = raw_size - fast_size;
        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE.max(raw_count + 2)];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                raw_count as i32,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            for index in 0..raw_count {
                offset = message.set(offset, &[0xA500_0000 | index as u32]);
            }
            assert_eq!(offset, raw_count + 2);
        }
        let slow_bytes: Vec<u8> = src_words[2 + fast_size / 4..2 + raw_count]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(src_phys + PAGE_SIZE as u64, &slow_bytes);

        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; src_words.len()];

        let rc = KServerSession::process_send_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &client_process.page_table,
            0x3000,
            true,
            0x5000,
            true,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(dst_phys + PAGE_SIZE as u64, slow_size),
            slow_bytes
        );
    }

    #[test]
    fn process_send_message_raw_data_from_tls_source_to_user_destination_uses_page_table_copy() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;

        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        client_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x3000,
                1,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );

        let client_phys = dram_memory_map::BASE + 0x20000;
        let server_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut client_process, 0x3000, client_phys);
        map_process_page_for_session_test(&mut server_process, 0x5000, server_phys);
        set_current_page_table_for_session_test(&mut server_process);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0xCAFE_BABE, 0xDEAD_BEEF]);
            assert_eq!(offset, 4);
        }
        let source_bytes: Vec<u8> = src_words[2..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(server_phys + 8, &source_bytes);

        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];

        let rc = KServerSession::process_send_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &client_process.page_table,
            0x3000,
            true,
            0x5000,
            false,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(client_phys + 8, source_bytes.len()),
            source_bytes
        );
    }

    #[test]
    fn process_receive_message_raw_data_from_user_source_to_tls_destination_uses_page_table_copy() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        client_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x3000,
                1,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ,
                KMemoryAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );

        let server_phys = dram_memory_map::BASE + 0x20000;
        let client_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_phys);
        map_process_page_for_session_test(&mut client_process, 0x3000, client_phys);
        set_current_page_table_for_session_test(&mut server_process);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x0102_0304, 0xAABB_CCDD]);
            assert_eq!(offset, 4);
        }
        let source_bytes: Vec<u8> = src_words[2..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(client_phys + 8, &source_bytes);

        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];

        let rc = KServerSession::process_receive_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &server_process.page_table,
            0x5000,
            false,
            &client_process.page_table,
            0x3000,
            true,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(server_phys + 8, source_bytes.len()),
            source_bytes
        );
    }

    #[test]
    fn process_receive_message_raw_data_from_tls_source_to_user_destination_uses_linear_kernel_copy(
    ) {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );

        let client_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut client_process, 0x3000, client_phys);

        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x1122_3344, 0x5566_7788]);
            assert_eq!(offset, 4);
        }
        let source_bytes: Vec<u8> = src_words[2..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(client_phys + 8, &source_bytes);

        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; MESSAGE_BUFFER_SIZE];

        let rc = KServerSession::process_receive_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &server_process.page_table,
            0x5000,
            true,
            &client_process.page_table,
            0x3000,
            false,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(&dst_words[2..4], &src_words[2..4]);
    }

    #[test]
    fn process_receive_message_raw_data_user_to_user_uses_heap_slow_path() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x70000);
        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;

        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        server_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x5000,
                2,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ_WRITE,
                KMemoryAttribute::LOCKED,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );
        client_process
            .page_table
            .get_base_mut()
            .m_memory_block_manager
            .update(
                0x3000,
                2,
                KMemoryState::NORMAL,
                KMemoryPermission::NOT_MAPPED | KMemoryPermission::KERNEL_READ,
                KMemoryAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
            );

        let server_phys = dram_memory_map::BASE + 0x20000;
        let client_phys = dram_memory_map::BASE + 0x40000;
        for page in 0..2 {
            map_process_page_for_session_test(
                &mut server_process,
                0x5000 + page * PAGE_SIZE,
                server_phys + (page * PAGE_SIZE) as u64,
            );
            map_process_page_for_session_test(
                &mut client_process,
                0x3000 + page * PAGE_SIZE,
                client_phys + (page * PAGE_SIZE) as u64,
            );
        }

        let raw_count = 1023usize;
        let raw_size = raw_count * std::mem::size_of::<u32>();
        let fast_size = PAGE_SIZE - 2 * std::mem::size_of::<u32>();
        let slow_size = raw_size - fast_size;
        let mut src_words = vec![0u32; MESSAGE_BUFFER_SIZE.max(raw_count + 2)];
        {
            let mut message = MessageBuffer::new(&mut src_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                false,
                0,
                0,
                0,
                0,
                raw_count as i32,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            for index in 0..raw_count {
                offset = message.set(offset, &[0x5A00_0000 | index as u32]);
            }
            assert_eq!(offset, raw_count + 2);
        }
        let slow_bytes: Vec<u8> = src_words[2 + fast_size / 4..2 + raw_count]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        page_table_memory.write_phys(client_phys + PAGE_SIZE as u64, &slow_bytes);

        let (src_header, src_special_header) = KServerSession::parse_message_headers(&src_words);
        let mut dst_words = vec![0u32; src_words.len()];

        let rc = KServerSession::process_receive_message_raw_data(
            &mut dst_words,
            &src_words,
            &src_header,
            &src_special_header,
            &server_process.page_table,
            0x5000,
            true,
            &client_process.page_table,
            0x3000,
            true,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(server_phys + PAGE_SIZE as u64, slow_size),
            slow_bytes
        );
    }

    #[test]
    fn send_reply_with_message_rejects_invalid_destination_header_size() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x1111_2222, 0x3333_4444]);
            assert_eq!(offset, 4);
        }
        let reply_bytes: Vec<u8> = reply_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let client_buffer_words = [0u32, 0x3ff];
        let client_buffer_bytes: Vec<u8> = client_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &client_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);
        assert_eq!(
            client_thread.lock().unwrap().get_wait_result(),
            RESULT_INVALID_COMBINATION.get_inner_value()
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_rejects_invalid_destination_receive_list_offset() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                false,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[0x1111_2222, 0x3333_4444]);
            assert_eq!(offset, 4);
        }
        let reply_bytes: Vec<u8> = reply_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let client_buffer_words = [0u32, (1u32 << 20) | 2];
        let client_buffer_bytes: Vec<u8> = client_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &client_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);
        assert_eq!(
            client_thread.lock().unwrap().get_wait_result(),
            RESULT_INVALID_COMBINATION.get_inner_value()
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_cleans_destination_handles_on_special_data_failure() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        assert_eq!(client_process.initialize_handle_table(), 0);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));
        park_client_thread_for_ipc_session_test(&client_thread);

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        assert_eq!(server_process.initialize_handle_table(), 0);
        let moved_object_id = 0xDCBA;
        let valid_handle = server_process.handle_table.add(moved_object_id).unwrap();
        let invalid_handle = 0x1234u32;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[2u32 | (2u32 << 5)]);
            offset = message.set_handle(offset, valid_handle);
            let _ = message.set_handle(offset, invalid_handle);
        }
        let reply_bytes: Vec<u8> = reply_words[..5]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);
        assert_eq!(
            client_thread.lock().unwrap().get_wait_result(),
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_HANDLE.get_inner_value()
        );
        assert!(client_process
            .lock()
            .unwrap()
            .handle_table
            .get_object(crate::hle::kernel::k_handle_table::encode_handle(0, 1))
            .is_none());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn cleanup_special_data_removes_destination_handles_and_clears_words() {
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 7;
        assert_eq!(process.initialize_handle_table(), 0);

        let copy_handle = process.handle_table.add(0xAAA0).unwrap();
        let move_handle = process.handle_table.add(0xBBB0).unwrap();

        let mut words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 | (1u32 << 1) | (1u32 << 5)]);
            offset = message.set_process_id(offset, process.process_id);
            offset = message.set_handle(offset, copy_handle);
            let _ = message.set_handle(offset, move_handle);
        }

        KServerSession::cleanup_special_data(
            &mut process,
            &mut words,
            MESSAGE_BUFFER_SIZE * std::mem::size_of::<u32>(),
        );

        let message = MessageBuffer::new(&mut words);
        assert_eq!(message.get_process_id(3), 0);
        assert_eq!(message.get_handle(5), INVALID_HANDLE);
        assert_eq!(message.get_handle(6), INVALID_HANDLE);
        assert!(process.handle_table.get_object(copy_handle).is_none());
        assert!(process.handle_table.get_object(move_handle).is_none());
    }

    #[test]
    fn restore_destination_header_restores_original_words() {
        let mut words = [0u32; MESSAGE_BUFFER_SIZE];
        words[0] = 0xDEAD_BEEF;
        words[1] = 0xCAFE_BABE;
        words[2] = 0xFFFF_FFFF;

        let header = crate::hle::kernel::message_buffer::MessageHeader::new(
            0x1234,
            true,
            0,
            0,
            0,
            0,
            0,
            crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
        );
        let special = crate::hle::kernel::message_buffer::SpecialHeader::new(true, 2, 3);

        KServerSession::restore_destination_header(&mut words, &header, &special);

        assert_eq!(words[0], header.get_data()[0]);
        assert_eq!(words[1], header.get_data()[1]);
        assert_eq!(words[2], 1u32 | (2u32 << 1) | (3u32 << 5));
    }

    #[test]
    fn mark_receive_list_broken_matches_upstream_boundary() {
        assert!(!KServerSession::mark_receive_list_broken(4, 4));
        assert!(KServerSession::mark_receive_list_broken(5, 4));
    }

    #[test]
    fn async_cleanup_result_matches_upstream_selection() {
        assert_eq!(
            KServerSession::async_cleanup_result(
                crate::hle::result::RESULT_SUCCESS.get_inner_value()
            )
            .get_inner_value(),
            RESULT_SESSION_CLOSED.get_inner_value()
        );
        assert_eq!(
            KServerSession::async_cleanup_result(RESULT_INVALID_STATE.get_inner_value())
                .get_inner_value(),
            RESULT_INVALID_STATE.get_inner_value()
        );
    }

    #[test]
    fn send_reply_closed_cleanup_failure_returns_success_to_server() {
        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.client_closed = true;

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.set_server_process(0x1234);
            request.mappings.push_send(
                KProcessAddress::new(0x1000),
                KProcessAddress::new(0x2000),
                0x40,
                KMemoryState::NORMAL,
            );
        }
        server.current_request = Some(request);

        assert_eq!(
            server.send_reply_with_message(0, 0, 0, false),
            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        );
    }

    #[test]
    fn process_send_message_receive_mapping_skips_aligned_middle_pages() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x40000);
        let mut src_process = KProcess::new();
        let mut dst_process = KProcess::new();

        let src_addr = 0x4000u64;
        let dst_addr = 0x8000u64;
        let size = PAGE_SIZE * 2;
        let src_phys = dram_memory_map::BASE + 0x10000;
        let dst_phys = dram_memory_map::BASE + 0x20000;

        attach_process_page_table_for_session_test(
            &mut src_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut dst_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::IPC,
        );
        for page in 0..2 {
            map_process_page_for_session_test(
                &mut src_process,
                src_addr as usize + page * PAGE_SIZE,
                src_phys + (page * PAGE_SIZE) as u64,
            );
            map_process_page_for_session_test(
                &mut dst_process,
                dst_addr as usize + page * PAGE_SIZE,
                dst_phys + (page * PAGE_SIZE) as u64,
            );
        }
        set_current_page_table_for_session_test(&mut src_process);

        page_table_memory.write_phys(src_phys, &vec![0xAA; size]);
        page_table_memory.write_phys(dst_phys, &vec![0x55; size]);

        let rc = KServerSession::process_send_message_receive_mapping(
            &src_process.page_table,
            &dst_process.page_table,
            KProcessAddress::new(dst_addr),
            KProcessAddress::new(src_addr),
            size,
            KMemoryState::IPC,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        assert_eq!(
            page_table_memory.read_phys(dst_phys, size),
            vec![0x55; size]
        );
    }

    #[test]
    fn process_send_message_receive_mapping_copies_only_unaligned_edges() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x50000);
        let mut src_process = KProcess::new();
        let mut dst_process = KProcess::new();

        let src_addr = 0x5003u64;
        let dst_addr = 0x9003u64;
        let size = PAGE_SIZE * 2;
        let src_map_addr = 0x5000usize;
        let dst_map_addr = 0x9000usize;
        let src_phys = dram_memory_map::BASE + 0x10000;
        let dst_phys = dram_memory_map::BASE + 0x30000;

        attach_process_page_table_for_session_test(
            &mut src_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        attach_process_page_table_for_session_test(
            &mut dst_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::IPC,
        );
        for page in 0..3 {
            map_process_page_for_session_test(
                &mut src_process,
                src_map_addr + page * PAGE_SIZE,
                src_phys + (page * PAGE_SIZE) as u64,
            );
            map_process_page_for_session_test(
                &mut dst_process,
                dst_map_addr + page * PAGE_SIZE,
                dst_phys + (page * PAGE_SIZE) as u64,
            );
        }
        set_current_page_table_for_session_test(&mut src_process);

        let mut src_bytes = vec![0x11; size];
        src_bytes[..PAGE_SIZE - 3].fill(0xAA);
        src_bytes[PAGE_SIZE - 3..PAGE_SIZE + 3].fill(0x11);
        src_bytes[PAGE_SIZE + 3..].fill(0xCC);
        page_table_memory.write_phys(src_phys + 3, &src_bytes);
        page_table_memory.write_phys(dst_phys + 3, &vec![0x55; size]);

        let rc = KServerSession::process_send_message_receive_mapping(
            &src_process.page_table,
            &dst_process.page_table,
            KProcessAddress::new(dst_addr),
            KProcessAddress::new(src_addr),
            size,
            KMemoryState::IPC,
        );

        assert_eq!(rc, crate::hle::result::RESULT_SUCCESS.get_inner_value());
        let copied = page_table_memory.read_phys(dst_phys + 3, size);
        assert_eq!(&copied[..PAGE_SIZE - 3], &vec![0xAA; PAGE_SIZE - 3]);
        assert_eq!(&copied[PAGE_SIZE - 3..size - 3], &vec![0x55; PAGE_SIZE]);
        assert_eq!(&copied[size - 3..], &vec![0xCC; 3]);
    }

    #[test]
    fn receive_request_with_message_records_map_alias_mapping() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let map_words = KServerSession::encode_map_alias_descriptor(
            0x3010,
            0x40,
            crate::hle::kernel::message_buffer::MapAliasAttribute::Ipc as u32,
        );
        let request_words = [1u32 << 20, 0, map_words[0], map_words[1], map_words[2]];
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(Arc::clone(&request));

        assert_eq!(server.receive_request_with_message(0, 0, 0), 0);
        let request = request.lock().unwrap();
        assert_eq!(request.mappings.get_send_count(), 1);
        let mapping = request.mappings.get_send_mapping(0);
        assert_eq!(mapping.client_address.get(), 0x3010);
        assert_eq!(mapping.server_address.get(), 0x3010);
        assert_eq!(mapping.size, 0x40);
        assert_eq!(mapping.state, KMemoryState::IPC);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_copies_receive_mapping_payload() {
        let mut page_table_memory = SessionPageTableMemoryForTest::new(0x40000);

        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        attach_process_page_table_for_session_test(
            &mut client_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let client_msg_phys = dram_memory_map::BASE + 0x10000;
        let client_recv_phys = dram_memory_map::BASE + 0x20000;
        map_process_page_for_session_test(&mut client_process, 0x3000, client_msg_phys);
        map_process_page_for_session_test(&mut client_process, 0x7000, client_recv_phys);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        attach_process_page_table_for_session_test(
            &mut server_process,
            page_table_memory.memory.clone(),
            0x0000,
            0x10000,
            KMemoryState::NORMAL,
        );
        let server_msg_phys = dram_memory_map::BASE + 0x30000;
        map_process_page_for_session_test(&mut server_process, 0x5000, server_msg_phys);
        set_current_page_table_for_session_test(&mut server_process);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));
        page_table_memory.write_phys(server_msg_phys + 0x10, &[1, 2, 3, 4]);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
            let _ = request.mappings.push_receive(
                KProcessAddress::new(0x7010),
                KProcessAddress::new(0x5010),
                4,
                KMemoryState::IPC,
            );
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);
        assert_eq!(
            page_table_memory.read_phys(client_recv_phys + 0x10, 4),
            vec![1, 2, 3, 4]
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_returns_receive_list_broken_on_transport_failure() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let request_words = [1u32 << 16, 0, (8u32 << 16), 0x3010];
        let request_bytes: Vec<u8> = request_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let server_buffer_words = [
            0u32,
            ((crate::hle::kernel::message_buffer::ReceiveListCountType::ToSingleBuffer as u32)
                << 10)
                | (3u32 << 20),
            0,
            0x6000,
            4u32 << 16,
        ];
        let server_buffer_bytes: Vec<u8> = server_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &server_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(
            server.receive_request_with_message(0, 0, 0),
            RESULT_RECEIVE_LIST_BROKEN.get_inner_value()
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_rejects_move_handles_from_client() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        assert_eq!(client_process.initialize_handle_table(), 0);
        let move_handle = client_process.handle_table.add(0xABCD).unwrap();
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 << 5]);
            let _ = message.set_handle(offset, move_handle);
        }
        let request_bytes: Vec<u8> = request_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(
            server.receive_request_with_message(0, 0, 0),
            RESULT_NOT_FOUND.get_inner_value()
        );

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_cleans_server_handles_on_special_data_failure() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        assert_eq!(client_process.initialize_handle_table(), 0);
        let valid_handle = client_process.handle_table.add(0xABCD).unwrap();
        let invalid_handle = 0x1234u32;
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        assert_eq!(server_process.initialize_handle_table(), 0);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let special = crate::hle::kernel::message_buffer::SpecialHeader::new(true, 2, 0);
            let mut offset = message.set_message_header(&header);
            offset = message.set(
                offset,
                &[1u32 | ((special.get_copy_handle_count() as u32) << 1)],
            );
            offset = message.set_process_id(offset, 0);
            offset = message.set_handle(offset, valid_handle);
            let _ = message.set_handle(offset, invalid_handle);
        }
        let request_bytes: Vec<u8> = request_words[..6]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(
            server.receive_request_with_message(0, 0, 0),
            RESULT_NOT_FOUND.get_inner_value()
        );
        assert!(server_process.lock().unwrap().handle_table.get_count() == 0);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_failure_clears_current_request_for_next_request() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        assert_eq!(client_process.initialize_handle_table(), 0);
        let move_handle = client_process.handle_table.add(0xABCD).unwrap();
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut invalid_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut invalid_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                0,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[1u32 << 5]);
            let _ = message.set_handle(offset, move_handle);
        }
        let invalid_bytes: Vec<u8> = invalid_words[..4]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();

        let mut valid_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut valid_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x9999,
                false,
                0,
                0,
                0,
                0,
                1,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            let _ = message.set(offset, &[0xDEAD_BEEF]);
        }
        let valid_bytes: Vec<u8> = valid_words[..3]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();

        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &invalid_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request1 = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request1.lock().unwrap();
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
        }
        let request2 = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request2.lock().unwrap();
            request.thread_id = Some(3);
            request.client_process_id = Some(7);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request1);
        server.request_list.push_back(request2);

        assert_eq!(
            server.receive_request_with_message(0, 0, 0),
            RESULT_NOT_FOUND.get_inner_value()
        );
        assert!(server.current_request.is_none());

        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &valid_bytes);

        assert_eq!(server.receive_request_with_message(0, 0, 0), 0);
        assert!(server.current_request.is_some());

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn receive_request_with_message_does_not_copy_raw_words_before_special_data_succeeds() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        assert_eq!(client_process.initialize_handle_table(), 0);
        let valid_handle = client_process.handle_table.add(0xABCD).unwrap();
        let invalid_handle = 0x1234u32;
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        assert_eq!(server_process.initialize_handle_table(), 0);
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut request_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut request_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x1234,
                true,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[2u32 << 1]);
            offset = message.set_handle(offset, valid_handle);
            offset = message.set_handle(offset, invalid_handle);
            let _ = message.set(offset, &[0xDEAD_BEEF, 0xCAFE_BABE]);
        }
        let request_bytes: Vec<u8> = request_words[..7]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &request_bytes);

        let server_buffer_words = [
            0u32,
            8u32 << 20,
            0xAAAA_AAAA,
            0xBBBB_BBBB,
            0xCCCC_CCCC,
            0x1111_1111,
            0x2222_2222,
            0x3333_3333,
        ];
        let server_buffer_bytes: Vec<u8> = server_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &server_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.request_list.push_back(request);

        assert_eq!(
            server.receive_request_with_message(0, 0, 0),
            RESULT_NOT_FOUND.get_inner_value()
        );

        let words = server_process
            .lock()
            .unwrap()
            .read_block(0x5000, server_buffer_bytes.len());
        let words: Vec<u32> = words
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        assert_eq!(words[5], 0x1111_1111);
        assert_eq!(words[6], 0x2222_2222);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }

    #[test]
    fn send_reply_with_message_does_not_copy_raw_words_before_special_data_succeeds() {
        let mut client_process = crate::hle::kernel::k_process::KProcess::new();
        client_process.process_id = 7;
        prepare_process_memory_for_session_test(&mut client_process);
        assert_eq!(client_process.initialize_handle_table(), 0);
        let client_process = Arc::new(ProcessLock::from_value(client_process));

        let client_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = client_thread.lock().unwrap();
            thread.thread_id = 3;
            thread.object_id = 4;
            thread.tls_address = KProcessAddress::new(0x3000);
            thread.parent = Some(Arc::downgrade(&client_process));
            thread.begin_wait();
        }
        client_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&client_thread));

        let mut server_process = crate::hle::kernel::k_process::KProcess::new();
        server_process.process_id = 8;
        prepare_process_memory_for_session_test(&mut server_process);
        assert_eq!(server_process.initialize_handle_table(), 0);
        let valid_handle = server_process.handle_table.add(0xDCBA).unwrap();
        let invalid_handle = 0x1234u32;
        let server_process = Arc::new(ProcessLock::from_value(server_process));

        let server_thread = Arc::new(KThreadLock::new(
            crate::hle::kernel::k_thread::KThread::new(),
        ));
        {
            let mut thread = server_thread.lock().unwrap();
            thread.thread_id = 5;
            thread.object_id = 6;
            thread.tls_address = KProcessAddress::new(0x5000);
            thread.parent = Some(Arc::downgrade(&server_process));
        }
        server_process
            .lock()
            .unwrap()
            .register_thread_object(Arc::clone(&server_thread));

        let mut reply_words = [0u32; MESSAGE_BUFFER_SIZE];
        {
            let mut message = MessageBuffer::new(&mut reply_words);
            let header = crate::hle::kernel::message_buffer::MessageHeader::new(
                0x4321,
                true,
                0,
                0,
                0,
                0,
                2,
                crate::hle::kernel::message_buffer::ReceiveListCountType::None as u32,
            );
            let mut offset = message.set_message_header(&header);
            offset = message.set(offset, &[2u32 << 5]);
            offset = message.set_handle(offset, valid_handle);
            offset = message.set_handle(offset, invalid_handle);
            let _ = message.set(offset, &[0xDEAD_BEEF, 0xCAFE_BABE]);
        }
        let reply_bytes: Vec<u8> = reply_words[..7]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        server_process
            .lock()
            .unwrap()
            .write_block(0x5000, &reply_bytes);

        let client_buffer_words = [
            0u32,
            8u32 << 20,
            0xAAAA_AAAA,
            0xBBBB_BBBB,
            0xCCCC_CCCC,
            0x1111_1111,
            0x2222_2222,
            0x3333_3333,
        ];
        let client_buffer_bytes: Vec<u8> = client_buffer_words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        client_process
            .lock()
            .unwrap()
            .write_block(0x3000, &client_buffer_bytes);

        let mut system = crate::core::System::new_for_test();
        system.set_current_process_arc(Arc::clone(&server_process));
        crate::hle::kernel::kernel::set_current_emu_thread(Some(&server_thread));

        let request = Arc::new(Mutex::new(KSessionRequest::new()));
        {
            let mut request = request.lock().unwrap();
            bind_request_client_thread_for_session_test(&mut request, &client_thread);
        }

        let mut server = KServerSession::new();
        server.initialize(0x1000);
        server.current_request = Some(request);

        assert_eq!(server.send_reply_with_message(0, 0, 0, false), 0);

        let words = client_process
            .lock()
            .unwrap()
            .read_block(0x3000, client_buffer_bytes.len());
        let words: Vec<u32> = words
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        assert_eq!(words[5], 0x1111_1111);
        assert_eq!(words[6], 0x2222_2222);

        crate::hle::kernel::kernel::set_current_emu_thread(None);
    }
}
