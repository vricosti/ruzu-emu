// SPDX-FileCopyrightText: 2026 Eden Emulator Project
// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later
//! Local storage counterpart of bcat/news/news_storage.{h,cpp}.
//! The remote built-in feed and its metadata/MessagePack importer are deliberately
//! omitted: the frontend authorizes local News only, with no automatic downloads.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct NewsRecordV1 {
    pub news_id: [u8; 24],
    pub user_id: [u8; 24],
    pub received_time: i64,
    pub read: i32,
    pub newly: i32,
    pub displayed: i32,
    pub extra1: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct NewsRecord {
    pub news_id: [u8; 24],
    pub user_id: [u8; 24],
    pub topic_id: [u8; 32],
    pub received_time: i64,
    pub pad1: [u8; 12],
    pub read: i32,
    pub newly: i32,
    pub displayed: i32,
    pub pad2: [u8; 8],
    pub extra1: i32,
    pub extra2: i32,
}
const _: () = assert!(size_of::<NewsRecord>() == 128);
const _: () = assert!(size_of::<NewsRecordV1>() == 72);

impl NewsRecord {
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.extend(self.news_id);
        out.extend(self.user_id);
        out.extend(self.topic_id);
        out.extend(self.received_time.to_le_bytes());
        out.extend(self.pad1);
        for value in [self.read, self.newly, self.displayed] {
            out.extend(value.to_le_bytes());
        }
        out.extend(self.pad2);
        for value in [self.extra1, self.extra2] {
            out.extend(value.to_le_bytes());
        }
        out
    }

    pub fn to_v1_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(72);
        out.extend(self.news_id);
        out.extend(self.user_id);
        out.extend(self.received_time.to_le_bytes());
        for value in [self.read, self.newly, self.displayed, self.extra1] {
            out.extend(value.to_le_bytes());
        }
        out
    }
}

#[derive(Clone, Default)]
pub struct StoredNews {
    pub record: NewsRecord,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub struct NewsStorage {
    items: HashMap<Vec<u8>, StoredNews>,
    open_counter: usize,
}

impl NewsStorage {
    // Callers hold the mutex for the complete operation instead of returning an
    // upstream reference after its internal scoped_lock has been destroyed.
    pub fn instance() -> &'static Mutex<Self> {
        static INSTANCE: OnceLock<Mutex<NewsStorage>> = OnceLock::new();
        INSTANCE.get_or_init(Mutex::default)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    fn make_key(news_id: &[u8], user_id: &[u8]) -> Vec<u8> {
        [news_id, b"|", user_id].concat()
    }

    fn now() -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_secs() as i64,
            Err(before_epoch) => -(before_epoch.duration().as_secs() as i64),
        }
    }

    fn copy_z(dst: &mut [u8], src: &[u8]) {
        dst.fill(0);
        let n = src.len().min(dst.len() - 1);
        dst[..n].copy_from_slice(&src[..n]);
    }

    pub fn upsert(
        &mut self,
        news_id: &[u8],
        user_id: &[u8],
        topic_id: &[u8],
        time: i64,
        payload: Vec<u8>,
    ) -> &StoredNews {
        let key = Self::make_key(news_id, user_id);
        let existing = self.items.get(&key);
        let was_read = load_read_ids().contains(news_id);
        let mut rec = NewsRecord::default();
        Self::copy_z(&mut rec.news_id, news_id);
        Self::copy_z(&mut rec.user_id, user_id);
        Self::copy_z(
            &mut rec.topic_id,
            if topic_id.is_empty() {
                b"nx_notice"
            } else {
                topic_id
            },
        );
        rec.received_time = existing.map_or_else(
            || {
                if time != 0 {
                    time
                } else {
                    Self::now()
                }
            },
            |old| old.record.received_time,
        );
        rec.read = if was_read {
            1
        } else {
            existing.map_or(0, |old| old.record.read)
        };
        rec.newly = i32::from(!was_read);
        if let Some(old) = existing {
            rec.displayed = old.record.displayed;
            rec.extra1 = old.record.extra1;
            rec.extra2 = old.record.extra2;
        }
        self.items.insert(
            key.clone(),
            StoredNews {
                record: rec,
                payload,
            },
        );
        &self.items[&key]
    }

    pub fn list_all(&self) -> Vec<NewsRecord> {
        let mut out: Vec<_> = self.items.values().map(|item| item.record).collect();
        out.sort_unstable_by(|a, b| b.received_time.cmp(&a.received_time));
        out
    }

    pub fn find_by_news_id(&self, news_id: &[u8], user_id: &[u8]) -> Option<StoredNews> {
        self.items
            .get(&Self::make_key(news_id, user_id))
            .or_else(|| {
                if user_id.is_empty() {
                    None
                } else {
                    self.items.get(&Self::make_key(news_id, b""))
                }
            })
            .cloned()
    }

    pub fn update_record(
        &mut self,
        news_id: &[u8],
        user_id: &[u8],
        updater: impl FnOnce(&mut NewsRecord),
    ) -> bool {
        if let Some(entry) = self.items.get_mut(&Self::make_key(news_id, user_id)) {
            updater(&mut entry.record);
            true
        } else {
            false
        }
    }

    pub fn mark_as_read(&mut self, news_id: &[u8]) {
        for entry in self.items.values_mut() {
            let id = entry
                .record
                .news_id
                .split(|c| *c == 0)
                .next()
                .unwrap_or_default();
            if id == news_id {
                entry.record.read = 1;
                entry.record.newly = 0;
                break;
            }
        }
        let mut ids = load_read_ids();
        ids.insert(news_id.to_vec());
        save_read_ids(&ids);
    }
    pub fn get_and_increment_open_counter(&mut self) -> usize {
        let previous = self.open_counter;
        self.open_counter = self.open_counter.wrapping_add(1);
        previous
    }
    pub fn reset_open_counter(&mut self) {
        self.open_counter = 0;
    }
}

