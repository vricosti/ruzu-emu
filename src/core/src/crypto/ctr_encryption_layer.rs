// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/crypto/ctr_encryption_layer.h and ctr_encryption_layer.cpp
//! Sits on top of a VirtualFile and provides CTR-mode AES decryption.

use parking_lot::Mutex;

use super::aes_util::{AesCipher, Mode, Op};
use super::encryption_layer::{EncryptionLayerBase, VfsFile, VirtualFile};
use super::key_manager::Key128;

/// IV data type for CTR mode (16 bytes).
pub type IvData = [u8; 16];

/// CTR-mode AES encryption layer.
/// Port of Core::Crypto::CTREncryptionLayer.
pub struct CtrEncryptionLayer {
    base_layer: EncryptionLayerBase,
    base_offset: usize,
    /// Must be interior-mutable because cipher state changes during read operations.
    cipher: Mutex<AesCipher>,
    iv: Mutex<IvData>,
}

impl CtrEncryptionLayer {
    pub fn new(base: VirtualFile, key: Key128, base_offset: usize) -> Self {
        let cipher = AesCipher::new_128(key, Mode::CTR);
        Self {
            base_layer: EncryptionLayerBase::new(base),
            base_offset,
            cipher: Mutex::new(cipher),
            iv: Mutex::new([0u8; 16]),
        }
    }

    pub fn set_iv(&self, iv: &IvData) {
        *self.iv.lock() = *iv;
    }

    fn update_iv(&self, offset: usize) {
        let mut iv = self.iv.lock();
        let mut shifted = offset >> 4;
        for i in 0..8 {
            iv[16 - i - 1] = (shifted & 0xFF) as u8;
            shifted >>= 8;
        }
        self.cipher.lock().set_iv(&*iv);
    }
}

impl VfsFile for CtrEncryptionLayer {
    fn read(&self, data: &mut [u8], offset: usize) -> usize {
        let length = data.len();
        if length == 0 {
            return 0;
        }

        let sector_offset = offset & 0xF;
        if sector_offset == 0 {
            self.update_iv(self.base_offset + offset);
            let raw = self.base_layer.base.read_bytes(length, offset);
            self.cipher.lock().transcode(&raw, data, Op::Decrypt);
            return length;
        }

        // Offset does not fall on block boundary (0x10)
        let block = self
            .base_layer
            .base
            .read_bytes(0x10, offset - sector_offset);
        self.update_iv(self.base_offset + offset - sector_offset);
        let mut decrypted = vec![0u8; block.len()];
        self.cipher
            .lock()
            .transcode(&block, &mut decrypted, Op::Decrypt);

        let read = 0x10 - sector_offset;

        if length + sector_offset < 0x10 {
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
