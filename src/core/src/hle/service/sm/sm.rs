// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/sm/sm.h and sm.cpp
//!
//! Contains:
//! - ServiceManager: manages service registration and lookup
//! - SM: the "sm:" service interface (ServiceFramework)
//! - LoopProcess: entry point matching upstream `SM::LoopProcess(Core::System&)`
//!
//! Result codes (matching upstream):
//! - ResultInvalidClient (ErrorModule::SM, 2)
//! - ResultAlreadyRegistered (ErrorModule::SM, 4)
//! - ResultInvalidServiceName (ErrorModule::SM, 6)
//! - ResultNotRegistered (ErrorModule::SM, 7)

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::kernel::k_event::KEvent;
use crate::hle::kernel::k_port::KPort;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_scheduler::KScheduler;
use crate::hle::kernel::svc::svc_results::RESULT_INVALID_STATE;
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerFactory,
    SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::server_manager::ServerManager;
use crate::hle::service::service::{
    build_handler_map, FunctionInfo, ServiceFramework, SERVER_SESSION_COUNT_MAX,
};
use crate::hle::service::sm::sm_controller::Controller;

// --- SM result codes (matching upstream sm.cpp) ---

pub const RESULT_INVALID_CLIENT: ResultCode =
    ResultCode::from_module_description(ErrorModule::SM, 2);
pub const RESULT_ALREADY_REGISTERED: ResultCode =
    ResultCode::from_module_description(ErrorModule::SM, 4);
pub const RESULT_INVALID_SERVICE_NAME: ResultCode =
    ResultCode::from_module_description(ErrorModule::SM, 6);
pub const RESULT_NOT_REGISTERED: ResultCode =
    ResultCode::from_module_description(ErrorModule::SM, 7);

// --- ServiceManager ---
//
// Matches upstream `Service::SM::ServiceManager` (sm.h lines 56-103).

/// Manages service registration, lookup, and the controller interface.
///
/// Upstream constructor: `ServiceManager::ServiceManager(Kernel::KernelCore& kernel_)`.
/// Upstream stores: `kernel`, `controller_interface`, `registered_services`,
/// `service_ports`, `deferral_event`.
/// Endpoint Arcs published by a ServerManager when it registers a named
/// service. Used by `svc_port::connect_to_named_port` to wire the new
/// session's SessionRequestManager so descendants can register sessions via
/// the queue (without locking `Mutex<ServerManager>`).
#[derive(Clone)]
pub struct ServiceOwnership {
    pub queue: crate::hle::service::hle_ipc::PendingRegistrationQueue,
    pub wakeup: Arc<crate::hle::service::os::event::Event>,
    pub server_manager: Weak<Mutex<crate::hle::service::server_manager::ServerManager>>,
}

#[derive(Clone)]
pub struct ServicePortEntry {
    pub port: Arc<Mutex<KPort>>,
    pub client_port_object_id: Option<u64>,
}

pub struct ServiceManager {
    controller_interface: Arc<Controller>,

    /// Map of registered services (name → handler factory).
    /// Matches upstream `std::unordered_map<std::string, SessionRequestHandlerFactory>`.
    registered_services: HashMap<String, SessionRequestHandlerFactory>,

    /// Map of service ports (name → KPort).
    /// Matches upstream `std::unordered_map<std::string, Kernel::KClientPort*>`.
    /// Upstream stores `KClientPort*` (obtained via `&port->GetClientPort()`),
    /// but the `KPort` owns both endpoints. We store the whole `KPort` wrapped
    /// in `Arc<Mutex<>>` so the caller can access both client and server sides.
    service_ports: HashMap<String, ServicePortEntry>,

    /// Map of service name → owning ServerManager's endpoints. Published by
    /// `ServerManager::register_named_service` / `manage_named_port` so
    /// `svc_port::connect_to_named_port` can wire the new session's
    /// SessionRequestManager with the pending-registration queue + wakeup
    /// event of the service's host thread.
    service_owners: HashMap<String, ServiceOwnership>,

    /// Deferral event for service registration.
    /// Upstream: `Kernel::KEvent* deferral_event{}`.
    deferral_event: Option<Arc<Mutex<KEvent>>>,
}

impl ServiceManager {
    /// Creates a new ServiceManager.
    ///
    /// Matches upstream `ServiceManager::ServiceManager(Kernel::KernelCore& kernel_)`.
    /// Upstream also creates the Controller interface here.
    pub fn new() -> Self {
        Self {
            controller_interface: Arc::new(Controller::new()),
            registered_services: HashMap::new(),
            service_ports: HashMap::new(),
            service_owners: HashMap::new(),
            deferral_event: None,
        }
    }

    /// Publish the ServerManager endpoints for a service name. Called when
    /// `ServerManager::register_named_service` / `manage_named_port` registers
    /// a service so future `connect_to_named_port` calls can wire the new
    /// session's SessionRequestManager with the queue + wakeup.
    pub fn set_service_ownership(&mut self, name: &str, ownership: ServiceOwnership) {
        self.service_owners.insert(name.to_string(), ownership);
    }