fn get_read_cache_path() -> std::path::PathBuf {
    use common::fs::path_util::{get_ruzu_path, RuzuPath};
    get_ruzu_path(RuzuPath::CacheDir).join("news/news_read")
}
fn load_read_ids() -> std::collections::BTreeSet<Vec<u8>> {
    std::fs::read(get_read_cache_path())
        .unwrap_or_default()
        .split(|c| *c == b'\n')
        .filter(|id| !id.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}
fn save_read_ids(ids: &std::collections::BTreeSet<Vec<u8>>) {
    let path = get_read_cache_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut data = Vec::new();
    for id in ids {
        data.extend(id);
        data.push(b'\n');
    }
    if let Err(error) = std::fs::write(path, data) {
        log::warn!("Unable to save News read flags: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_records_roundtrip_through_database_and_data_interfaces() {
        const CHILD: &str = "RUZU_LOCAL_NEWS_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD) {
            common::fs::path_util::set_app_directory(
                &std::path::PathBuf::from(root).to_string_lossy(),
            );
        } else {
            let root = std::env::temp_dir().join(format!(
                "ruzu-news-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::bcat::news::news_storage::tests::local_records_roundtrip_through_database_and_data_interfaces", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
            std::fs::remove_dir_all(&root).unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use super::super::{
            news_data_service::INewsDataService, news_database_service::INewsDatabaseService,
            news_service::INewsService,
        };
        let db = INewsDatabaseService::new();
        let data = INewsDataService::new();
        assert_eq!(db.count(b"").1, 0);
        assert_eq!(data.open(b"missing"), crate::hle::result::RESULT_UNKNOWN);
        let mut topics = [0xaa; 64];
        assert_eq!(INewsService::new().get_topic_list(&mut topics), 0);
        assert_eq!(topics, [0; 64]);
        {
            let mut store = NewsStorage::instance().lock().unwrap();
            store.upsert(b"LA_LOCAL", b"", b"local", 100, vec![4, 5, 6]);
            store.upsert(b"LA_OTHER", b"", b"local", 50, vec![1]);
            assert_eq!(store.get_and_increment_open_counter(), 0);
            assert_eq!(store.get_and_increment_open_counter(), 1);
            assert_eq!(store.list_all()[0].received_time, 100);
            assert_eq!(
                store.find_by_news_id(b"LA_LOCAL", b"user").unwrap().payload,
                vec![4, 5, 6]
            );
            store.upsert(b"LA_LOCAL", b"", b"local", 200, vec![7, 8]);
            assert_eq!(store.list_all()[0].received_time, 100);
        }
        assert_eq!(db.count(b"").1, 2);
        assert_eq!(
            data.open(b"LA_LOCAL\0ignored"),
            crate::hle::result::RESULT_SUCCESS
        );
        let mut out = [0; 4];
        assert_eq!(data.read(0, &mut out), 2);
        assert_eq!(out, [7, 8, 0, 0]);
        let mut list = [0xaa; 256];
        assert_eq!(db.get_list(0, &mut list, b"", b"").1, 2);
        assert_eq!(
            NewsStorage::instance()
                .lock()
                .unwrap()
                .get_and_increment_open_counter(),
            0
        );
        db.update_integer_value_with_addition(1, b"read", b"'LA_LOCAL'");
        {
            let mut store = NewsStorage::instance().lock().unwrap();
            assert_eq!(
                store.find_by_news_id(b"LA_LOCAL", b"").unwrap().record.read,
                1
            );
            store.clear();
            store.upsert(b"LA_LOCAL", b"", b"", 100, vec![]);
            let record = store.list_all()[0];
            assert_eq!(record.read, 1);
            assert_eq!(record.newly, 0);
            store.clear();
        }
        assert_eq!(data.open(b"missing"), crate::hle::result::RESULT_UNKNOWN);
        assert_eq!(data.get_size(), 0);
    }

    #[test]
    fn record_layout_and_reserved_bytes_match_wire_format() {
        assert_eq!(std::mem::offset_of!(NewsRecord, read), 100);
        assert_eq!(std::mem::offset_of!(NewsRecord, extra2), 124);
        assert_eq!(std::mem::offset_of!(NewsRecordV1, received_time), 48);
        let r = NewsRecord {
            received_time: -1,
            read: 7,
            ..Default::default()
        };
        let bytes = r.to_bytes();
        assert_eq!(bytes.len(), 128);
        assert_eq!(&bytes[88..100], &[0; 12]);
        assert_eq!(&bytes[112..120], &[0; 8]);
        assert_eq!(&bytes[80..88], &(-1i64).to_le_bytes());
        assert_eq!(r.to_v1_bytes().len(), 72);
    }
}
