use crate::device::{AudioBuffer, DeviceSession};
use parking_lot::Mutex;

pub const BUFFER_APPEND_LIMIT: i32 = 4;

pub struct AudioBuffers<const N: usize> {
    inner: Mutex<AudioBuffersInner<N>>,
}

struct AudioBuffersInner<const N: usize> {
    buffers: [AudioBuffer; N],
    released_index: i32,
    released_count: i32,
    registered_index: i32,
    registered_count: i32,
    appended_index: i32,
    appended_count: i32,
    append_limit: u32,
}

impl<const N: usize> AudioBuffers<N> {
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Mutex::new(AudioBuffersInner {
                buffers: [AudioBuffer::default(); N],
                released_index: 0,
                released_count: 0,
                registered_index: 0,
                registered_count: 0,
                appended_index: 0,
                appended_count: 0,
                append_limit: limit as u32,
            }),
        }
    }

    pub fn append_buffer(&self, buffer: AudioBuffer) {
        let mut inner = self.inner.lock();
        let index = inner.appended_index as usize;
        inner.buffers[index] = buffer;
        inner.appended_count += 1;
        inner.appended_index = (inner.appended_index + 1) % inner.append_limit as i32;
    }

    pub fn register_buffers(&self, out_buffers: &mut Vec<AudioBuffer>) {
        let mut inner = self.inner.lock();
        let to_register = inner
            .appended_count
            .min(BUFFER_APPEND_LIMIT)
            .min(BUFFER_APPEND_LIMIT - inner.registered_count);

        for _ in 0..to_register {
            let mut index = inner.appended_index - inner.appended_count;
            if index < 0 {
                index += N as i32;
            }

            out_buffers.push(inner.buffers[index as usize]);
            inner.registered_count += 1;
            inner.registered_index = (inner.registered_index + 1) % inner.append_limit as i32;
            inner.appended_count -= 1;
            if inner.appended_count == 0 {
                break;
            }
        }
    }

    pub fn release_buffers(
        &self,
        current_time_ns: u64,
        session: &DeviceSession,
        force: bool,
    ) -> bool {
        let mut inner = self.inner.lock();
        let mut buffer_released = false;
        while inner.registered_count > 0 {
            let mut index = inner.registered_index - inner.registered_count;
            if index < 0 {
                index += N as i32;
            }

            if !force && !session.is_buffer_consumed(inner.buffers[index as usize]) {
                break;
            }

            let buffer = inner.buffers[index as usize];
            session.release_buffer(buffer);
            self.release_buffer_locked(&mut inner, index as usize, current_time_ns as i64);
            buffer_released = true;
        }

        buffer_released || inner.registered_count == 0
    }

    pub fn get_released_buffers(&self, tags: &mut [u64]) -> u32 {
        let mut inner = self.inner.lock();
        let mut released = 0u32;
        while inner.released_count > 0 {
            let mut index = inner.released_index - inner.released_count;
            if index < 0 {
                index += N as i32;
            }

            inner.released_count -= 1;
            let buffer = &mut inner.buffers[index as usize];
            let tag = buffer.tag;
            // Match upstream AudioBuffers::GetReleasedBuffers: the playback
            // range is retained because GetNextTimestamp reads the most
            // recently appended slot even after its tag has been returned.
            buffer.played_timestamp = 0;
            buffer.samples = 0;
            buffer.tag = 0;
            buffer.size = 0;
            if tag == 0 {
                break;
            }
            if let Some(slot) = tags.get_mut(released as usize) {
                *slot = tag;
            }
            released += 1;
            if released as usize >= tags.len() {
                break;
            }
        }
        released
    }

    pub fn contains_buffer(&self, tag: u64) -> bool {
        let inner = self.inner.lock();
        let registered_buffers =
            inner.appended_count + inner.registered_count + inner.released_count;
        if registered_buffers == 0 {
            return false;
        }

        let mut index = inner.released_index - inner.released_count;
        if index < 0 {
            index += inner.append_limit as i32;
        }

        for _ in 0..registered_buffers {
            if inner.buffers[index as usize].tag == tag {
                return true;
            }
            index = (index + 1) % inner.append_limit as i32;
        }
        false
    }

    pub fn get_appended_registered_count(&self) -> u32 {
        let inner = self.inner.lock();
        (inner.appended_count + inner.registered_count) as u32
    }

    pub fn get_total_buffer_count(&self) -> u32 {
        let inner = self.inner.lock();
        (inner.appended_count + inner.registered_count + inner.released_count) as u32
    }

    pub fn flush_buffers(&self, buffers_released: &mut u32) -> bool {
        let mut inner = self.inner.lock();
        let mut flushed = Vec::new();
        let append_limit = inner.append_limit;
        *buffers_released =
            self.get_registered_appended_buffers_locked(&mut inner, &mut flushed, append_limit);
        if inner.registered_count > 0 {
            return false;
        }
        (inner.released_count + inner.appended_count) as u32 <= inner.append_limit
    }

    pub fn get_next_timestamp(&self) -> u64 {
        let inner = self.inner.lock();
        let mut index = inner.appended_index - 1;
        if index < 0 {
            index += inner.append_limit as i32;
        }
        inner.buffers[index as usize].end_timestamp
    }

    fn get_registered_appended_buffers_locked(
        &self,
        inner: &mut AudioBuffersInner<N>,
        buffers_flushed: &mut Vec<AudioBuffer>,
        max_buffers: u32,
    ) -> u32 {
        if inner.registered_count + inner.appended_count == 0 {
            return 0;
        }

        let buffers_to_flush =
            ((inner.registered_count + inner.appended_count) as u32).min(max_buffers) as usize;
        while inner.registered_count > 0 && buffers_flushed.len() < buffers_to_flush {
            let mut index = inner.registered_index - inner.registered_count;
            if index < 0 {
                index += N as i32;
            }
            buffers_flushed.push(inner.buffers[index as usize]);
            inner.registered_count -= 1;
            inner.released_count += 1;
            inner.released_index = (inner.released_index + 1) % inner.append_limit as i32;
        }

        while inner.appended_count > 0 && buffers_flushed.len() < buffers_to_flush {
            let mut index = inner.appended_index - inner.appended_count;
            if index < 0 {
                index += N as i32;
            }
            buffers_flushed.push(inner.buffers[index as usize]);
            inner.appended_count -= 1;
            inner.released_count += 1;
            inner.released_index = (inner.released_index + 1) % inner.append_limit as i32;
        }

        buffers_flushed.len() as u32
    }

    fn release_buffer_locked(
        &self,
        inner: &mut AudioBuffersInner<N>,
        index: usize,
        timestamp: i64,
    ) {
        inner.buffers[index].played_timestamp = timestamp;
        inner.registered_count -= 1;
        inner.released_count += 1;
        inner.released_index = (inner.released_index + 1) % inner.append_limit as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::common::SampleFormat;
    use crate::device::KernelMemoryProvider;
    use crate::sink::sink::new_sink_handle;
    use crate::sink::{NullSink, StreamType};
    use crate::SharedSystem;
    use parking_lot::Mutex;
    use ruzu_core::memory::memory_manager::{MemoryManager, MemoryPermission, MemoryState};
    use std::sync::Arc;

    fn make_system() -> SharedSystem {
        crate::make_test_system()
    }

    fn make_guest_memory() -> Arc<Mutex<MemoryManager>> {
        let mut memory = MemoryManager::with_capacity(0x10000, 0x100).unwrap();
        memory
            .map(
                0x1000,
                0x1000,
                MemoryPermission::READ_WRITE,
                MemoryState::Normal,
            )
            .unwrap();
        Arc::new(Mutex::new(memory))
    }

    #[test]
    fn release_buffers_writes_audio_in_samples_before_releasing_tag() {
        let system = make_system();
        let sink = new_sink_handle(Box::new(NullSink::new_recording_for_test("null")));
        let guest_memory = make_guest_memory();
        let mut session = DeviceSession::new(system.clone());
        session.initialize(
            sink.clone(),
            "BuiltInHeadset",
            SampleFormat::PcmInt16,
            2,
            0,
            0,
            StreamType::In,
            Some(Arc::new(KernelMemoryProvider::new(guest_memory.clone()))),
        );

        let buffers = AudioBuffers::<4>::new(4);
        buffers.append_buffer(AudioBuffer {
            start_timestamp: 0,
            end_timestamp: 2,
            played_timestamp: 0,
            samples: 0x1000,
            tag: 0xAA,
            size: 8,
        });

        let mut registered = Vec::new();
        buffers.register_buffers(&mut registered);
        session.append_buffers(&registered);

        let stream = sink
            .lock()
            .acquire_sink_stream(system, 2, "BuiltInHeadset-0", StreamType::In);
        stream.lock().process_audio_in(&[10, 11, 12, 13], 2);

        assert!(buffers.release_buffers(1234, &session, true));

        let bytes = guest_memory.lock().read_bytes(0x1000, 8).unwrap();
        assert_eq!(bytes, vec![80, 0, 88, 0, 96, 0, 104, 0]);

        let mut tags = [0u64; 1];
        assert_eq!(buffers.get_released_buffers(&mut tags), 1);
        assert_eq!(tags[0], 0xAA);
    }

    #[test]
    fn getting_released_tag_preserves_next_buffer_timestamp_like_upstream() {
        let buffers = AudioBuffers::<4>::new(4);
        buffers.append_buffer(AudioBuffer {
            start_timestamp: 10,
            end_timestamp: 1034,
            played_timestamp: 77,
            samples: 0x1000,
            tag: 0xAA,
            size: 4096,
        });

        {
            let mut inner = buffers.inner.lock();
            inner.appended_count = 0;
            inner.released_count = 1;
            inner.released_index = 1;
        }

        let mut tags = [0u64; 1];
        assert_eq!(buffers.get_released_buffers(&mut tags), 1);
        assert_eq!(tags[0], 0xAA);
        assert_eq!(buffers.get_next_timestamp(), 1034);

        let inner = buffers.inner.lock();
        assert_eq!(inner.buffers[0].start_timestamp, 10);
        assert_eq!(inner.buffers[0].end_timestamp, 1034);
        assert_eq!(inner.buffers[0].played_timestamp, 0);
        assert_eq!(inner.buffers[0].samples, 0);
        assert_eq!(inner.buffers[0].tag, 0);
        assert_eq!(inner.buffers[0].size, 0);
    }
}
