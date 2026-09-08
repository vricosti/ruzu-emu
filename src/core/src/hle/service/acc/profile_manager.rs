// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/acc/profile_manager.h
//! Port of zuyu/src/core/hle/service/acc/profile_manager.cpp

use common::fs::path_util::{get_ruzu_path, RuzuPath};
use common::uuid::UUID;

use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};

pub const MAX_USERS: usize = 8;
pub const PROFILE_USERNAME_SIZE: usize = 32;

const ERROR_TOO_MANY_USERS: ResultCode =
    ResultCode::from_module_description(ErrorModule::Account, u32::MAX);
const ERROR_USER_ALREADY_EXISTS: ResultCode =
    ResultCode::from_module_description(ErrorModule::Account, u32::MAX - 1);
const ERROR_ARGUMENT_IS_NULL: ResultCode =
    ResultCode::from_module_description(ErrorModule::Account, 20);

pub type ProfileUsername = [u8; PROFILE_USERNAME_SIZE];
pub type UserIdArray = [u128; MAX_USERS];

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UserRaw {
    uuid: [u8; 16],
    uuid2: [u8; 16],
    timestamp: u64,
    username: ProfileUsername,
    extra_data: UserData,
}

const _: () = assert!(core::mem::size_of::<UserRaw>() == 0xC8);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ProfileDataRaw {
    _padding: [u8; 0x10],
    users: [UserRaw; MAX_USERS],
}

const _: () = assert!(core::mem::size_of::<ProfileDataRaw>() == 0x650);

impl Default for ProfileDataRaw {
    fn default() -> Self {
        Self {
            _padding: [0u8; 0x10],
            users: [UserRaw::default(); MAX_USERS],
        }
    }
}

/// Contains extra data related to a user.
///
/// Corresponds to `UserData` in upstream `profile_manager.h`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserData {
    pub _padding1: u32,
    pub icon_id: u32,
    pub bg_color_id: u8,
    pub _padding2: [u8; 0x7],
    pub _padding3: [u8; 0x10],
    pub _padding4: [u8; 0x60],
}

const _: () = assert!(core::mem::size_of::<UserData>() == 0x80);

impl Default for UserData {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// General information about a user's profile.
///
/// Corresponds to `ProfileInfo` in upstream `profile_manager.h`.
#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub user_uuid: u128,
    pub username: ProfileUsername,
    pub creation_time: u64,
    pub data: UserData,
    pub is_open: bool,
}

impl Default for ProfileInfo {
    fn default() -> Self {
        Self {
            user_uuid: 0,
            username: [0u8; PROFILE_USERNAME_SIZE],
            creation_time: 0,
            data: UserData::default(),
            is_open: false,
        }
    }
}

/// Profile base data sent over IPC.
///
/// Corresponds to `ProfileBase` in upstream `profile_manager.h`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProfileBase {
    pub user_uuid: [u8; 16],
    pub timestamp: u64,
    pub username: ProfileUsername,
}

const _: () = assert!(core::mem::size_of::<ProfileBase>() == 0x38);

impl Default for ProfileBase {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

impl ProfileBase {
    /// Zero out all fields to make the profile slot considered "Empty".
    pub fn invalidate(&mut self) {
        self.user_uuid = [0u8; 16];
        self.timestamp = 0;
        self.username.fill(0);
    }
}

/// The profile manager handles multiple user profiles.
///
/// Corresponds to `ProfileManager` in upstream `profile_manager.h`.
pub struct ProfileManager {
    is_save_needed: bool,
    profiles: [ProfileInfo; MAX_USERS],
    stored_opened_profiles: [ProfileInfo; MAX_USERS],
    user_count: usize,
    last_opened_user: u128,
}

impl ProfileManager {
    pub fn new() -> Self {
        let mut manager = Self {
            is_save_needed: false,
            profiles: Default::default(),
            stored_opened_profiles: Default::default(),
            user_count: 0,
            last_opened_user: 0,
        };
        manager.parse_user_save_file();
        if manager.user_count == 0 {
            let mut username = [0u8; PROFILE_USERNAME_SIZE];
            username[..4].copy_from_slice(b"ruzu");
            let _ = manager.create_new_user(UUID::make_random().as_u128(), &username);
            manager.write_user_save_file();
        }

        let current =
            (*common::settings::values().current_user.get_value() as usize).clamp(0, MAX_USERS - 1);
        let current = if manager.user_exists_index(current) {
            current
        } else {
            common::settings::values_mut().current_user.set_value(0);
            0
        };

        if let Some(uuid) = manager.get_user(current) {
            manager.open_user(uuid);
        }
        manager
    }