    /// Look up the ServerManager endpoints for a registered service name.
    pub fn get_service_ownership(&self, name: &str) -> Option<&ServiceOwnership> {
        self.service_owners.get(name)
    }

    /// Invokes a control request on the controller interface.
    ///
    /// Matches upstream `ServiceManager::InvokeControlRequest(HLERequestContext&)`.
    pub fn invoke_control_request(&self, ctx: &mut HLERequestContext) {
        self.controller_interface.invoke_request(ctx);
    }

    /// Returns the shared IPC controller.
    pub fn controller_interface(&self) -> Arc<Controller> {
        self.controller_interface.clone()
    }

    /// Sets the deferral event.
    ///
    /// Matches upstream `ServiceManager::SetDeferralEvent(Kernel::KEvent*)`.
    pub fn set_deferral_event(&mut self, event: Option<Arc<Mutex<KEvent>>>) {
        self.deferral_event = event;
    }

    pub fn deferral_event_clone(&self) -> Option<Arc<Mutex<KEvent>>> {
        self.deferral_event.clone()
    }

    /// Registers a service.
    ///
    /// Matches upstream `ServiceManager::RegisterService(KServerPort**, name, max_sessions, handler)`:
    /// ```cpp
    /// auto* port = Kernel::KPort::Create(kernel);
    /// port->Initialize(ServerSessionCountMax, false, 0);
    /// Kernel::KPort::Register(kernel, port);
    /// service_ports.emplace(name, std::addressof(port->GetClientPort()));
    /// registered_services.emplace(name, handler);
    /// if (deferral_event) { deferral_event->Signal(); }
    /// *out_server_port = std::addressof(port->GetServerPort());
    /// ```
    pub fn register_service(
        &mut self,
        name: String,
        max_sessions: u32,
        handler: SessionRequestHandlerFactory,
    ) -> ResultCode {
        match self.register_service_with_port(name, max_sessions, handler) {
            Ok(_) => RESULT_SUCCESS,
            Err(result) => result,
        }
    }

    /// Registers a service and returns the created `KPort` owner.
    ///
    /// This keeps the upstream ownership boundary in `sm.rs`: the service
    /// manager creates the port, stores it internally, and also exposes the
    /// created server endpoint to callers that need to return it through IPC.
    pub fn register_service_with_port(
        &mut self,
        name: String,
        _max_sessions: u32,
        handler: SessionRequestHandlerFactory,
    ) -> Result<Arc<Mutex<KPort>>, ResultCode> {
        let trace_boot = std::env::var_os("RUZU_APPLET_BOOT_TRACE")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"));
        let validate_result = validate_service_name(&name);
        if validate_result.is_error() {
            return Err(validate_result);
        }

        if self.registered_services.contains_key(&name) {
            log::error!("Service is already registered! service={}", name);
            return Err(RESULT_ALREADY_REGISTERED);
        }

        // Create and initialize a KPort (matching upstream).
        let mut port = KPort::new();
        port.initialize(SERVER_SESSION_COUNT_MAX as i32, false, 0);
        let port = Arc::new(Mutex::new(port));

        // Store the port and handler factory.
        // Upstream stores &port->GetClientPort() in service_ports; we store
        // the whole KPort since we own it (no slab allocator yet).
        self.service_ports.insert(
            name.clone(),
            ServicePortEntry {
                port: Arc::clone(&port),
                client_port_object_id: None,
            },
        );
        self.registered_services.insert(name, handler);
        if trace_boot {
            log::info!("ServiceManager::register_service_with_port: inserted service metadata");
        }

        Ok(port)
    }

    pub fn set_service_port_client_port_object_id(
        &mut self,
        name: &str,
        client_port_object_id: u64,
    ) {
        if let Some(entry) = self.service_ports.get_mut(name) {
            entry.client_port_object_id = Some(client_port_object_id);
        }
    }

    /// Unregisters a service.
    ///
    /// Matches upstream `ServiceManager::UnregisterService(const std::string& name)`.
    pub fn unregister_service(&mut self, name: &str) -> ResultCode {
        let validate_result = validate_service_name(name);
        if validate_result.is_error() {
            return validate_result;
        }

        if self.registered_services.remove(name).is_none() {
            log::error!("Server is not registered! service={}", name);
            return RESULT_NOT_REGISTERED;
        }
        self.service_ports.remove(name);

        RESULT_SUCCESS
    }

    /// Gets a service port by name.
    ///
    /// Matches upstream `ServiceManager::GetServicePort(KClientPort**, const std::string& name)`.
    /// Returns the `Arc<Mutex<KPort>>` which contains both client and server endpoints.
    pub fn get_service_port(&self, name: &str) -> Result<Arc<Mutex<KPort>>, ResultCode> {
        let validate_result = validate_service_name(name);
        if validate_result.is_error() {
            return Err(validate_result);
        }

        match self.service_ports.get(name) {
            Some(entry) => Ok(entry.port.clone()),
            None => {
                log::warn!("Server is not registered! service={}", name);
                Err(RESULT_NOT_REGISTERED)
            }
        }
    }

