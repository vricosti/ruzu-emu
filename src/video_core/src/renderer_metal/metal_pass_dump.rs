// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded attachment readbacks for native Metal investigation (no Eden equivalent).
//! Copies follow existing render-pass ends; they do not split passes or wait for GPU idle.

use std::path::PathBuf;
use std::io::Write;
use block2::RcBlock;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSCopying;
use objc2_metal::*;

const MAX_PASSES: usize = 2048;
const MAX_BYTES: usize = 256 * 1024 * 1024;

pub(super) struct MetalPassDump {
    directory: PathBuf,
    armed: bool,
    passes: usize,
    bytes: usize,
    descriptor: Option<Retained<MTLRenderPassDescriptor>>,
    shaders: Vec<[u64; 6]>,
    shader_filter: Option<u64>,
    inputs: Vec<(String, Retained<ProtocolObject<dyn MTLTexture>>)>,
    input_metadata: String,
    image_map_written: bool,
    events: Vec<String>,
    events_seen: usize,
}

impl MetalPassDump {
    pub(super) fn from_environment() -> Option<Self> {
        let directory = PathBuf::from(std::env::var_os("RUZU_METAL_PASS_DUMP_DIR")?);
        if !directory.is_absolute() || std::fs::create_dir(&directory).is_err() {
            log::error!("Metal pass dump requires a fresh absolute directory: {directory:?}");
            return None;
        }
        log::info!("Metal pass dump armed: create {:?}", directory.join("capture.request"));
        let shader_filter = std::env::var("RUZU_METAL_PASS_DUMP_FRAGMENT").ok()
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok());
        Some(Self { directory, armed: false, passes: 0, bytes: 0, descriptor: None, shaders: Vec::new(),
            shader_filter, inputs: Vec::new(), input_metadata: String::new(),
            image_map_written: false, events: Vec::new(), events_seen: 0 })
    }

    pub(super) fn begin(&mut self, descriptor: &MTLRenderPassDescriptor) {
        if !self.armed {
            self.armed = self.directory.join("capture.request").is_file();
        }
        if self.armed && self.passes < MAX_PASSES && self.bytes < MAX_BYTES {
            self.descriptor = Some(descriptor.copy());
            self.shaders.clear();
            self.inputs.clear();
            self.input_metadata.clear();
        }
    }

    pub(super) fn draw(&mut self, shaders: [u64; 6]) {
        if self.descriptor.is_some() && self.shaders.len() < 64 && !self.shaders.contains(&shaders) {
            self.shaders.push(shaders);
        }
    }

    pub(super) fn resources(
        &mut self,
        shaders: [u64; 6],
        prepared: &super::metal_graphics_pipeline::MetalPreparedGraphics,
        cache: &super::metal_texture_cache::MetalTextureCache,
    ) {
        use super::metal_graphics_pipeline::MetalTextureBindingSource;
        if self.descriptor.is_none() || self.shader_filter != Some(shaders[5]) { return; }
        if !self.image_map_written {
            self.image_map_written = true;
            let mut map = String::new();
            for (id, image) in cache.base.slot_images.iter().take(4096) {
                if let Some(native) = &image.backend {
                    map.push_str(&format!("image={} root={:p} gpu=0x{:X} cpu=0x{:X} info={:?}\n",
                        id.index, native.handle(), image.gpu_addr, image.cpu_addr, image.info));
                }
            }
            if let Err(error) = std::fs::write(self.directory.join("images.txt"), map) {
                log::error!("Cannot write Metal diagnostic image map: {error}");
            }
        }
        for binding in &prepared.fragment.textures {
            let Some(texture) = &binding.texture else { continue; };
            let name = format!("input-f{}", binding.index);
            if self.inputs.iter().any(|(old_name, old_texture)| old_name == &name && std::ptr::eq(&**old_texture, &**texture)) {
                continue;
            }
            if self.inputs.len() == 64 { break; }
            let unique_name = format!("{name}-{}", self.inputs.len());
            let mut source = format!("{:?}", binding.source);
            if let MetalTextureBindingSource::Sampled { view_id, .. } = binding.source {
                if cache.base.slot_image_views.contains(view_id) {
                    let view = &cache.base.slot_image_views[view_id];
                    if cache.base.slot_images.contains(view.image_id) {
                        let image = &cache.base.slot_images[view.image_id];
                        source.push_str(&format!(" image={} gpu=0x{:X} cpu=0x{:X} info={:?}",
                            view.image_id.index, image.gpu_addr, image.cpu_addr, image.info));
                    }
                }
            }
            self.input_metadata.push_str(&format!("{unique_name} shader={:016X} source={source}\n", shaders[5]));
            self.inputs.push((unique_name, texture.clone()));
        }
    }

    pub(super) fn event(&mut self, args: std::fmt::Arguments<'_>) {
        if self.armed && self.passes < MAX_PASSES && self.events_seen < 1024 {
            self.events_seen += 1;
            self.events.push(format!("before_pass={} {args}", self.passes));
        }
    }

    pub(super) fn end(&mut self, command_buffer: &ProtocolObject<dyn MTLCommandBuffer>) {
        let Some(descriptor) = self.descriptor.take() else { return; };
        let pass = self.passes;
        self.passes += 1;
        if !self.events.is_empty() {
            let result = (|| -> std::io::Result<()> {
                let mut file = std::fs::OpenOptions::new().append(true).create(true)
                    .open(self.directory.join("operations.txt"))?;
                for event in self.events.drain(..) { writeln!(file, "{event}")?; }
                Ok(())
            })();
            if let Err(error) = result { log::error!("Metal diagnostic operations: {error}"); }
        }
        let mut attachments = Vec::new();
        for index in 0..crate::texture_cache::types::NUM_RT {
            let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(index) };
            attachments.push((format!("color{index}"), color.texture(), color.level(), color.slice(), color.storeAction()));
        }
        let depth = descriptor.depthAttachment();
        attachments.push(("depth".to_owned(), depth.texture(), depth.level(), depth.slice(), depth.storeAction()));
        let mut metadata = format!("pass={pass} shaders={:X?}\n", self.shaders);
        metadata.push_str(&self.input_metadata);
        // These read-only shader inputs are observed after consumption. A
        // feedback alias can have changed during the pass; preserve identities
        // in metadata rather than claiming these are pre-draw snapshots.
        attachments.extend(self.inputs.drain(..).map(|(name, texture)| (name, Some(texture), 0, 0, MTLStoreAction::Store)));
        let mut copies = Vec::new();
        for (name, texture, level, slice, store) in attachments {
            let Some(texture) = texture else { continue; };
            let format = texture.pixelFormat();
            let width = (texture.width() >> level).max(1);
            let height = (texture.height() >> level).max(1);
            let (root, root_level, root_slice) = texture_root(&texture);
            metadata.push_str(&format!("{name}: object={:p} format={} width={width} height={height} level={level} slice={slice} samples={} store={} type={} layers={} root=0x{root:X} root_level={root_level} root_slice={root_slice}\n", &*texture, format.0, texture.sampleCount(), store.0, texture.textureType().0, descriptor.renderTargetArrayLength()));
            if self.shader_filter.is_some_and(|hash| !self.shaders.iter().any(|stages| stages[5] == hash)) {
                metadata.push_str(" skipped: fragment shader filter\n");
                continue;
            }
            let Some((bpp, options)) = transfer_format(format) else {
                metadata.push_str(" skipped: unsupported pixel format\n"); continue;
            };
            if texture.sampleCount() != 1 || !matches!(texture.textureType(), MTLTextureType::Type2D | MTLTextureType::Type2DArray)
                || descriptor.renderTargetArrayLength() > 1
                || texture.storageMode() == MTLStorageMode::Memoryless || store != MTLStoreAction::Store {
                metadata.push_str(" skipped: not a stored single-sample 2D attachment\n"); continue;
            }
            let Some((pitch, length)) = layout(width, height, bpp, MAX_BYTES - self.bytes) else {
                metadata.push_str(" skipped: byte budget\n"); continue;
            };
            let Some(buffer) = texture.device().newBufferWithLength_options(length, MTLResourceOptions::StorageModeShared) else {
                metadata.push_str(" skipped: allocation failure\n"); continue;
            };
            self.bytes += length;
            metadata.push_str(&format!(" row_pitch={pitch} bytes={length} file=pass-{pass:03}-{name}.raw\n"));
            copies.push((name, texture, buffer, level, slice, width, height, pitch, length, options));
        }
        if copies.is_empty() {
            let _ = std::fs::write(self.directory.join(format!("pass-{pass:03}.txt")), metadata);
            return;
        }
        let Some(encoder) = command_buffer.blitCommandEncoder() else {
            log::error!("Cannot encode Metal pass dump {pass}"); return;
        };
        for (_, texture, buffer, level, slice, width, height, pitch, length, options) in &copies {
            // Source is stored by the preceding, now-ended render encoder. Metal's
            // tracked hazards order these copies before subsequent writes to it.
            unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage_options(
                    texture, *slice, *level, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: *width, height: *height, depth: 1 },
                    buffer, 0, *pitch, *length, *options,
                );
            }
        }
        encoder.endEncoding();
        let directory = self.directory.clone();
        let handler = RcBlock::new(move |completed: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            if unsafe { completed.as_ref() }.status() != MTLCommandBufferStatus::Completed {
                log::error!("Metal pass dump {pass}: failed command buffer"); return;
            }
            let result = (|| -> std::io::Result<()> {
                for (name, _, buffer, _, _, _, _, _, length, _) in &copies {
                    // Completion makes the shared allocation CPU-readable. This
                    // callback retains it; it is never reused as guest staging.
                    let bytes = unsafe { std::slice::from_raw_parts(buffer.contents().as_ptr().cast::<u8>(), *length) };
                    std::fs::write(directory.join(format!("pass-{pass:03}-{name}.raw")), bytes)?;
                }
                std::fs::write(directory.join(format!("pass-{pass:03}.txt")), &metadata)
            })();
            if let Err(error) = result { log::error!("Metal pass dump {pass}: {error}"); }
        });
        unsafe { command_buffer.addCompletedHandler(RcBlock::as_ptr(&handler)); }
    }
}

