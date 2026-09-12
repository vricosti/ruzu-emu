// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/clocks/system_clock_core.h/.cpp
//!
//! SystemClockCore: base system clock that combines a steady clock with a context.

use std::sync::{Arc, Mutex};

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::psc::time::common::{
    OperationEvent, SteadyClockTimePoint, SystemClockContext,
};
use crate::hle::service::psc::time::errors::RESULT_CLOCK_MISMATCH;

use super::context_writers::ContextWriter;

/// SystemClockCore holds a SystemClockContext and references a SteadyClockCore.
///
/// In upstream C++, this holds a reference to SteadyClockCore and a raw pointer
/// to ContextWriter. Here we use a callback for the steady clock time point and
/// an `Arc<Mutex<dyn ContextWriter>>` for the writer — matching the upstream
/// shared-reference pattern where both SystemClockCore and TimeManager can
/// access the same writer.
pub struct SystemClockCore {
    initialized: bool,
    context: SystemClockContext,
    /// Callback to get current time point from the steady clock.
    get_time_point: Box<dyn Fn() -> Result<SteadyClockTimePoint, ResultCode> + Send + Sync>,
    /// Shared reference to the context writer.
    /// Corresponds to `ContextWriter* m_context_writer` in upstream.
    ///
    /// Stored as `Arc<dyn ErasedContextWriter>` so `Arc<Mutex<ConcreteWriter>>`
    /// can be attached without coercing `Mutex<T>` to `Mutex<dyn Trait>`.
    context_writer: Option<Arc<dyn ErasedContextWriter>>,
}

/// Type-erased `Arc<Mutex<impl ContextWriter>>` so Setup* can attach the
/// TimeManager-owned writers the same way Eden assigns `ContextWriter&`.
trait ErasedContextWriter: Send + Sync {
    fn write(&self, context: &SystemClockContext) -> ResultCode;
    fn link(&self, operation_event: OperationEvent);
}

impl<T: ContextWriter + Send> ErasedContextWriter for Mutex<T> {
    fn write(&self, context: &SystemClockContext) -> ResultCode {
        self.lock().unwrap().write(context)
    }

    fn link(&self, operation_event: OperationEvent) {
        self.lock().unwrap().link(operation_event);
    }
}

impl SystemClockCore {
    pub fn new(
        get_time_point: Box<dyn Fn() -> Result<SteadyClockTimePoint, ResultCode> + Send + Sync>,
    ) -> Self {
        Self {
            initialized: false,
            context: SystemClockContext::default(),
            get_time_point,
            context_writer: None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn set_initialized(&mut self) {
        self.initialized = true;
    }

    /// Set the context writer for this clock.
    ///
    /// Corresponds to upstream `m_context_writer = writer;` in the
    /// ServiceManager setup code.
    pub fn set_context_writer<T: ContextWriter + Send + 'static>(
        &mut self,
        writer: Arc<Mutex<T>>,
    ) {
        self.context_writer = Some(writer);
    }

    /// Check if the current steady clock source matches the context's steady time point.
    pub fn check_clock_source_matches(&self) -> bool {
        let context = match self.get_context() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let time_point = match (self.get_time_point)() {
            Ok(tp) => tp,
            Err(_) => return false,
        };
        context.steady_time_point.id_matches(&time_point)
    }

    /// Get the current time in seconds.
    pub fn get_current_time(&self) -> Result<i64, ResultCode> {
        let time_point = (self.get_time_point)()?;
        let context = self.get_context()?;

        if !context.steady_time_point.id_matches(&time_point) {
            return Err(RESULT_CLOCK_MISMATCH);
        }

        Ok(context.offset + time_point.time_point)
    }

    /// Set the current time, deriving a new context from the steady clock.
    pub fn set_current_time(&mut self, time: i64) -> ResultCode {
        let time_point = match (self.get_time_point)() {
            Ok(tp) => tp,
            Err(e) => return e,
        };

        let context = SystemClockContext {
            offset: time - time_point.time_point,
            steady_time_point: time_point,
        };
        self.set_context_and_write(&context)
    }

    pub fn get_current_time_point(&self) -> Result<SteadyClockTimePoint, ResultCode> {
        (self.get_time_point)()
    }

    pub fn get_context(&self) -> Result<SystemClockContext, ResultCode> {
        Ok(self.context)
    }

    pub fn set_context(&mut self, context: &SystemClockContext) -> ResultCode {
        self.context = *context;
        RESULT_SUCCESS
    }

    /// Set the context and write through the context writer.
    ///
    /// Corresponds to `SystemClockCore::SetContextAndWrite` in upstream.
    pub fn set_context_and_write(&mut self, context: &SystemClockContext) -> ResultCode {
        let rc = self.set_context(context);
        if rc != RESULT_SUCCESS {
            return rc;
        }

        if let Some(ref writer) = self.context_writer {
            let rc = writer.write(context);
            if rc != RESULT_SUCCESS {
                return rc;
            }
        }

        RESULT_SUCCESS
    }

    /// Link an operation event to this clock's context writer.
    ///
    /// Corresponds to `SystemClockCore::LinkOperationEvent` in upstream.
    /// The operation event will be signaled whenever the context changes.
    pub fn link_operation_event(&mut self, operation_event: OperationEvent) {
        if let Some(ref writer) = self.context_writer {
            writer.link(operation_event);
        }
    }
}
