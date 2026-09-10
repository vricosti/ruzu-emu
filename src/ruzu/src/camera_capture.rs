//! Frontend acquisition counterpart of GRenderWindow's camera methods in
//! Eden yuzu/bootmanager.{h,cpp}. SDL replaces Qt Multimedia; the existing
//! InputCommon Camera remains responsible for guest-format delivery.

use sdl3_sys::{camera::*, error::SDL_GetError, init::*, pixels::*, stdinc::SDL_free, surface::*};
use std::{ffi::CStr, ptr::NonNull};

fn error() -> String {
    unsafe { CStr::from_ptr(SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

struct Subsystem;
impl Subsystem {
    fn new() -> Result<Self, String> {
        if unsafe { SDL_InitSubSystem(SDL_INIT_CAMERA) } {
            Ok(Self)
        } else {
            Err(error())
        }
    }
}
impl Drop for Subsystem {
    fn drop(&mut self) {
        unsafe { SDL_QuitSubSystem(SDL_INIT_CAMERA) };
    }
}

#[derive(Clone)]
pub struct Device {
    pub name: String,
    id: SDL_CameraID,
}
impl Device {
    // SDL instance IDs expire at shutdown. Names are persistent but may collide;
    // explicit ambiguous selections are rejected rather than opening a camera
    // the user did not select. Qt IDs are not silently interpreted as SDL IDs.
    pub fn setting_id(&self) -> String {
        format!("sdl-name:{}", self.name)
    }
}

fn devices() -> Result<Vec<Device>, String> {
    let mut count = 0;
    let ids = NonNull::new(unsafe { SDL_GetCameras(&mut count) }).ok_or_else(error)?;
    let result = unsafe { std::slice::from_raw_parts(ids.as_ptr(), count.max(0) as usize) }
        .iter()
        .filter_map(|&id| {
            let name = unsafe { SDL_GetCameraName(id) };
            if name.is_null() {
                return None;
            }
            Some(Device {
                id,
                name: unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned(),
            })
        })
        .collect();
    unsafe { SDL_free(ids.as_ptr().cast()) };
    Ok(result)
}

pub fn available_devices() -> Result<Vec<Device>, String> {
    let _subsystem = Subsystem::new()?;
    devices()
}

fn select_device(devices: &[Device], selected: &str) -> Result<usize, String> {
    if selected == "auto" {
        return (!devices.is_empty())
            .then_some(0)
            .ok_or_else(|| "No camera available".into());
    }
    let matches: Vec<_> = devices
        .iter()
        .enumerate()
        .filter(|(_, device)| device.setting_id() == selected)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err("Configured camera unavailable; select a camera again".into()),
        _ => Err("Camera name is ambiguous; automatic selection is required".into()),
    }
}

pub struct Capture {
    camera: NonNull<SDL_Camera>,
    _subsystem: Subsystem,
}
impl Capture {
    pub fn open(selected: &str) -> Result<Self, String> {
        let subsystem = Subsystem::new()?;
        let devices = devices()?;
        let index = select_device(&devices, selected)?;
        let camera = NonNull::new(unsafe { SDL_OpenCamera(devices[index].id, std::ptr::null()) })
            .ok_or_else(error)?;
        Ok(Self {
            camera,
            _subsystem: subsystem,
        })
    }

    /// Nonblocking acquisition. Permission pending and no new frame are normal.
    pub fn frame(&mut self, width: usize, height: usize) -> Result<Option<Vec<u32>>, String> {
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let w = i32::try_from(width).map_err(|_| "Invalid camera width")?;
        let h = i32::try_from(height).map_err(|_| "Invalid camera height")?;
        let permission = unsafe { SDL_GetCameraPermissionState(self.camera.as_ptr()) };
        if permission == SDL_CAMERA_PERMISSION_STATE_DENIED {
            return Err("Camera permission denied".into());
        }
        if permission == SDL_CAMERA_PERMISSION_STATE_PENDING {
            return Ok(None);
        }
        let Some(surface) = NonNull::new(unsafe {
            SDL_AcquireCameraFrame(self.camera.as_ptr(), std::ptr::null_mut())
        }) else {
            return Ok(None);
        };
        let frame = Frame {
            camera: self.camera,
            surface,
        };
        let converted = Surface(
            NonNull::new(unsafe {
                SDL_ConvertSurface(frame.surface.as_ptr(), SDL_PIXELFORMAT_ARGB8888)
            })
            .ok_or_else(error)?,
        );
        let scaled = Surface(
            NonNull::new(unsafe {
                SDL_ScaleSurface(converted.0.as_ptr(), w, h, SDL_SCALEMODE_LINEAR)
            })
            .ok_or_else(error)?,
        );
        let surface = unsafe { scaled.0.as_ref() };
        let pitch = usize::try_from(surface.pitch).map_err(|_| "Invalid camera pitch")?;
        let length = pitch.checked_mul(height).ok_or("Camera frame too large")?;
        if surface.pixels.is_null() {
            return Err("Missing camera pixels".into());
        }
        // Owned software surfaces returned by Convert/ScaleSurface are readable
        // until their guards are dropped. Copy rows, not potentially padded u32s.
        let bytes = unsafe { std::slice::from_raw_parts(surface.pixels.cast::<u8>(), length) };
        copy_flipped_rows(bytes, width, height, pitch).map(Some)
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { SDL_CloseCamera(self.camera.as_ptr()) };
    }
}
struct Frame {
    camera: NonNull<SDL_Camera>,
    surface: NonNull<SDL_Surface>,
}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { SDL_ReleaseCameraFrame(self.camera.as_ptr(), self.surface.as_ptr()) };
    }
}
struct Surface(NonNull<SDL_Surface>);
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { SDL_DestroySurface(self.0.as_ptr()) };
    }
}

fn copy_flipped_rows(
    bytes: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
) -> Result<Vec<u32>, String> {
    let row = width.checked_mul(4).ok_or("Camera row too large")?;
    let length = pitch.checked_mul(height).ok_or("Camera frame too large")?;
    if pitch < row || bytes.len() < length {
        return Err("Invalid camera frame stride".into());
    }
    let mut pixels = Vec::with_capacity(width.checked_mul(height).ok_or("Camera frame too large")?);
    for y in (0..height).rev() {
        for pixel in bytes[y * pitch..y * pitch + row].chunks_exact(4) {
            pixels.push(u32::from_ne_bytes(pixel.try_into().unwrap()));
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn padded_frames_flip_vertically_without_copying_padding() {
        let bytes: Vec<u8> = [1u32, 2, 99, 3, 4, 99]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        assert_eq!(copy_flipped_rows(&bytes, 2, 2, 12).unwrap(), [3, 4, 1, 2]);
        assert!(copy_flipped_rows(&bytes, 3, 2, 8).is_err());
        assert!(copy_flipped_rows(&bytes[..16], 2, 2, 12).is_err());
    }
    #[test]
    fn selection_never_falls_back_from_an_unavailable_explicit_device() {
        let device = Device {
            name: "Synthetic camera".into(),
            id: SDL_CameraID(1),
        };
        assert_eq!(select_device(&[device.clone()], "auto").unwrap(), 0);
        assert_eq!(
            select_device(&[device.clone()], &device.setting_id()).unwrap(),
            0
        );
        assert!(select_device(&[device.clone()], "old-qt-camera-id").is_err());
        assert!(select_device(&[device.clone(), device.clone()], &device.setting_id()).is_err());
        assert!(select_device(&[], "auto").is_err());
    }
}