fn texture_root(texture: &ProtocolObject<dyn MTLTexture>) -> (usize, usize, usize) {
    if let Some(parent) = texture.parentTexture() {
        let (root, level, slice) = texture_root(&parent);
        (root, level + texture.parentRelativeLevel(), slice + texture.parentRelativeSlice())
    } else {
        (texture as *const _ as usize, 0, 0)
    }
}

fn layout(width: usize, height: usize, bpp: usize, budget: usize) -> Option<(usize, usize)> {
    let pitch = width.checked_mul(bpp)?.checked_add(255)? & !255;
    let length = pitch.checked_mul(height)?;
    (width != 0 && height != 0 && length <= budget).then_some((pitch, length))
}

fn transfer_format(format: MTLPixelFormat) -> Option<(usize, MTLBlitOption)> {
    let bytes = match format {
        MTLPixelFormat::RGBA8Unorm | MTLPixelFormat::RGBA8Unorm_sRGB
        | MTLPixelFormat::BGRA8Unorm | MTLPixelFormat::BGRA8Unorm_sRGB
        | MTLPixelFormat::RGB10A2Unorm | MTLPixelFormat::R32Float | MTLPixelFormat::Depth32Float => 4,
        MTLPixelFormat::RGBA16Float | MTLPixelFormat::RG32Float => 8,
        MTLPixelFormat::R16Float | MTLPixelFormat::RG8Unorm | MTLPixelFormat::Depth16Unorm => 2,
        MTLPixelFormat::R8Unorm => 1,
        MTLPixelFormat::Depth32Float_Stencil8 => return Some((4, MTLBlitOption::DepthFromDepthStencil)),
        _ => return None,
    };
    Some((bytes, MTLBlitOption::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readback_layout_is_aligned_bounded_and_checked() {
        assert_eq!(layout(960, 540, 4, MAX_BYTES), Some((3840, 2_073_600)));
        assert_eq!(layout(1, 2, 4, 512), Some((256, 512)));
        assert_eq!(layout(1, 2, 4, 511), None);
        assert_eq!(layout(usize::MAX, 1, 4, MAX_BYTES), None);
        assert_eq!(layout(1, usize::MAX, 4, MAX_BYTES), None);
        assert_eq!(layout(0, 1, 4, MAX_BYTES), None);
    }
}