    pub fn add_user(&mut self, user: &ProfileInfo) -> ResultCode {
        if self.add_to_profiles(user).is_none() {
            return ERROR_TOO_MANY_USERS;
        }
        RESULT_SUCCESS
    }

    pub fn create_new_user(&mut self, uuid: u128, username: &ProfileUsername) -> ResultCode {
        if self.user_count == MAX_USERS {
            return ERROR_TOO_MANY_USERS;
        }
        if uuid == 0 {
            return ERROR_ARGUMENT_IS_NULL;
        }
        if username[0] == 0 {
            return ERROR_ARGUMENT_IS_NULL;
        }
        if self
            .profiles
            .iter()
            .any(|profile| profile.user_uuid == uuid)
        {
            return ERROR_USER_ALREADY_EXISTS;
        }
        self.is_save_needed = true;
        let info = ProfileInfo {
            user_uuid: uuid,
            username: *username,
            creation_time: 0,
            data: UserData::default(),
            is_open: false,
        };
        self.add_user(&info)
    }

    pub fn get_user(&self, index: usize) -> Option<u128> {
        if index >= MAX_USERS {
            return None;
        }
        Some(self.profiles[index].user_uuid)
    }

    pub fn get_user_index(&self, uuid: u128) -> Option<usize> {
        self.profiles[..self.user_count]
            .iter()
            .position(|p| p.user_uuid == uuid)
    }

    pub fn get_profile_base(&self, index: Option<usize>) -> Option<ProfileBase> {
        let idx = index?;
        if idx >= self.user_count {
            return None;
        }
        let profile = &self.profiles[idx];
        Some(ProfileBase {
            user_uuid: profile.user_uuid.to_le_bytes(),
            timestamp: profile.creation_time,
            username: profile.username,
        })
    }

    pub fn get_profile_base_by_uuid(&self, uuid: u128) -> Option<ProfileBase> {
        let index = self.get_user_index(uuid);
        self.get_profile_base(index)
    }

    pub fn get_profile_base_and_data(
        &self,
        index: Option<usize>,
    ) -> Option<(ProfileBase, UserData)> {
        let idx = index?;
        if idx >= self.user_count {
            return None;
        }
        let profile = &self.profiles[idx];
        Some((
            ProfileBase {
                user_uuid: profile.user_uuid.to_le_bytes(),
                timestamp: profile.creation_time,
                username: profile.username,
            },
            profile.data,
        ))
    }

    pub fn get_user_count(&self) -> usize {
        self.user_count
    }

    pub fn get_open_user_count(&self) -> usize {
        self.profiles[..self.user_count]
            .iter()
            .filter(|p| p.is_open)
            .count()
    }

    pub fn user_exists(&self, uuid: u128) -> bool {
        self.get_user_index(uuid).is_some()
    }

    pub fn user_exists_index(&self, index: usize) -> bool {
        index < MAX_USERS && self.profiles[index].user_uuid != 0
    }

    pub fn open_user(&mut self, uuid: u128) {
        if let Some(idx) = self.get_user_index(uuid) {
            self.profiles[idx].is_open = true;
            self.last_opened_user = uuid;
        }
    }

    pub fn close_user(&mut self, uuid: u128) {
        if let Some(idx) = self.get_user_index(uuid) {
            self.profiles[idx].is_open = false;
        }
    }

    pub fn get_open_users(&self) -> UserIdArray {
        let mut users = [0u128; MAX_USERS];
        for (i, profile) in self.profiles.iter().enumerate() {
            if profile.is_open {
                users[i] = profile.user_uuid;
            }
        }
        users.sort_by_key(|uuid| *uuid == 0);
        users
    }

    pub fn get_all_users(&self) -> UserIdArray {
        let mut users = [0u128; MAX_USERS];
        for (i, profile) in self.profiles.iter().enumerate() {
            users[i] = profile.user_uuid;
        }
        users
    }

