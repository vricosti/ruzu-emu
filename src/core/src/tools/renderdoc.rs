// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of core/tools/renderdoc.h and renderdoc.cpp.
//! RenderDoc API integration for frame capture.

use std::ffi::c_void;

type StartFrameCapture = unsafe extern "C" fn(*mut c_void, *mut c_void);
type EndFrameCapture = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

/// Prefix of renderdoc_app.h's API table through EndFrameCapture. The first
/// nineteen entries (GetAPIVersion through SetActiveWindow) are unused here.
/// API 1.6.0 preserves this prefix; all slots use RENDERDOC_CC (cdecl).
#[repr(C)]
struct ApiPrefix {
    unused: [*const c_void; 19],
    start_frame_capture: StartFrameCapture,
    is_frame_capturing: unsafe extern "C" fn() -> u32,
    end_frame_capture: EndFrameCapture,
}

struct RdocApi {
    start_frame_capture: StartFrameCapture,
    end_frame_capture: EndFrameCapture,
}

/// RenderDoc API wrapper.
///
/// Corresponds to upstream `Tools::RenderdocAPI`.
pub struct RenderdocApi {
    rdoc_api: Option<RdocApi>,
    is_capturing: bool,
}

impl RenderdocApi {
    /// Create a new RenderDoc API instance.
    ///
    /// Attempts to load the RenderDoc shared library and retrieve the API entry point.
    /// If RenderDoc is not loaded into the process, the API will be unavailable and
    /// `toggle_capture` will be a no-op.
    ///
    /// Corresponds to upstream `RenderdocAPI::RenderdocAPI()`.
    pub fn new() -> Self {
        let api = Self::try_load_api();
        Self {
            rdoc_api: api,
            is_capturing: false,
        }
    }

    /// Toggle frame capture on/off.
    ///
    /// Corresponds to upstream `RenderdocAPI::ToggleCapture()`.
    pub fn toggle_capture(&mut self) {
        let Some(api) = &self.rdoc_api else { return };
        // SAFETY: callbacks come from a successfully negotiated, resident API.
        // Null device/window select RenderDoc's active target, as upstream does.
        unsafe {
            if !self.is_capturing {
                (api.start_frame_capture)(std::ptr::null_mut(), std::ptr::null_mut());
            } else {
                (api.end_frame_capture)(std::ptr::null_mut(), std::ptr::null_mut());
            }
        }
        self.is_capturing = !self.is_capturing;
    }

