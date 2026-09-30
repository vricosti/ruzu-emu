use common::VAddr;
use parking_lot::Mutex;
use ruzu_core::memory::memory_manager::MemoryManager;
use std::sync::Arc;

pub trait GuestMemoryProvider: Send + Sync {
    fn read_bytes(&self, addr: VAddr, size: usize) -> Option<Vec<u8>>;
    fn write_bytes(&self, addr: VAddr, data: &[u8]) -> bool;

    fn read_i16_samples(&self, addr: VAddr, sample_count: usize) -> Option<Vec<i16>> {
        let bytes = self.read_bytes(addr, sample_count * std::mem::size_of::<i16>())?;
        Some(
            bytes
                .chunks_exact(std::mem::size_of::<i16>())
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                .collect(),
        )
    }

    fn write_i16_samples(&self, addr: VAddr, samples: &[i16]) -> bool {
        let mut bytes = Vec::with_capacity(std::mem::size_of_val(samples));
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.write_bytes(addr, &bytes)
    }
}

pub type SharedGuestMemory = Arc<dyn GuestMemoryProvider>;

pub struct KernelMemoryProvider {
    memory: Arc<Mutex<MemoryManager>>,
}

impl KernelMemoryProvider {
    pub fn new(memory: Arc<Mutex<MemoryManager>>) -> Self {
        Self { memory }
    }
}

impl GuestMemoryProvider for KernelMemoryProvider {
    fn read_bytes(&self, addr: VAddr, size: usize) -> Option<Vec<u8>> {
        self.memory.lock().read_bytes(addr, size).ok()
    }

    fn write_bytes(&self, addr: VAddr, data: &[u8]) -> bool {
        self.memory.lock().write_bytes(addr, data).is_ok()
    }
}

pub struct ProcessMemoryProvider {
    memory: Arc<ruzu_core::memory::memory::SharedMemory>,
}

impl ProcessMemoryProvider {
    pub fn new(memory: Arc<ruzu_core::memory::memory::SharedMemory>) -> Self {
        Self { memory }
    }
}

impl GuestMemoryProvider for ProcessMemoryProvider {
    fn read_bytes(&self, addr: VAddr, size: usize) -> Option<Vec<u8>> {
        let mut bytes = vec![0; size];
        self.memory
            .access()
            .ok()?
            .read_block(addr, &mut bytes)
            .then_some(bytes)
    }

    fn write_bytes(&self, addr: VAddr, data: &[u8]) -> bool {
        self.memory
            .access()
            .is_ok_and(|memory| memory.write_block(addr, data))
    }
}