    pub fn get_last_opened_user(&self) -> u128 {
        self.last_opened_user
    }

    pub fn get_stored_opened_users(&self) -> UserIdArray {
        let mut users = [0u128; MAX_USERS];
        for (i, profile) in self.stored_opened_profiles.iter().enumerate() {
            if profile.is_open {
                users[i] = profile.user_uuid;
            }
        }
        users.sort_by_key(|uuid| *uuid == 0);
        users
    }

    pub fn store_opened_users(&mut self) {
        self.stored_opened_profiles = Default::default();
        let mut profile_index = 0;
        for profile in self.profiles.iter() {
            if profile.is_open {
                self.stored_opened_profiles[profile_index] = profile.clone();
                profile_index += 1;
            }
        }
    }

    pub fn can_system_register_user(&self) -> bool {
        true
    }

    pub fn remove_user(&mut self, uuid: u128) -> bool {
        if let Some(idx) = self.get_user_index(uuid) {
            self.remove_profile_at_index(idx)
        } else {
            false
        }
    }

    pub fn set_profile_base(&mut self, uuid: u128, profile_new: &ProfileBase) -> bool {
        if let Some(idx) = self.get_user_index(uuid) {
            let new_uuid = u128::from_le_bytes(profile_new.user_uuid);
            if new_uuid == 0 {
                return false;
            }
            self.profiles[idx].user_uuid = new_uuid;
            self.profiles[idx].username = profile_new.username;
            self.profiles[idx].creation_time = profile_new.timestamp;
            self.is_save_needed = true;
            true
        } else {
            false
        }
    }

    pub fn set_profile_base_and_data(
        &mut self,
        uuid: u128,
        profile_new: &ProfileBase,
        data_new: &UserData,
    ) -> bool {
        if let Some(idx) = self.get_user_index(uuid) {
            if self.set_profile_base(uuid, profile_new) {
                self.profiles[idx].data = *data_new;
                self.is_save_needed = true;
                return true;
            }
            false
        } else {
            false
        }
    }

    /// Write user profiles to save file.
    /// Port of upstream `ProfileManager::WriteUserSaveFile`.
    pub fn write_user_save_file(&mut self) {
        if !self.is_save_needed {
            return;
        }

        let save_dir =
            get_ruzu_path(RuzuPath::NANDDir).join("system/save/8000000000000010/su/avators");
        if let Err(e) = std::fs::create_dir_all(&save_dir) {
            log::warn!("Failed to create profile save directory: {}", e);
            return;
        }

        let save_path = save_dir.join("profiles.dat");

        let mut raw = ProfileDataRaw::default();
        for i in 0..MAX_USERS {
            let profile = &self.profiles[i];
            raw.users[i] = UserRaw {
                uuid: profile.user_uuid.to_le_bytes(),
                uuid2: profile.user_uuid.to_le_bytes(),
                timestamp: profile.creation_time,
                username: profile.username,
                extra_data: profile.data,
            };
        }

        let data: &[u8] = unsafe {
            std::slice::from_raw_parts(
                &raw as *const ProfileDataRaw as *const u8,
                core::mem::size_of::<ProfileDataRaw>(),
            )
        };

        match std::fs::write(&save_path, &data) {
            Ok(()) => {
                self.is_save_needed = false;
                log::debug!("Wrote profile save data to {}", save_path.display());
            }
            Err(e) => {
                log::warn!("Failed to write profile save data: {}", e);
            }
        }
    }

    /// Parse user profiles from save file.
    /// Port of upstream `ProfileManager::ParseUserSaveFile`.
    fn parse_user_save_file(&mut self) {
        let save_path = get_ruzu_path(RuzuPath::NANDDir)
            .join("system/save/8000000000000010/su/avators/profiles.dat");

        let data = match std::fs::read(&save_path) {
            Ok(d) => d,
            Err(_) => {
                log::warn!(
                    "Failed to load profile data from save... \
                     Generating new user 'yuzu' with random UUID."
                );
                return;
            }
        };

        if data.len() < core::mem::size_of::<ProfileDataRaw>() {
            log::warn!(
                "profiles.dat is smaller than expected ({} < {})... \
                 Generating new user 'yuzu' with random UUID.",
                data.len(),
                core::mem::size_of::<ProfileDataRaw>(),
            );
            return;
        }

        let raw = unsafe { &*(data.as_ptr() as *const ProfileDataRaw) };

        for user in raw.users {
            let uuid = u128::from_le_bytes(user.uuid);
            if uuid == 0 {
                continue;
            }

            self.add_user(&ProfileInfo {
                user_uuid: uuid,
                username: user.username,
                creation_time: user.timestamp,
                data: user.extra_data,
                is_open: false,
            });
        }

        // Sort valid profiles first (matching upstream stable_partition).
        self.profiles.sort_by_key(|profile| profile.user_uuid == 0);
    }