    pub fn get_service_port_entry(&self, name: &str) -> Result<ServicePortEntry, ResultCode> {
        let validate_result = validate_service_name(name);
        if validate_result.is_error() {
            return Err(validate_result);
        }

        match self.service_ports.get(name) {
            Some(entry) => Ok(entry.clone()),
            None => {
                log::warn!("Server is not registered! service={}", name);
                Err(RESULT_NOT_REGISTERED)
            }
        }
    }

    /// Gets a service handler by name, invoking the factory.
    ///
    /// Matches upstream `ServiceManager::GetService<T>(const std::string& name, bool block)`
    /// with `block=false`.
    pub fn get_service(&self, name: &str) -> Option<SessionRequestHandlerPtr> {
        self.registered_services.get(name).map(|factory| factory())
    }

    /// Gets a service handler by name, blocking until registered.
    ///
    /// Matches upstream `ServiceManager::GetService<T>(name, true)` which polls
    /// every 100ms until the service appears.
    pub fn get_service_blocking(
        sm: &Arc<std::sync::Mutex<Self>>,
        system: crate::core::SystemRef,
        name: &str,
    ) -> SessionRequestHandlerPtr {
        const BLOCKING_POLL_NS: i64 = 100_000_000;
        loop {
            if let Some(handler) = sm.lock().unwrap().get_service(name) {
                return handler;
            }

            if !system.is_null()
                && system
                    .get()
                    .kernel()
                    .is_some_and(|kernel| kernel.is_current_thread_guest_core())
            {
                crate::hle::kernel::svc::svc_thread::sleep_thread(system.get(), BLOCKING_POLL_NS);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

impl Drop for ServiceManager {
    /// Matches upstream `ServiceManager::~ServiceManager()` which closes ports and deferral event.
    fn drop(&mut self) {
        // Upstream: for (auto& [name, port] : service_ports) { port->Close(); }
        // Close each port (transition to closed state).
        for (_name, entry) in self.service_ports.drain() {
            entry.port.lock().unwrap().on_server_closed();
        }
        self.registered_services.clear();
        self.deferral_event = None;
    }
}

/// Validates a service name (1-8 characters).
///
/// Matches upstream `static Result ValidateServiceName(const std::string& name)`.
fn validate_service_name(name: &str) -> ResultCode {
    if name.is_empty() || name.len() > 8 {
        log::error!("Invalid service name! service={}", name);
        return RESULT_INVALID_SERVICE_NAME;
    }
    RESULT_SUCCESS
}

// --- SM service ---
//
// Matches upstream `Service::SM::SM` (sm.h lines 35-54, sm.cpp lines 253-270).

/// Interface to "sm:" service.
///
/// Corresponds to upstream `Service::SM::SM : ServiceFramework<SM>`.
/// Constructor: `SM(ServiceManager& service_manager_, Core::System& system_)`.
pub struct Sm {
    system: SystemRef,
    // Upstream stores `ServiceManager&`: the service does not own its manager.
    // A strong Arc here forms `ServiceManager -> factory -> Sm -> ServiceManager`
    // and keeps every service from the previous emulation session alive.
    service_manager: std::sync::Weak<Mutex<ServiceManager>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Sm {
    fn current_process_and_scheduler(&self) -> Option<(Arc<ProcessLock>, Arc<Mutex<KScheduler>>)> {
        let current_thread = self.system.get().current_thread()?;
        let thread_guard = current_thread.lock().unwrap();
        let process = thread_guard
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade())?;
        let scheduler = thread_guard
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.upgrade())
            .or_else(|| {
                process
                    .lock()
                    .unwrap()
                    .scheduler
                    .as_ref()
                    .and_then(|scheduler| scheduler.upgrade())
            })?;
        Some((process, scheduler))
    }

    fn signal_deferral_event(&self, event: &Arc<Mutex<KEvent>>) {
        let Some((process, _scheduler)) = self.current_process_and_scheduler() else {
            return;
        };
        KEvent::signal_arc(event, &process);
    }

    /// Creates a new SM service.
    ///
    /// Matches upstream `SM::SM(ServiceManager& service_manager_, Core::System& system_)`.
    pub fn new(service_manager: Arc<Mutex<ServiceManager>>, system: SystemRef) -> Self {
        // Matches upstream handler registration:
        // RegisterHandlers({{0, &SM::Initialize, "Initialize"}, ...})
        // RegisterHandlersTipc({{0, &SM::Initialize, "Initialize"}, ...})
        let handlers = build_handler_map(&[
            (0, Some(Sm::initialize_handler), "Initialize"),
            (1, Some(Sm::get_service_cmif_handler), "GetService"),
            (
                2,
                Some(Sm::register_service_cmif_handler),
                "RegisterService",
            ),
            (3, Some(Sm::unregister_service_handler), "UnregisterService"),
            (4, None, "DetachClient"),
        ]);
        let handlers_tipc = build_handler_map(&[
            (0, Some(Sm::initialize_handler), "Initialize"),
            (1, Some(Sm::get_service_tipc_handler), "GetService"),
            (
                2,
                Some(Sm::register_service_tipc_handler),
                "RegisterService",
            ),
            (3, Some(Sm::unregister_service_handler), "UnregisterService"),
            (4, None, "DetachClient"),
        ]);

        Self {
            system,
            service_manager: Arc::downgrade(&service_manager),
            handlers,
            handlers_tipc,
        }
    }

    fn service_manager_owner(&self) -> Arc<Mutex<ServiceManager>> {
        self.service_manager
            .upgrade()
            .expect("SM request outlived its ServiceManager")
    }

    // --- Handler trampolines (fn pointers that downcast from &dyn ServiceFramework) ---

    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.initialize(ctx);
    }

    fn get_service_cmif_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.get_service_cmif(ctx);
    }

    fn get_service_tipc_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.get_service_tipc(ctx);
    }