    /// Attempt to load the RenderDoc API from the process.
    ///
    /// On Linux: tries `dlopen("librenderdoc.so", RTLD_NOW | RTLD_NOLOAD)`.
    /// On Android: tries `dlopen("libVkLayer_GLES_RenderDoc.so", ...)`.
    /// On Windows: tries `GetModuleHandleA("renderdoc.dll")`.
    ///
    /// Returns `None` if RenderDoc is not loaded.
    fn try_load_api() -> Option<RdocApi> {
        #[cfg(all(unix, not(target_os = "haiku")))]
        {
            // Try to load RenderDoc if it's already in the process.
            // SAFETY: dlopen with RTLD_NOLOAD only checks if already loaded.
            unsafe {
                #[cfg(target_os = "android")]
                const RENDERDOC_LIB: &[u8] = b"libVkLayer_GLES_RenderDoc.so\0";
                #[cfg(not(target_os = "android"))]
                const RENDERDOC_LIB: &[u8] = b"librenderdoc.so\0";

                let handle = libc::dlopen(
                    RENDERDOC_LIB.as_ptr() as *const libc::c_char,
                    libc::RTLD_NOW | libc::RTLD_NOLOAD,
                );
                if handle.is_null() {
                    return None;
                }

                let get_api_sym = libc::dlsym(
                    handle,
                    b"RENDERDOC_GetAPI\0".as_ptr() as *const libc::c_char,
                );
                if get_api_sym.is_null() {
                    libc::dlclose(handle);
                    return None;
                }

                // Call RENDERDOC_GetAPI(eRENDERDOC_API_Version_1_6_0, &api_ptr).
                // The function signature is: int RENDERDOC_GetAPI(int version, void** api_ptr)
                // eRENDERDOC_API_Version_1_6_0 = 10600
                type GetApiFn =
                    unsafe extern "C" fn(version: i32, api_ptr: *mut *mut std::ffi::c_void) -> i32;
                let get_api: GetApiFn = std::mem::transmute(get_api_sym);
                let mut api_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
                let ret = get_api(10600, &mut api_ptr);
                if ret != 1 || api_ptr.is_null() {
                    log::warn!("RENDERDOC_GetAPI failed (ret={})", ret);
                    libc::dlclose(handle);
                    return None;
                }
                log::info!("RenderDoc API loaded successfully");
                // Keep the successful dlopen reference resident, as upstream
                // does, so these callbacks cannot outlive their library.
                let api = &*api_ptr.cast::<ApiPrefix>();
                Some(RdocApi {
                    start_frame_capture: api.start_frame_capture,
                    end_frame_capture: api.end_frame_capture,
                })
            }
        }

        #[cfg(windows)]
        unsafe {
            use winapi::um::libloaderapi::{GetModuleHandleA, GetProcAddress};
            let module = GetModuleHandleA(c"renderdoc.dll".as_ptr());
            if module.is_null() {
                return None;
            }
            let symbol = GetProcAddress(module, c"RENDERDOC_GetAPI".as_ptr());
            if symbol.is_null() {
                return None;
            }
            let get_api: unsafe extern "C" fn(i32, *mut *mut c_void) -> i32 =
                std::mem::transmute(symbol);
            let mut api_ptr = std::ptr::null_mut();
            if get_api(10600, &mut api_ptr) != 1 || api_ptr.is_null() {
                return None;
            }
            let api = &*api_ptr.cast::<ApiPrefix>();
            Some(RdocApi {
                start_frame_capture: api.start_frame_capture,
                end_frame_capture: api.end_frame_capture,
            })
        }

        #[cfg(not(any(windows, all(unix, not(target_os = "haiku")))))]
        {
            None
        }
    }
}

impl Default for RenderdocApi {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn unavailable_api_does_not_enter_capture() {
        let mut api = RenderdocApi {
            rdoc_api: None,
            is_capturing: false,
        };
        api.toggle_capture();
        api.toggle_capture();
        assert!(!api.is_capturing);
    }

    #[test]
    fn toggle_calls_start_end_in_order_with_active_target() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static INVALID: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn start(device: *mut c_void, window: *mut c_void) {
            if !device.is_null()
                || !window.is_null()
                || CALLS.fetch_add(1, Ordering::SeqCst) % 2 != 0
            {
                INVALID.fetch_add(1, Ordering::SeqCst);
            }
        }
        unsafe extern "C" fn end(device: *mut c_void, window: *mut c_void) -> u32 {
            if !device.is_null()
                || !window.is_null()
                || CALLS.fetch_add(1, Ordering::SeqCst) % 2 != 1
            {
                INVALID.fetch_add(1, Ordering::SeqCst);
            }
            // Upstream toggles its state even when capture failed.
            0
        }
        let mut api = RenderdocApi {
            rdoc_api: Some(RdocApi {
                start_frame_capture: start,
                end_frame_capture: end,
            }),
            is_capturing: false,
        };
        for _ in 0..2 {
            api.toggle_capture();
            assert!(api.is_capturing);
            api.toggle_capture();
            assert!(!api.is_capturing);
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 4);
        assert_eq!(INVALID.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn api_prefix_offsets_match_renderdoc_app_header() {
        let pointer = std::mem::size_of::<*const c_void>();
        assert_eq!(
            std::mem::offset_of!(ApiPrefix, start_frame_capture),
            19 * pointer
        );
        assert_eq!(
            std::mem::offset_of!(ApiPrefix, is_frame_capturing),
            20 * pointer
        );
        assert_eq!(
            std::mem::offset_of!(ApiPrefix, end_frame_capture),
            21 * pointer
        );
        assert_eq!(std::mem::size_of::<ApiPrefix>(), 22 * pointer);
    }
}