    fn add_to_profiles(&mut self, profile: &ProfileInfo) -> Option<usize> {
        if self.user_count >= MAX_USERS {
            return None;
        }
        let idx = self.user_count;
        self.profiles[idx] = profile.clone();
        self.user_count += 1;
        Some(idx)
    }

    fn remove_profile_at_index(&mut self, index: usize) -> bool {
        if index >= MAX_USERS || index >= self.user_count {
            return false;
        }
        // RemoveProfileAtIndex clears the selected slot before stable-partitioning
        // valid users. In particular, the last occupied slot need not be slot 7.
        self.profiles[index] = ProfileInfo::default();
        self.profiles.sort_by_key(|profile| profile.user_uuid == 0);
        self.user_count -= 1;
        self.is_save_needed = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProfileBase, ProfileManager, ERROR_ARGUMENT_IS_NULL, ERROR_TOO_MANY_USERS,
        ERROR_USER_ALREADY_EXISTS, MAX_USERS, PROFILE_USERNAME_SIZE,
    };
    use common::fs::path_util::{set_ruzu_path, RuzuPath};
    use common::uuid::UUID;

    #[test]
    fn removed_profiles_stay_removed_after_disk_reload() {
        const CHILD: &str = "RUZU_PROFILE_REMOVAL_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD) {
            set_ruzu_path(RuzuPath::NANDDir, std::path::Path::new(&root));
            for count in 1..=MAX_USERS {
                for removed in 0..count {
                    let mut manager = ProfileManager {
                        is_save_needed: false,
                        profiles: Default::default(),
                        stored_opened_profiles: Default::default(),
                        user_count: 0,
                        last_opened_user: 0,
                    };
                    let mut name = [0; PROFILE_USERNAME_SIZE];
                    name[..8].copy_from_slice(b"Homebrew");
                    for index in 0..count {
                        assert_eq!(manager.create_new_user(index as u128 + 1, &name), super::RESULT_SUCCESS);
                    }
                    assert!(manager.remove_user(removed as u128 + 1));
                    let expected: Vec<_> = (1..=count as u128)
                        .filter(|uuid| *uuid != removed as u128 + 1).collect();
                    assert_eq!(manager.user_count, expected.len());
                    assert_eq!(&manager.get_all_users()[..expected.len()], expected.as_slice());
                    assert!(manager.profiles[expected.len()..].iter().all(|p| p.user_uuid == 0));
                    assert!(!manager.remove_user(removed as u128 + 1));
                    manager.write_user_save_file();
                    let bytes = std::fs::read(std::path::Path::new(&root)
                        .join("system/save/8000000000000010/su/avators/profiles.dat")).unwrap();
                    assert!(bytes[0x10 + expected.len() * 0xc8..].iter().all(|b| *b == 0));
                    // Parse directly: the constructor deliberately creates a new
                    // default user when the last profile has been removed.
                    manager.profiles = Default::default();
                    manager.user_count = 0;
                    manager.parse_user_save_file();
                    assert_eq!(manager.user_count, expected.len());
                    assert_eq!(&manager.get_all_users()[..expected.len()], expected.as_slice());
                }
            }
            return;
        }
        let root = std::env::temp_dir().join(format!("ruzu-profile-removal-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
            .env(CHILD, &root).status().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(status.success());
    }

    #[test]
    fn profile_manager_new_creates_and_opens_default_user() {
        let base = std::env::temp_dir().join(format!(
            "ruzu_profile_manager_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        set_ruzu_path(RuzuPath::NANDDir, &base);
        common::settings::values_mut().current_user.set_value(0);

        let manager = ProfileManager::new();
        assert_eq!(manager.get_user_count(), 1);
        assert_eq!(manager.get_open_user_count(), 1);

        let user = manager.get_user(0).unwrap();
        assert_ne!(user, 0);

        let mut expected_name = [0u8; PROFILE_USERNAME_SIZE];
        expected_name[..4].copy_from_slice(b"ruzu");
        assert_eq!(
            manager.get_profile_base(Some(0)).unwrap().username,
            expected_name
        );
        assert_eq!(manager.get_profile_base(Some(0)).unwrap().timestamp, 0);

        let user_bytes = user.to_le_bytes();
        assert_ne!(user_bytes, UUID::new().uuid);
    }

    #[test]
    fn profile_manager_create_new_user_matches_upstream_validation() {
        let base = std::env::temp_dir().join(format!(
            "ruzu_profile_manager_create_validation_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        set_ruzu_path(RuzuPath::NANDDir, &base);
        common::settings::values_mut().current_user.set_value(0);

        let mut manager = ProfileManager::new();
        let before_count = manager.get_user_count();
        let empty_name = [0u8; PROFILE_USERNAME_SIZE];
        let mut valid_name = [0u8; PROFILE_USERNAME_SIZE];
        valid_name[..4].copy_from_slice(b"test");

        assert_eq!(
            manager.create_new_user(0, &valid_name),
            ERROR_ARGUMENT_IS_NULL
        );
        assert_eq!(
            manager.create_new_user(0x1234, &empty_name),
            ERROR_ARGUMENT_IS_NULL
        );
        assert_eq!(manager.get_user_count(), before_count);

        let existing_user = manager.get_user(0).unwrap();
        assert_eq!(
            manager.create_new_user(existing_user, &valid_name),
            ERROR_USER_ALREADY_EXISTS
        );

        for index in before_count..MAX_USERS {
            let uuid = 0x2000 + index as u128;
            let mut name = [0u8; PROFILE_USERNAME_SIZE];
            name[..4].copy_from_slice(b"fill");
            assert!(manager.create_new_user(uuid, &name).is_success());
        }
        assert_eq!(
            manager.create_new_user(0x3000, &valid_name),
            ERROR_TOO_MANY_USERS
        );
        assert!(manager.get_user(MAX_USERS).is_none());
    }

    #[test]
    fn profile_manager_set_profile_base_replaces_uuid() {
        let base = std::env::temp_dir().join(format!(
            "ruzu_profile_manager_set_base_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        set_ruzu_path(RuzuPath::NANDDir, &base);
        common::settings::values_mut().current_user.set_value(0);

        let mut manager = ProfileManager::new();
        let old_uuid = manager.get_user(0).unwrap();
        let new_uuid = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_F001u128;
        let mut profile = ProfileBase::default();
        profile.user_uuid = new_uuid.to_le_bytes();
        profile.timestamp = 0x55AA;
        profile.username[..4].copy_from_slice(b"mii0");

        assert!(manager.set_profile_base(old_uuid, &profile));
        assert_eq!(manager.get_user(0), Some(new_uuid));
        let updated = manager.get_profile_base_by_uuid(new_uuid).unwrap();
        assert_eq!(updated.timestamp, 0x55AA);
        assert_eq!(&updated.username[..4], b"mii0");
    }

    #[test]
    fn profile_manager_write_user_save_file_uses_profile_data_raw_layout() {
        let base = std::env::temp_dir().join(format!(
            "ruzu_profile_manager_layout_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        set_ruzu_path(RuzuPath::NANDDir, &base);
        common::settings::values_mut().current_user.set_value(0);

        let mut manager = ProfileManager::new();
        manager.write_user_save_file();

        let save_path = base.join("system/save/8000000000000010/su/avators/profiles.dat");
        let data = std::fs::read(save_path).unwrap();
        assert_eq!(data.len(), core::mem::size_of::<super::ProfileDataRaw>());
        assert_eq!(&data[..0x10], &[0u8; 0x10]);

        let user = manager.get_user(0).unwrap();
        assert_eq!(&data[0x10..0x20], &user.to_le_bytes());
        assert_eq!(&data[0x20..0x30], &user.to_le_bytes());
    }
}