    fn register_service_cmif_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.register_service_cmif(ctx);
    }

    fn register_service_tipc_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.register_service_tipc(ctx);
    }

    fn unregister_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let sm = unsafe { &*(this as *const dyn ServiceFramework as *const Sm) };
        sm.unregister_service(ctx);
    }

    // --- Actual handler implementations ---

    /// SM::Initialize service function.
    ///
    /// Matches upstream `SM::Initialize(HLERequestContext& ctx)`.
    fn initialize(&self, ctx: &mut HLERequestContext) {
        log::debug!("SM::Initialize called");

        if let Some(manager) = ctx.get_manager() {
            manager.lock().unwrap().set_is_initialized_for_sm();
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SM::GetService (CMIF variant).
    ///
    /// Matches upstream `SM::GetServiceCmif(HLERequestContext& ctx)`.
    fn get_service_cmif(&self, ctx: &mut HLERequestContext) {
        let (result, client_session_object_id) = self.get_service_impl(ctx);
        if ctx.get_is_deferred() {
            return;
        }

        if result.is_success() {
            let mut rb = ResponseBuilder::new_with_flags(
                ctx,
                2,
                0,
                1,
                crate::hle::service::ipc_helpers::ResponseBuilderFlags::AlwaysMoveHandles,
            );
            rb.push_result(result);
            rb.push_move_object_id(client_session_object_id.unwrap_or(0));
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(result);
        }
    }

    /// SM::GetService (TIPC variant).
    ///
    /// Matches upstream `SM::GetServiceTipc(HLERequestContext& ctx)`.
    fn get_service_tipc(&self, ctx: &mut HLERequestContext) {
        let (result, client_session_object_id) = self.get_service_impl(ctx);
        if ctx.get_is_deferred() {
            return;
        }

        let mut rb = ResponseBuilder::new_with_flags(
            ctx,
            2,
            0,
            1,
            crate::hle::service::ipc_helpers::ResponseBuilderFlags::AlwaysMoveHandles,
        );
        rb.push_result(result);
        rb.push_move_object_id(if result.is_success() {
            client_session_object_id.unwrap_or(0)
        } else {
            0
        });
    }

    /// Pop service name from request: 8 bytes of ASCII, printable characters only.
    ///
    /// Matches upstream `static std::string PopServiceName(IPC::RequestParser& rp)`.
    fn pop_service_name(rp: &mut RequestParser) -> String {
        let name_buf: [u8; 8] = rp.pop_raw();
        let mut result = String::new();
        for &c in &name_buf {
            if c >= b' ' && c <= b'~' {
                result.push(c as char);
            }
        }
        result
    }

    /// Internal implementation for GetService.
    ///
    /// Matches upstream `SM::GetServiceImpl(KClientSession**, HLERequestContext&)`.
    fn get_service_impl(&self, ctx: &mut HLERequestContext) -> (ResultCode, Option<u64>) {
        if let Some(manager) = ctx.get_manager() {
            if !manager.lock().unwrap().get_is_initialized_for_sm() {
                log::warn!("  GetService: client not initialized for SM");
                return (RESULT_INVALID_CLIENT, None);
            }
        }

        let mut rp = RequestParser::new(ctx);
        let name = Self::pop_service_name(&mut rp);
        log::info!("  GetService: looking up \"{}\"", name);

        let (port, handler, parent_server_manager, parent_queue, parent_wakeup) = {
            let service_manager = self.service_manager_owner();
            let sm = service_manager.lock().unwrap();
            let port_result = sm.get_service_port_entry(&name);
            let handler = sm.get_service(&name);
            // Use the TARGET service's owning ServerManager — not the caller's.
            // Upstream routes the new session via the target port's
            // `on_port_event`, which constructs the SessionRequestManager
            // with the target ServerManager's reference. Ruzu shortcuts that
            // here, but we still need to wire the SessionRequestManager to
            // the right host thread; otherwise notify_available signals the
            // wrong wakeup_event and the new session's IPCs would never be
            // processed.
            let target = sm.get_service_ownership(&name).cloned();
            if std::env::var_os("RUZU_TRACE_SM_OWNERSHIP").is_some() {
                let state = target
                    .as_ref()
                    .map(|t| {
                        if t.server_manager.upgrade().is_some() {
                            "full"
                        } else {
                            "stale_no_owner"
                        }
                    })
                    .unwrap_or("missing");
                let queue = target
                    .as_ref()
                    .map(|t| Arc::as_ptr(&t.queue) as usize)
                    .unwrap_or(0);
                let wakeup = target
                    .as_ref()
                    .map(|t| Arc::as_ptr(&t.wakeup) as usize)
                    .unwrap_or(0);
                eprintln!(
                    "[SM_OWNERSHIP] service={name} state={state} queue=0x{queue:X} wakeup=0x{wakeup:X}"
                );
            }
            let (parent_server_manager, parent_queue, parent_wakeup) = match target {
                Some(t) => (t.server_manager.upgrade(), Some(t.queue), Some(t.wakeup)),
                // Fallback: inherit the caller's chain (preserves pre-fix
                // behavior for any service that doesn't publish ownership).
                None => ctx
                    .get_manager()
                    .map(|m| {
                        let guard = m.lock().unwrap();
                        (
                            guard.get_server_manager(),
                            guard.pending_registrations().cloned(),
                            guard.server_wakeup().cloned(),
                        )
                    })
                    .unwrap_or((None, None, None)),
            };
            (
                port_result.map(|entry| (entry.port, entry.client_port_object_id)),
                handler,
                parent_server_manager,
                parent_queue,
                parent_wakeup,
            )
        };

        match port {
            Err(e) if e == RESULT_INVALID_SERVICE_NAME => {
                log::error!("Invalid service name '{}'", name);
                (RESULT_INVALID_SERVICE_NAME, None)
            }
            Err(_) => {
                // Upstream: LOG_INFO(Service_SM, "Waiting for service {} to become available", name);
                log::info!("Waiting for service {} to become available", name);
                ctx.set_is_deferred();
                (RESULT_NOT_REGISTERED, None)
            }
            Ok((port, client_port_object_id)) => {
                let Some(owner_process) = ctx.owner_process_arc() else {
                    log::error!("  GetService(\"{}\"): missing owner process", name);
                    return (RESULT_INVALID_STATE, None);
                };

                let kernel = self.system.get().kernel().expect("kernel not initialized");
                let mut process = owner_process.lock().unwrap();
                if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
                    log::error!("  GetService(\"{}\"): handle table init failed", name);
                    return (RESULT_INVALID_STATE, None);
                }

                let (session_object_id, client_session_object_id, server_session) = {
                    let mut port_guard = port.lock().unwrap();
                    if port_guard.is_light() {
                        log::error!("  GetService(\"{}\"): light ports not yet ported", name);
                        return (RESULT_INVALID_STATE, None);
                    }

                    if let Some(client_port_object_id) = client_port_object_id {
                        if process
                            .get_client_port_by_object_id(client_port_object_id)
                            .is_none()
                        {
                            process.register_client_port_object(
                                client_port_object_id,
                                Arc::clone(&port),
                            );
                        }
                    }

                    let (session_object_id, client_session_object_id) =
                        match port_guard.client.create_session(
                            &mut process,
                            kernel,
                            client_port_object_id,
                            port_guard.get_name(),
                        ) {
                            Ok(ids) => ids,
                            Err(result) => {
                                log::error!(
                                    "  GetService(\"{}\"): create_session failed {:#x}",
                                    name,
                                    result.get_inner_value()
                                );
                                return (result, None);
                            }
                        };

                    let server_session = process
                        .get_server_session_by_object_id(session_object_id)
                        .unwrap_or_else(|| {
                            log::error!(
                                "[SESSION_REG] missing after create session={:#x} client={:#x} sessions={} clients={} session_keys={:?}",
                                session_object_id,
                                client_session_object_id,
                                process.session_objects.len(),
                                process.client_session_objects.len(),
                                process.session_objects.keys().copied().collect::<Vec<_>>(),
                            );
                            panic!("created server session must be registered");
                        });
                    let enqueue_result = port_guard.enqueue_session(session_object_id);
                    if enqueue_result.is_error() {
                        process.unregister_client_session_object_by_object_id(
                            client_session_object_id,
                        );
                        process.unregister_session_object_by_object_id(session_object_id);
                        return (enqueue_result, None);
                    }

                    (session_object_id, client_session_object_id, server_session)
                };

                if let Some(handler) = handler {
                    let manager = Arc::new(Mutex::new(
                        crate::hle::service::hle_ipc::SessionRequestManager::from_parent_server_manager_endpoints(
                            parent_server_manager,
                            parent_queue.clone(),
                            parent_wakeup.clone(),
                        ),
                    ));
                    manager.lock().unwrap().set_session_handler(handler);

                    if let Some(client_session) =
                        process.get_client_session_by_object_id(client_session_object_id)
                    {
                        client_session.lock().unwrap().initialize(session_object_id);
                    }
                    server_session.lock().unwrap().set_manager(manager.clone());

                    // Register via the parent's ServerManager pending-
                    // registration queue rather than locking
                    // `Mutex<ServerManager>` directly — the parent's host
                    // thread holds it for its loop_process lifetime, so a
                    // direct lock would deadlock. The host thread drains the
                    // queue at the start of each wait_and_process_impl.
                    if let Some(queue) = parent_queue {
                        queue.lock().unwrap().push((server_session, manager));
                        if let Some(wakeup) = parent_wakeup {
                            wakeup.signal();
                        }
                    }
                }

                log::info!(
                    "  GetService(\"{}\") -> client_session_object_id={:#x}",
                    name,
                    client_session_object_id
                );
                (RESULT_SUCCESS, Some(client_session_object_id))
            }
        }
    }

    /// SM::RegisterService (CMIF variant).
    ///
    /// Matches upstream `SM::RegisterServiceCmif(HLERequestContext& ctx)`.
    fn register_service_cmif(&self, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let name = Self::pop_service_name(&mut rp);
        let _is_light = rp.pop_u32() != 0;
        let max_session_count = rp.pop_u32();

        self.register_service_impl(ctx, name, max_session_count, _is_light);
    }

    /// SM::RegisterService (TIPC variant).
    ///
    /// Matches upstream `SM::RegisterServiceTipc(HLERequestContext& ctx)`.
    fn register_service_tipc(&self, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let name = Self::pop_service_name(&mut rp);
        let max_session_count = rp.pop_u32();
        let _is_light = rp.pop_u32() != 0;

        self.register_service_impl(ctx, name, max_session_count, _is_light);
    }

    /// Internal implementation for RegisterService.
    ///
    /// Matches upstream `SM::RegisterServiceImpl(ctx, name, max_session_count, is_light)`.
    fn register_service_impl(
        &self,
        ctx: &mut HLERequestContext,
        name: String,
        max_session_count: u32,
        is_light: bool,
    ) {
        log::debug!(
            "SM::RegisterService called with name={}, max_session_count={}, is_light={}",
            name,
            max_session_count,
            is_light
        );

        let (result, deferral_event) = {
            let service_manager = self.service_manager_owner();
            let mut sm = service_manager.lock().unwrap();
            // Upstream passes nullptr as handler factory for guest-registered services.
            let factory: SessionRequestHandlerFactory =
                Box::new(|| -> SessionRequestHandlerPtr { panic!("null factory called") });
            let result = sm.register_service_with_port(name.clone(), max_session_count, factory);
            let deferral_event = sm.deferral_event_clone();
            (result, deferral_event)
        };
        if let Some(event) = deferral_event {
            if std::env::var_os("RUZU_APPLET_BOOT_TRACE")
                .is_some_and(|value| value != std::ffi::OsStr::new("0"))
            {
                log::info!("SM::RegisterServiceImpl: signaling deferral event after unlock");
            }
            self.signal_deferral_event(&event);
        }

        let port = match result {
            Ok(port) => port,
            Err(result) => {
                log::error!(
                    "failed to register service with error_code={:08X}",
                    result.get_inner_value()
                );
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
                return;
            }
        };

        let Some(owner_process) = ctx.owner_process_arc() else {
            log::error!("SM::RegisterService: owner process missing for service registration");
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_INVALID_STATE);
            return;
        };

        let kernel = self.system.get().kernel().expect("kernel not initialized");
        let client_port_object_id = kernel.create_new_object_id() as u64;
        let server_port_object_id = kernel.create_new_object_id() as u64;
        {
            let mut process = owner_process.lock().unwrap();
            if process.ensure_handle_table_initialized() != RESULT_SUCCESS.get_inner_value() {
                let service_manager = self.service_manager_owner();
                let mut sm = service_manager.lock().unwrap();
                sm.unregister_service(&name);
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(RESULT_INVALID_STATE);
                return;
            }
            process.register_client_port_object(client_port_object_id, Arc::clone(&port));
            process.register_server_port_object(server_port_object_id, port);
        }
        kernel.register_kernel_object(client_port_object_id);
        kernel.register_kernel_object(server_port_object_id);
        self.service_manager_owner()
            .lock()
            .unwrap()
            .set_service_port_client_port_object_id(&name, client_port_object_id);

        let mut rb = ResponseBuilder::new_with_flags(
            ctx,
            2,
            0,
            1,
            crate::hle::service::ipc_helpers::ResponseBuilderFlags::AlwaysMoveHandles,
        );
        rb.push_result(RESULT_SUCCESS);
        rb.push_move_object_id(server_port_object_id);
    }

    /// SM::UnregisterService.
    ///
    /// Matches upstream `SM::UnregisterService(HLERequestContext& ctx)`.
    fn unregister_service(&self, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let name = Self::pop_service_name(&mut rp);

        log::debug!("SM::UnregisterService called with name={}", name);

        let result = self
            .service_manager_owner()
            .lock()
            .unwrap()
            .unregister_service(&name);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
}

