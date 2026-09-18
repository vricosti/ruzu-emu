// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal library reuse; Eden has no corresponding Metal compiler cache.
//! Device ownership fixes the compiler/device identity. Language version and the
//! complete source form the key; math mode and position invariance are fixed by
//! compile_msl_library.
//! New variable compiler options must be added to the key before being exposed.

use super::metal_shader::{compile_msl_library, MetalShaderError};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{MTLDevice, MTLFunction, MTLLibrary};
use shader_recompiler::backend::msl::MslVersion;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

type Library = Retained<ProtocolObject<dyn MTLLibrary>>;
type Function = Retained<ProtocolObject<dyn MTLFunction>>;
type Version = (u8, u8);
const MAX_LIBRARIES: usize = 2048;
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct Compiled {
    library: Option<Library>,
    functions: HashMap<String, Function>,
}

#[derive(Default)]
struct Entries {
    by_version: HashMap<Version, HashMap<Arc<str>, Arc<Mutex<Compiled>>>>,
    fifo: VecDeque<(Version, Arc<str>)>,
    source_bytes: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl Entries {
    fn acquire(&mut self, source: &str, version: MslVersion) -> Arc<Mutex<Compiled>> {
        let version = (version.major, version.minor);
        if let Some(entry) = self.by_version.get(&version).and_then(|m| m.get(source)) {
            self.hits += 1;
            return entry.clone();
        }
        self.misses += 1;
        let entry = Arc::new(Mutex::new(Compiled::default()));
        if source.len() > MAX_SOURCE_BYTES {
            return entry;
        }
        while self.fifo.len() >= MAX_LIBRARIES
            || self.source_bytes + source.len() > MAX_SOURCE_BYTES
        {
            let (old_version, old_source) = self.fifo.pop_front().unwrap();
            self.by_version
                .get_mut(&old_version)
                .unwrap()
                .remove(&old_source);
            self.source_bytes -= old_source.len();
            self.evictions += 1;
        }
        let source: Arc<str> = source.into();
        self.source_bytes += source.len();
        self.by_version
            .entry(version)
            .or_default()
            .insert(source.clone(), entry.clone());
        self.fifo.push_back((version, source));
        entry
    }
}

pub(super) struct MetalShaderCache {
    entries: Mutex<Entries>,
    profile: bool,
}

impl Default for MetalShaderCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            profile: std::env::var_os("RUZU_PROFILE_METAL_STALLS").is_some(),
        }
    }
}

impl MetalShaderCache {
    fn acquire(&self, source: &str, version: MslVersion) -> Arc<Mutex<Compiled>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.acquire(source, version);
        if self.profile && (entries.hits + entries.misses).is_multiple_of(128) {
            log::info!(
                "[METAL_SHADER_CACHE] lookup_hits={} lookup_misses={} evictions={} retained={} source_bytes={}",
                entries.hits,
                entries.misses,
                entries.evictions,
                entries.fifo.len(),
                entries.source_bytes
            );
        }
        entry
    }

    pub(super) fn library(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        source: &str,
        version: MslVersion,
    ) -> Result<Library, MetalShaderError> {
        let entry = self.acquire(source, version);
        let mut compiled = entry.lock().unwrap();
        Self::compile(&mut compiled, device, source, version)
    }

    fn compile(
        compiled: &mut Compiled,
        device: &ProtocolObject<dyn MTLDevice>,
        source: &str,
        version: MslVersion,
    ) -> Result<Library, MetalShaderError> {
        if let Some(library) = &compiled.library {
            return Ok(library.clone());
        }
        // Only the same source waits here. The map mutex is never held during
        // compilation. Errors leave this entry retryable, not a negative cache.
        let library = compile_msl_library(device, source, version)?;
        compiled.library = Some(library.clone());
        Ok(library)
    }

    pub(super) fn function(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        source: &str,
        version: MslVersion,
        name: &str,
    ) -> Result<(Library, Function), MetalShaderError> {
        let entry = self.acquire(source, version);
        let mut compiled = entry.lock().unwrap();
        let library = Self::compile(&mut compiled, device, source, version)?;
        if let Some(function) = compiled.functions.get(name) {
            return Ok((library, function.clone()));
        }
        let function = library
            .newFunctionWithName(&NSString::from_str(name))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint(name.into()))?;
        compiled.functions.insert(name.into(), function.clone());
        Ok((library, function))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_source_version_and_bounded_retention() {
        let mut entries = Entries::default();
        let original = entries.acquire("source", MslVersion::V2_3);
        assert!(Arc::ptr_eq(
            &original,
            &entries.acquire("source", MslVersion::V2_3)
        ));
        assert!(!Arc::ptr_eq(
            &original,
            &entries.acquire("source", MslVersion::V3_0)
        ));
        assert!(!Arc::ptr_eq(
            &original,
            &entries.acquire("source ", MslVersion::V2_3)
        ));
        for i in 0..MAX_LIBRARIES {
            entries.acquire(&format!("{i}"), MslVersion::V2_3);
        }
        assert_eq!(entries.fifo.len(), MAX_LIBRARIES);
        assert_eq!(entries.evictions, 3);
        assert!(!Arc::ptr_eq(
            &original,
            &entries.acquire("source", MslVersion::V2_3)
        ));
        let large = "x".repeat(MAX_SOURCE_BYTES);
        entries.acquire(&large, MslVersion::V2_3);
        assert_eq!(entries.fifo.len(), 1);
        assert_eq!(entries.source_bytes, MAX_SOURCE_BYTES);
        entries.acquire(&(large + "x"), MslVersion::V2_3);
        assert_eq!(entries.fifo.len(), 1);
    }

    #[test]
    fn native_functions_are_reused_across_device_clones_and_parallel_requests() {
        let device = super::super::metal_device::MetalDevice::new().unwrap();
        let source = "#include <metal_stdlib>\nusing namespace metal;\nkernel void first() {}\nkernel void second() {}";
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let device = device.clone();
                std::thread::spawn(move || {
                    device
                        .shader_cache()
                        .function(device.device(), source, MslVersion::V2_3, "first")
                        .unwrap()
                })
            })
            .collect();
        let values: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        for (library, function) in &values {
            assert_eq!(Retained::as_ptr(library), Retained::as_ptr(&values[0].0));
            assert_eq!(Retained::as_ptr(function), Retained::as_ptr(&values[0].1));
        }
        let (_, other) = device
            .shader_cache()
            .function(device.device(), source, MslVersion::V2_3, "second")
            .unwrap();
        assert_ne!(Retained::as_ptr(&other), Retained::as_ptr(&values[0].1));
        assert!(device
            .shader_cache()
            .function(device.device(), source, MslVersion::V2_3, "missing")
            .is_err());
        assert!(device
            .shader_cache()
            .function(device.device(), source, MslVersion::V2_3, "first")
            .is_ok());
        for _ in 0..2 {
            assert!(device
                .shader_cache()
                .library(device.device(), "invalid MSL", MslVersion::V2_3)
                .is_err());
        }
    }
}
