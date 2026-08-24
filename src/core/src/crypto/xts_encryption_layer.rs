// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/crypto/xts_encryption_layer.h and xts_encryption_layer.cpp
//! Sits on top of a VirtualFile and provides XTS-mode AES decryption.

use parking_lot::Mutex;

use super::aes_util::{AesCipher, Mode, Op};
use super::encryption_layer::{EncryptionLayerBase, VfsFile, VirtualFile};
use super::key_manager::Key256;

/// XTS sector size: 0x4000 (16 KiB).
const XTS_SECTOR_SIZE: u64 = 0x4000;

/// XTS-mode AES encryption layer.
/// Port of Core::Crypto::XTSEncryptionLayer.
pub struct XtsEncryptionLayer {
    base_layer: EncryptionLayerBase,
    /// Must be interior-mutable because cipher state changes during read operations.
    cipher: Mutex<AesCipher>,
}

impl XtsEncryptionLayer {
    pub fn new(base: VirtualFile, key: Key256) -> Self {
        let cipher = AesCipher::new_256(key, Mode::XTS);
        Self {
            base_layer: EncryptionLayerBase::new(base),
            cipher: Mutex::new(cipher),
        }
    }
}

impl VfsFile for XtsEncryptionLayer {
    fn read(&self, data: &mut [u8], offset: usize) -> usize {
        let length = data.len();
        if length == 0 {
            return 0;
        }

        let sector_size = XTS_SECTOR_SIZE as usize;
        let sector_offset = offset & (sector_size - 1); // offset % 0x4000

        if sector_offset == 0 {
            if length % sector_size == 0 {
                let raw = self.base_layer.base.read_bytes(length, offset);
                self.cipher.lock().xts_transcode(
                    &raw,
                    data,
                    offset / sector_size,
                    sector_size,
                    Op::Decrypt,
                );
                return raw.len();
            }
            if length > sector_size {
                let rem = length % sector_size;
                let read_aligned = length - rem;
                let a = self.read(&mut data[..read_aligned], offset);
                let b = self.read(&mut data[read_aligned..], offset + read_aligned);
                return a + b;
            }
            // length < sector_size
            let mut buffer = self.base_layer.base.read_bytes(sector_size, offset);
            if buffer.len() < sector_size {
                buffer.resize(sector_size, 0);
            }
            let mut decrypted = vec![0u8; sector_size];
            self.cipher.lock().xts_transcode(
                &buffer,
                &mut decrypted,
                offset / sector_size,
                sector_size,
                Op::Decrypt,
            );
            let copy_len = length.min(decrypted.len());
            data[..copy_len].copy_from_slice(&decrypted[..copy_len]);
            return copy_len;
        }

        // Offset does not fall on block boundary (0x4000)
        let block_start = offset - sector_offset;
        let mut block = self.base_layer.base.read_bytes(sector_size, block_start);
        if block.len() < sector_size {
            block.resize(sector_size, 0);
        }
        let mut decrypted = vec![0u8; sector_size];
        self.cipher.lock().xts_transcode(
            &block,
            &mut decrypted,
            block_start / sector_size,
            sector_size,
            Op::Decrypt,
        );

        let read = sector_size - sector_offset;

        if length + sector_offset < sector_size {
            let copy_len = length.min(read);
            data[..copy_len].copy_from_slice(&decrypted[sector_offset..sector_offset + copy_len]);
            return copy_len;
        }

        data[..read].copy_from_slice(&decrypted[sector_offset..sector_offset + read]);
        let more = self.read(&mut data[read..], offset + read);
        read + more
    }

    fn get_name(&self) -> String {
        self.base_layer.get_name()
    }

    fn get_size(&self) -> usize {
        self.base_layer.get_size()
    }

    fn resize(&self, new_size: usize) -> bool {
        self.base_layer.resize(new_size)
    }

    fn is_writable(&self) -> bool {
        self.base_layer.is_writable()
    }

    fn is_readable(&self) -> bool {
        self.base_layer.is_readable()
    }

    fn write(&self, data: &[u8], offset: usize) -> usize {
        self.base_layer.write(data, offset)
    }

    fn rename(&self, name: &str) -> bool {
        self.base_layer.rename(name)
    }
}

/// Implement the file_sys VfsFile trait so XtsEncryptionLayer can be used as a VirtualFile
/// in the file system layer (e.g., NAX decrypted file).
impl crate::file_sys::vfs::vfs::VfsFile for XtsEncryptionLayer {
    fn get_name(&self) -> String {
        self.base_layer.get_name()
    }

    fn get_size(&self) -> usize {
        self.base_layer.get_size()
    }

    fn resize(&self, new_size: usize) -> bool {
        self.base_layer.resize(new_size)
    }

    fn get_containing_directory(&self) -> Option<crate::file_sys::vfs::vfs_types::VirtualDir> {
        None
    }

    fn is_writable(&self) -> bool {
        self.base_layer.is_writable()
    }

    fn is_readable(&self) -> bool {
        self.base_layer.is_readable()
    }

    fn read(&self, data: &mut [u8], length: usize, offset: usize) -> usize {
        let read_len = length.min(data.len());
        // Delegate to the crypto VfsFile::read which takes (data, offset).
        <Self as VfsFile>::read(self, &mut data[..read_len], offset)
    }

    fn write(&self, data: &[u8], length: usize, offset: usize) -> usize {
        let write_len = length.min(data.len());
        self.base_layer.write(&data[..write_len], offset)
    }

    fn rename(&self, name: &str) -> bool {
        self.base_layer.rename(name)
    }
}