impl SessionRequestHandler for Sm {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        self.handle_sync_request_impl(ctx)
    }

    fn service_name(&self) -> &str {
        "sm:"
    }
}

impl ServiceFramework for Sm {
    fn get_service_name(&self) -> &str {
        "sm:"
    }

    fn get_max_sessions(&self) -> u32 {
        4
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }

    fn service_manager(&self) -> Option<Arc<Mutex<ServiceManager>>> {
        self.service_manager.upgrade()
    }
}

// --- LoopProcess ---

/// Runs SM services.
///
/// Matches upstream `void LoopProcess(Core::System& system)` (sm.cpp lines 274-286):
/// ```cpp
/// void LoopProcess(Core::System& system) {
///     auto& service_manager = system.ServiceManager();
///     auto server_manager = std::make_unique<ServerManager>(system);
///
///     Kernel::KEvent* deferral_event{};
///     server_manager->ManageDeferral(&deferral_event);
///     service_manager.SetDeferralEvent(deferral_event);
///
///     auto sm_service = std::make_shared<SM>(system.ServiceManager(), system);
///     server_manager->ManageNamedPort("sm:", [sm_service] { return sm_service; });
///
///     ServerManager::RunServer(std::move(server_manager));
/// }
/// ```
pub fn loop_process(service_manager: &Arc<Mutex<ServiceManager>>, system: crate::core::SystemRef) {
    let trace_boot = std::env::var_os("RUZU_APPLET_BOOT_TRACE")
        .is_some_and(|value| value != std::ffi::OsStr::new("0"));
    let server_manager = ServerManager::new_shared(system);

    // Manage deferral event.
    // Upstream: server_manager->ManageDeferral(&deferral_event);
    //           service_manager.SetDeferralEvent(deferral_event);
    let (_result, deferral_event) = server_manager.lock().unwrap().manage_deferral();
    if trace_boot {
        log::info!("SM::loop_process: deferral event managed");
    }
    service_manager
        .lock()
        .unwrap()
        .set_deferral_event(deferral_event);

    // Register the "sm:" named port.
    // Upstream: ManageNamedPort creates a KPort and registers it via
    // KObjectName::NewFromName so ConnectToNamedPort can find it.
    let sm_service = Arc::new(Sm::new(service_manager.clone(), system));
    let sm_clone = sm_service.clone();
    let factory: SessionRequestHandlerFactory = Box::new(move || sm_clone.clone());
    server_manager
        .lock()
        .unwrap()
        .manage_named_port("sm:", factory, 64);
    if trace_boot {
        log::info!("SM::loop_process: managed named port sm:");
    }

    // Also register in ServiceManager so get_service("sm:") works
    // for internal HLE lookups (ConnectToNamedPort also checks ServiceManager).
    {
        let sm_clone2 = sm_service.clone();
        let factory2: SessionRequestHandlerFactory = Box::new(move || sm_clone2.clone());
        let deferral_event = {
            let mut sm = service_manager.lock().unwrap();
            let result = sm.register_service("sm:".to_string(), 64, factory2);
            if result.is_error() {
                log::error!(
                    "SM::loop_process: failed to register sm: in ServiceManager: 0x{:08X}",
                    result.get_inner_value()
                );
            } else if let Some(kernel) =
                (!system.is_null()).then(|| system.get().kernel()).flatten()
            {
                let client_port_object_id = kernel.create_new_object_id() as u64;
                kernel.register_kernel_object(client_port_object_id);
                sm.set_service_port_client_port_object_id("sm:", client_port_object_id);
            }
            sm.deferral_event_clone()
        };
        if let Some(event) = deferral_event {
            sm_service.signal_deferral_event(&event);
        }
    }
    if trace_boot {
        log::info!("SM::loop_process: registered sm: in ServiceManager");
    }

    // Upstream: ServerManager::RunServer(std::move(server_manager));
    ServerManager::run_server_shared(server_manager);
}

