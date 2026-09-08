// SPDX-FileCopyrightText: 2026 Eden Emulator Project
// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
use super::news_storage::{NewsRecord, NewsRecordV1, NewsStorage};
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Mutex;
// Port of news_data_service.{h,cpp}; local payloads only, no built-in feed download.
pub mod commands {
    pub const OPEN: u32 = 0;
    pub const OPEN_WITH_NEWS_RECORD_V1: u32 = 1;
    pub const READ: u32 = 2;
    pub const GET_SIZE: u32 = 3;
    pub const OPEN_WITH_NEWS_RECORD: u32 = 1001;
}

pub struct INewsDataService {
    opened_payload: Mutex<Vec<u8>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INewsDataService {
    pub fn new() -> Self {
        Self {
            opened_payload: Mutex::new(Vec::new()),
            handlers: build_handler_map(&[
                (commands::OPEN, Some(Self::open_handler), "Open"),
                (
                    commands::OPEN_WITH_NEWS_RECORD_V1,
                    Some(Self::open_with_news_record_v1_handler),
                    "OpenWithNewsRecordV1",
                ),
                (
                    commands::OPEN_WITH_NEWS_RECORD,
                    Some(Self::open_with_news_record_handler),
                    "OpenWithNewsRecord",
                ),
                (commands::READ, Some(Self::read_handler), "Read"),
                (commands::GET_SIZE, Some(Self::get_size_handler), "GetSize"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    fn try_open(&self, key: &[u8], user: &[u8]) -> bool {
        let mut payload = self.opened_payload.lock().unwrap();
        payload.clear();
        let store = NewsStorage::instance().lock().unwrap();
        let found = store
            .find_by_news_id(key, user)
            .or_else(|| {
                if user.is_empty() {
                    None
                } else {
                    store.find_by_news_id(key, b"")
                }
            })
            .or_else(|| {
                store
                    .list_all()
                    .first()
                    .and_then(|r| store.find_by_news_id(to_string_view(&r.news_id), b""))
            });
        if let Some(found) = found {
            *payload = found.payload;
            true
        } else {
            false
        }
    }
    pub fn open(&self, name: &[u8]) -> ResultCode {
        if self.try_open(to_string_view(name), b"") {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        }
    }
    pub fn open_with_news_record_v1(&self, record: NewsRecordV1) -> ResultCode {
        if self.try_open(
            to_string_view(&record.news_id),
            to_string_view(&record.user_id),
        ) {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        }
    }
    pub fn open_with_news_record(&self, record: NewsRecord) -> ResultCode {
        if self.try_open(
            to_string_view(&record.news_id),
            to_string_view(&record.user_id),
        ) {
            RESULT_SUCCESS
        } else {
            RESULT_UNKNOWN
        }
    }
    pub fn read(&self, offset: i64, out: &mut [u8]) -> u64 {
        let payload = self.opened_payload.lock().unwrap();
        let tail = payload.get(offset.max(0) as usize..).unwrap_or_default();
        let count = out.len().min(tail.len());
        out[..count].copy_from_slice(&tail[..count]);
        count as u64
    }
    pub fn get_size(&self) -> i64 {
        self.opened_payload.lock().unwrap().len() as i64
    }
    fn open_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.open(&ctx.read_buffer_a(0));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
    fn open_with_news_record_v1_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let record = RequestParser::new(ctx).pop_raw::<NewsRecordV1>();
        let result = service.open_with_news_record_v1(record);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
    fn open_with_news_record_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let record = RequestParser::new(ctx).pop_raw::<NewsRecord>();
        let result = service.open_with_news_record(record);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }
    fn read_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let offset = RequestParser::new(ctx).pop_u64() as i64;
        let mut out = vec![0; ctx.get_write_buffer_size(0)];
        let size = service.read(offset, &mut out);
        if size != 0 {
            ctx.write_buffer(&out[..size as usize], 0);
        }
        let result = RESULT_SUCCESS;
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u64(size);
    }
    fn get_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let size = service.get_size() as u64;
        let result = RESULT_SUCCESS;
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(result);
        rb.push_u64(size);
    }
}
fn to_string_view(bytes: &[u8]) -> &[u8] {
    bytes.split(|c| *c == 0).next().unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_clip_offset_and_preserve_unwritten_tail() {
        let service = INewsDataService::new();
        *service.opened_payload.lock().unwrap() = vec![1, 2, 3];
        let mut out = [0xaa; 5];
        assert_eq!(service.read(-1, &mut out), 3);
        assert_eq!(out, [1, 2, 3, 0xaa, 0xaa]);
        assert_eq!(service.read(2, &mut out[..2]), 1);
        assert_eq!(out[0], 3);
        assert_eq!(service.read(i64::MAX, &mut out), 0);
        assert_eq!(service.get_size(), 3);
    }
}

impl SessionRequestHandler for INewsDataService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "INewsDataService"
    }
}
impl ServiceFramework for INewsDataService {
    fn get_service_name(&self) -> &str {
        "INewsDataService"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
