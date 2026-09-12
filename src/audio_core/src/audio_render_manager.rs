use crate::adsp::adsp::AudioRendererHandle;
use crate::common::audio_renderer_parameter::AudioRendererParameterInternal;
use crate::common::common::MAX_RENDERER_SESSIONS;
use crate::common::feature_support::check_valid_revision;
use crate::errors::RESULT_INVALID_REVISION;
use crate::renderer::{System, SystemManager};
use crate::{Result, SharedSystem};
use common::ResultCode;
use parking_lot::Mutex;
use std::sync::Arc;

pub struct Manager {
    system: SharedSystem,
    session_ids: [i32; MAX_RENDERER_SESSIONS],
    session_count: u32,
    session_lock: Mutex<()>,
    system_manager: SystemManager,
}

impl Manager {
    pub fn new(system: SharedSystem, audio_renderer: AudioRendererHandle) -> Self {
        Self {
            system: system.clone(),
            session_ids: std::array::from_fn(|index| index as i32),
            session_count: 0,
            session_lock: Mutex::new(()),
            system_manager: SystemManager::new(system, audio_renderer),
        }
    }

    pub fn stop(&mut self) {
        self.system_manager.stop();
    }

    pub fn get_system_manager(&mut self) -> &mut SystemManager {
        &mut self.system_manager
    }

    pub fn get_work_buffer_size(
        &self,
        params: &AudioRendererParameterInternal,
        out_count: &mut u64,
    ) -> Result {
        if !check_valid_revision(params.revision) {
            return RESULT_INVALID_REVISION;
        }
        *out_count = System::get_work_buffer_size(params);
        ResultCode::SUCCESS
    }

    pub fn get_session_id(&mut self) -> i32 {
        let _lock = self.session_lock.lock();
        debug_assert!((self.session_count as usize) <= self.session_ids.len());
        let index = self.session_count as usize;
        // Upstream indexes `session_ids[session_count]` after
        // `ASSERT(session_count <= session_ids.size())`. When the pool is
        // full (`session_count == len`) that index is out of range; return -1
        // without mutating, matching the `session_id >= 0` guard.
        if index >= self.session_ids.len() {
            return -1;
        }
        let session_id = self.session_ids[index];
        if session_id >= 0 {
            self.session_ids[index] = -1;
            self.session_count += 1;
        }
        session_id
    }

    pub fn release_session_id(&mut self, session_id: i32) {
        let _lock = self.session_lock.lock();
        self.session_count = self.session_count.saturating_sub(1);
        self.session_ids[self.session_count as usize] = session_id;
    }

    pub fn get_session_count(&self) -> u32 {
        let _lock = self.session_lock.lock();
        self.session_count
    }

    pub fn add_system(&mut self, system: Arc<Mutex<System>>) -> bool {
        self.system_manager.add(system)
    }

    pub fn remove_system(&mut self, system: &Arc<Mutex<System>>) -> bool {
        self.system_manager.remove(system)
    }

    pub fn system(&self) -> SharedSystem {
        self.system.clone()
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adsp::apps::audio_renderer::AudioRenderer;
    use crate::common::common::MAX_RENDERER_SESSIONS;
    use crate::sink::null_sink::NullSink;
    use crate::sink::sink::new_sink_handle;

    fn make_manager() -> Manager {
        let system = crate::make_test_system();
        let audio_renderer = Arc::new(parking_lot::Mutex::new(AudioRenderer::new(
            system.clone(),
            new_sink_handle(Box::new(NullSink::new("test"))),
        )));
        Manager::new(system, audio_renderer)
    }

    #[test]
    fn get_session_id_exhausted_pool_does_not_increment_count() {
        let mut manager = make_manager();
        let mut ids = Vec::new();
        for _ in 0..MAX_RENDERER_SESSIONS {
            let session_id = manager.get_session_id();
            assert!(session_id >= 0);
            ids.push(session_id);
        }
        assert_eq!(ids, vec![0, 1]);
        assert_eq!(manager.get_session_count(), MAX_RENDERER_SESSIONS as u32);

        let exhausted = manager.get_session_id();
        assert_eq!(exhausted, -1);
        assert_eq!(manager.get_session_count(), MAX_RENDERER_SESSIONS as u32);
    }
}