/// Register the `sm:` named port for unit-test systems without entering the
/// blocking `ServerManager::LoopProcess`.
///
/// Runtime keeps using `loop_process`, matching upstream. This helper mirrors
/// only the setup phase required by `System::new_for_test`: manage deferral,
/// create the `sm:` named port, register the internal service lookup entry,
/// bind the final `Arc<Mutex<ServerManager>>`, then register the port while a
/// valid weak pointer is already available so `ConnectToNamedPort` can wire
/// child sessions.
pub fn setup_sm_for_test(
    service_manager: &Arc<Mutex<ServiceManager>>,
    system: crate::core::SystemRef,
) -> Arc<Mutex<ServerManager>> {
    let server_manager = ServerManager::new_shared(system);

    let (_result, deferral_event) = server_manager.lock().unwrap().manage_deferral();
    service_manager
        .lock()
        .unwrap()
        .set_deferral_event(deferral_event);

    let sm_service = Arc::new(Sm::new(service_manager.clone(), system));
    let sm_clone = sm_service.clone();
    let factory: SessionRequestHandlerFactory = Box::new(move || sm_clone.clone());
    server_manager
        .lock()
        .unwrap()
        .manage_named_port("sm:", factory, 64);

    {
        let sm_clone2 = sm_service.clone();
        let factory2: SessionRequestHandlerFactory = Box::new(move || sm_clone2.clone());
        let deferral_event = {
            let mut sm = service_manager.lock().unwrap();
            let result = sm.register_service("sm:".to_string(), 64, factory2);
            if result.is_error() {
                log::error!(
                    "SM::setup_sm_for_test: failed to register sm: in ServiceManager: 0x{:08X}",
                    result.get_inner_value()
                );
            } else if let Some(kernel) =
                (!system.is_null()).then(|| system.get().kernel()).flatten()
            {
                let client_port_object_id = kernel.create_new_object_id() as u64;
                kernel.register_kernel_object(client_port_object_id);
                sm.set_service_port_client_port_object_id("sm:", client_port_object_id);
            }
            sm.deferral_event_clone()
        };
        if let Some(event) = deferral_event {
            sm_service.signal_deferral_event(&event);
        }
    }

    server_manager
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};

    #[test]
    fn test_result_codes() {
        assert!(RESULT_INVALID_CLIENT.is_error());
        assert_eq!(RESULT_INVALID_CLIENT.get_module(), ErrorModule::SM);
        assert_eq!(RESULT_INVALID_CLIENT.get_description(), 2);

        assert!(RESULT_ALREADY_REGISTERED.is_error());
        assert_eq!(RESULT_ALREADY_REGISTERED.get_description(), 4);

        assert!(RESULT_INVALID_SERVICE_NAME.is_error());
        assert_eq!(RESULT_INVALID_SERVICE_NAME.get_description(), 6);

        assert!(RESULT_NOT_REGISTERED.is_error());
        assert_eq!(RESULT_NOT_REGISTERED.get_description(), 7);
    }

    #[test]
    fn test_service_manager_register_unregister() {
        let mut sm = ServiceManager::new();

        let factory: SessionRequestHandlerFactory =
            Box::new(|| -> SessionRequestHandlerPtr { panic!("test") });
        let result = sm.register_service("test_svc".to_string(), 64, factory);
        assert!(result.is_success());

        // Double registration should fail.
        let factory2: SessionRequestHandlerFactory =
            Box::new(|| -> SessionRequestHandlerPtr { panic!("test") });
        let result2 = sm.register_service("test_svc".to_string(), 64, factory2);
        assert_eq!(result2, RESULT_ALREADY_REGISTERED);

        // Unregister.
        let result3 = sm.unregister_service("test_svc");
        assert!(result3.is_success());

        // Unregister again should fail.
        let result4 = sm.unregister_service("test_svc");
        assert_eq!(result4, RESULT_NOT_REGISTERED);
    }

    #[test]
    fn sm_factory_does_not_keep_its_service_manager_alive() {
        let manager = Arc::new(Mutex::new(ServiceManager::new()));
        let weak_manager = Arc::downgrade(&manager);
        let service = Arc::new(Sm::new(Arc::clone(&manager), SystemRef::null()));
        let factory: SessionRequestHandlerFactory = Box::new(move || service.clone());
        assert!(manager
            .lock()
            .unwrap()
            .register_service("sm:".to_string(), 64, factory)
            .is_success());

        drop(manager);

        assert!(weak_manager.upgrade().is_none());
    }

    #[test]
    fn test_validate_service_name() {
        assert!(validate_service_name("sm:").is_success());
        assert!(validate_service_name("12345678").is_success());
        assert!(validate_service_name("").is_error());
        assert!(validate_service_name("123456789").is_error());
    }

    #[test]
    fn test_loop_process() {
        let sm = Arc::new(Mutex::new(ServiceManager::new()));
        loop_process(&sm, crate::core::SystemRef::null());
        // After loop_process, "sm:" is registered on the SM (for connect_to_named_port
        // compatibility) and also as a managed named port on the ServerManager.
        assert!(sm.lock().unwrap().get_service_port("sm:").is_ok());
    }

    #[test]
    fn service_port_identity_allows_session_finalize_notification() {
        let mut system = System::new_for_test();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        {
            let mut process_guard = process.lock().unwrap();
            process_guard.process_id = 1;
            process_guard.initialize_handle_table();
        }
        system.set_current_process_arc(Arc::clone(&process));
        let kernel = system.kernel().unwrap();

        let mut sm = ServiceManager::new();
        let factory: SessionRequestHandlerFactory =
            Box::new(|| -> SessionRequestHandlerPtr { panic!("test") });
        assert!(sm
            .register_service("test_svc".to_string(), 64, factory)
            .is_success());

        let client_port_object_id = kernel.create_new_object_id() as u64;
        kernel.register_kernel_object(client_port_object_id);
        sm.set_service_port_client_port_object_id("test_svc", client_port_object_id);

        let entry = sm.get_service_port_entry("test_svc").unwrap();
        {
            let mut process_guard = process.lock().unwrap();
            process_guard
                .register_client_port_object(client_port_object_id, Arc::clone(&entry.port));
            let (session_object_id, _client_session_object_id) = entry
                .port
                .lock()
                .unwrap()
                .client
                .create_session(&mut process_guard, kernel, entry.client_port_object_id, 0)
                .unwrap();

            assert_eq!(entry.port.lock().unwrap().client.get_num_sessions(), 1);
            process_guard.unregister_session_object_by_object_id(session_object_id);
        }

        assert_eq!(entry.port.lock().unwrap().client.get_num_sessions(), 0);
    }
}
