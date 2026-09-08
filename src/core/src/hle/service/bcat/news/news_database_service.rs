// SPDX-FileCopyrightText: 2025 Eden Emulator Project
// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
use super::news_storage::{NewsRecord, NewsStorage};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;

// Port of bcat/news/news_database_service.{h,cpp}; remote feed loading is excluded.
pub mod commands {
    pub const GET_LIST_V1: u32 = 0;
    pub const COUNT: u32 = 1;
    pub const COUNT_WITH_KEY: u32 = 2;
    pub const UPDATE_INTEGER_VALUE: u32 = 3;
    pub const UPDATE_INTEGER_VALUE_WITH_ADDITION: u32 = 4;
    pub const UPDATE_STRING_VALUE: u32 = 5;
    pub const GET_LIST: u32 = 1000;
}

pub struct INewsDatabaseService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}
impl INewsDatabaseService {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    commands::GET_LIST_V1,
                    Some(Self::get_list_v1_handler),
                    "GetListV1",
                ),
                (commands::COUNT, Some(Self::count_handler), "Count"),
                (
                    commands::COUNT_WITH_KEY,
                    Some(Self::count_with_key_handler),
                    "CountWithKey",
                ),
                (
                    commands::UPDATE_INTEGER_VALUE,
                    Some(Self::update_integer_value_handler),
                    "UpdateIntegerValue",
                ),
                (
                    commands::UPDATE_INTEGER_VALUE_WITH_ADDITION,
                    Some(Self::update_integer_value_with_addition_handler),
                    "UpdateIntegerValueWithAddition",
                ),
                (
                    commands::UPDATE_STRING_VALUE,
                    Some(Self::update_string_value_handler),
                    "UpdateStringValue",
                ),
                (commands::GET_LIST, Some(Self::get_list_handler), "GetList"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
    pub fn count(&self, _where: &[u8]) -> (ResultCode, i32) {
        (
            RESULT_SUCCESS,
            NewsStorage::instance().lock().unwrap().list_all().len() as i32,
        )
    }
    pub fn count_with_key(&self, _key: &[u8], where_clause: &[u8]) -> (ResultCode, i32) {
        self.count(where_clause)
    }
    pub fn get_list(
        &self,
        offset: i32,
        out: &mut [u8],
        _where: &[u8],
        _order: &[u8],
    ) -> (ResultCode, i32) {
        let mut store = NewsStorage::instance().lock().unwrap();
        store.reset_open_counter();
        (
            RESULT_SUCCESS,
            write_records(&store.list_all(), offset, out, false),
        )
    }
    pub fn get_list_v1(
        &self,
        offset: i32,
        out: &mut [u8],
        _where: &[u8],
        _order: &[u8],
    ) -> (ResultCode, i32) {
        let store = NewsStorage::instance().lock().unwrap();
        (
            RESULT_SUCCESS,
            write_records(&store.list_all(), offset, out, true),
        )
    }
    pub fn update_integer_value(&self, value: u32, key: &[u8], _where: &[u8]) -> ResultCode {
        let mut store = NewsStorage::instance().lock().unwrap();
        for rec in store.list_all() {
            store.update_record(to_string_view(&rec.news_id), b"", |r| {
                update_field(r, to_string_view(key), value as i32, false);
            });
        }
        RESULT_SUCCESS
    }
    pub fn update_integer_value_with_addition(
        &self,
        value: u32,
        key: &[u8],
        where_clause: &[u8],
    ) -> ResultCode {
        let column = to_string_view(key);
        let where_clause = to_string_view(where_clause);
        let id = extract_news_id(where_clause);
        let mut store = NewsStorage::instance().lock().unwrap();
        if !id.is_empty() {
            if column == b"read" && value > 0 {
                store.mark_as_read(id);
            } else {
                store.update_record(id, b"", |r| {
                    update_field(r, column, value as i32, true);
                });
            }
        }
        RESULT_SUCCESS
    }
    pub fn update_string_value(&self, _key: &[u8], _value: &[u8], _where: &[u8]) -> ResultCode {
        log::warn!("(STUBBED) UpdateStringValue");
        RESULT_SUCCESS
    }

    fn get_list_v1_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let offset = RequestParser::new(ctx).pop_u32() as i32;
        let mut out = vec![0; ctx.get_write_buffer_size(0)];
        let (_, count) = service.get_list_v1(
            offset,
            &mut out,
            &ctx.read_buffer_x(0),
            &ctx.read_buffer_x(1),
        );
        if out.len() >= 72 {
            ctx.write_buffer(&out, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }

    fn count_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (_, count) = service.count(&ctx.read_buffer_x(0));
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }

    fn count_with_key_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (_, count) = service.count_with_key(&ctx.read_buffer_x(0), &ctx.read_buffer_x(1));
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }

    fn update_integer_value_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let value = RequestParser::new(ctx).pop_u32();
        service.update_integer_value(value, &ctx.read_buffer_x(0), &ctx.read_buffer_x(1));
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn update_integer_value_with_addition_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let value = RequestParser::new(ctx).pop_u32();
        service.update_integer_value_with_addition(
            value,
            &ctx.read_buffer_x(0),
            &ctx.read_buffer_x(1),
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn update_string_value_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.update_string_value(
            &ctx.read_buffer_x(0),
            &ctx.read_buffer_x(1),
            &ctx.read_buffer_x(2),
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_list_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let offset = RequestParser::new(ctx).pop_u32() as i32;
        let mut out = vec![0; ctx.get_write_buffer_size(0)];
        let (_, count) = service.get_list(
            offset,
            &mut out,
            &ctx.read_buffer_x(0),
            &ctx.read_buffer_x(1),
        );
        if out.len() >= 128 {
            ctx.write_buffer(&out, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }
}
fn to_string_view(bytes: &[u8]) -> &[u8] {
    bytes.split(|c| *c == 0).next().unwrap_or_default()
}
fn extract_news_id(where_clause: &[u8]) -> &[u8] {
    let Some(start) = where_clause.windows(3).position(|s| s == b"'LA") else {
        return b"";
    };
    let tail = &where_clause[start + 1..];
    let Some(end) = tail.iter().position(|c| *c == b'\'') else {
        return b"";
    };
    &tail[..end]
}
fn update_field(rec: &mut NewsRecord, column: &[u8], value: i32, additive: bool) -> bool {
    let field = match column {
        b"read" => &mut rec.read,
        b"newly" => &mut rec.newly,
        b"displayed" => &mut rec.displayed,
        b"extra1" | b"extra_1" => &mut rec.extra1,
        b"extra2" | b"extra_2" => &mut rec.extra2,
        b"priority" | b"decoration_type" | b"feedback" | b"category" => return true,
        _ => return false,
    };
    *field = if additive {
        field.wrapping_add(value)
    } else {
        value
    };
    true
}
// Mechanical common serialization for upstream GetList/GetListV1.
fn write_records(list: &[NewsRecord], offset: i32, out: &mut [u8], v1: bool) -> i32 {
    let size = if v1 { 72 } else { 128 };
    if out.len() < size {
        return 0;
    }
    out.fill(0);
    let records = list.get(offset.max(0) as usize..).unwrap_or_default();
    let count = records.len().min(out.len() / size);
    for (record, dest) in records.iter().take(count).zip(out.chunks_exact_mut(size)) {
        dest.copy_from_slice(&if v1 {
            record.to_v1_bytes()
        } else {
            record.to_bytes()
        });
    }
    count as i32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_versions_preserve_offsets_capacity_and_zero_padding() {
        let records = [
            NewsRecord {
                read: 1,
                ..Default::default()
            },
            NewsRecord {
                read: 2,
                ..Default::default()
            },
        ];
        for v1 in [false, true] {
            let size = if v1 { 72 } else { 128 };
            let field = if v1 { 56 } else { 100 };
            let mut small = vec![0xaa; size - 1];
            assert_eq!(write_records(&records, 0, &mut small, v1), 0);
            assert!(small.iter().all(|x| *x == 0xaa));
            let mut output = vec![0xaa; size + 7];
            assert_eq!(write_records(&records, 1, &mut output, v1), 1);
            assert_eq!(&output[field..field + 4], &2i32.to_le_bytes());
            assert!(output[size..].iter().all(|x| *x == 0));
            assert_eq!(write_records(&records, -1, &mut output, v1), 1);
            assert_eq!(&output[field..field + 4], &1i32.to_le_bytes());
            assert_eq!(write_records(&records, i32::MAX, &mut output, v1), 0);
            assert!(output.iter().all(|x| *x == 0));
        }
    }
    #[test]
    fn integer_updates_preserve_signed_bits_and_supported_aliases() {
        let mut r = NewsRecord::default();
        assert!(update_field(&mut r, b"extra_2", -1, false));
        assert_eq!(r.extra2, -1);
        assert!(update_field(&mut r, b"extra2", 2, true));
        assert_eq!(r.extra2, 1);
        assert!(!update_field(&mut r, b"unknown", 3, false));
        assert!(update_field(&mut r, b"priority", 3, false));
        assert_eq!(
            extract_news_id(b"N_SWITCH(news_id,'LA_SYNTHETIC',1,0)=1"),
            b"LA_SYNTHETIC"
        );
        assert_eq!(extract_news_id(b"'LA_unterminated"), b"");
    }
}

impl SessionRequestHandler for INewsDatabaseService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "INewsDatabaseService"
    }
}
impl ServiceFramework for INewsDatabaseService {
    fn get_service_name(&self) -> &str {
        "INewsDatabaseService"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
