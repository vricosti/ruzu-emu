// SPDX-FileCopyrightText: Copyright 2025 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/hle/service/am/frontend/applet_web_browser.{h,cpp}`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use common::fs::path_util::{concat_path_safe, get_ruzu_path, RuzuPath};

use crate::core::SystemRef;
use crate::file_sys::fs_filesystem::OpenMode;
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::registered_cache::ContentProvider;
use crate::file_sys::romfs::extract_romfs;
use crate::file_sys::system_archive::system_archive::synthesize_system_archive;
use crate::file_sys::vfs::vfs::{vfs_raw_copy, vfs_raw_copy_d, VfsFilesystem};
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
use crate::frontend::applets::web_browser::WebBrowserApplet;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::ns::platform_service_manager::{
    decrypt_shared_font_to_ttf, SHARED_FONT_FILE_NAMES,
};

use super::applet_web_browser_types::{
    DocumentKind, ShimKind, WebAppletVersion, WebArgHeader, WebArgInputTlvMap, WebArgInputTlvType,
    WebArgOutputTlvType, WebExitReason,
};
use super::applets::FrontendApplet;

const DECRYPTED_SHARED_FONTS: [&str; 7] = [
    "FontStandard.ttf",
    "FontChineseSimplified.ttf",
    "FontExtendedChineseSimplified.ttf",
    "FontChineseTraditional.ttf",
    "FontKorean.ttf",
    "FontNintendoExtended.ttf",
    "FontNintendoExtended2.ttf",
];

fn parse_string_value(data: &[u8]) -> String {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

fn parse_u32(data: &[u8]) -> u32 {
    u32::from_le_bytes(
        data[..4]
            .try_into()
            .expect("web applet u32 TLV is too small"),
    )
}

fn parse_u64(data: &[u8]) -> u64 {
    u64::from_le_bytes(
        data[..8]
            .try_into()
            .expect("web applet u64 TLV is too small"),
    )
}

fn get_main_url(url: &str) -> &str {
    url.split_once('?').map_or(url, |(main, _)| main)
}

fn resolve_url(url: &str) -> String {
    match url.find('%') {
        Some(index) => format!("{}lp1{}", &url[..index], &url[index + 1..]),
        None => url.to_string(),
    }
}

fn read_web_args(web_arg: &[u8], header: &mut WebArgHeader) -> WebArgInputTlvMap {
    header.total_tlv_entries = u16::from_le_bytes([web_arg[0], web_arg[1]]);
    header._padding = [web_arg[2], web_arg[3]];
    header.shim_kind = ShimKind(u32::from_le_bytes(web_arg[4..8].try_into().unwrap()));

    if web_arg.len() == std::mem::size_of::<WebArgHeader>() {
        return HashMap::new();
    }

    let mut input_tlv_map = HashMap::new();
    let mut current_offset = std::mem::size_of::<WebArgHeader>();
    for _ in 0..header.total_tlv_entries {
        let Some(tlv_data) = web_arg.get(current_offset..current_offset + 8) else {
            return input_tlv_map;
        };
        let input_tlv_type = WebArgInputTlvType(u16::from_le_bytes([tlv_data[0], tlv_data[1]]));
        let arg_data_size = u16::from_le_bytes([tlv_data[2], tlv_data[3]]) as usize;
        current_offset += 8;
        let Some(data) = web_arg.get(current_offset..current_offset + arg_data_size) else {
            return input_tlv_map;
        };
        current_offset += arg_data_size;
        input_tlv_map.insert(input_tlv_type, data.to_vec());
    }
    input_tlv_map
}

fn get_offline_romfs(
    system: SystemRef,
    title_id: u64,
    nca_type: ContentRecordType,
) -> Option<VirtualFile> {
    if nca_type == ContentRecordType::Data {
        let controller = system.get().get_filesystem_controller();
        let controller = controller.lock().unwrap();
        let nca = controller
            .get_system_nand_contents()
            .and_then(|contents| contents.get_entry(title_id, nca_type));
        return match nca {
            Some(nca) => nca.get_romfs(),
            None => {
                log::error!(
                    "NCA of type={nca_type:?} with title_id={title_id:016X} is not found in the System NAND!"
                );
                synthesize_system_archive(title_id)
            }
        };
    }

    let Some(provider) = system.get().get_content_provider() else {
        log::error!("ContentProvider is unavailable for offline web applet");
        return None;
    };
    let provider = provider.lock().unwrap();
    let Some(nca) = provider.get_entry(title_id, nca_type) else {
        if nca_type == ContentRecordType::HtmlDocument {
            log::warn!("Falling back to AppLoader to get the RomFS.");
            let mut romfs = None;
            let _ = system.get().get_app_loader().read_manual_rom_fs(&mut romfs);
            if romfs.is_some() {
                return romfs;
            }
        }
        log::error!(
            "NCA of type={nca_type:?} with title_id={title_id:016X} is not found in the ContentProvider!"
        );
        return None;
    };

    let base_romfs = nca.get_romfs()?;
    let controller = system.get().get_filesystem_controller();
    let controller = controller.lock().unwrap();
    Some(
        PatchManager::new(title_id, &controller, &*provider).patch_romfs(
            Some(&nca),
            base_romfs,
            nca_type,
            None,
            true,
        ),
    )
}

fn extract_shared_fonts(system: SystemRef) {
    let fonts_dir = get_ruzu_path(RuzuPath::CacheDir).join("fonts");
    for (index, &(font, font_name)) in SHARED_FONT_FILE_NAMES.iter().enumerate() {
        let output_name = DECRYPTED_SHARED_FONTS[index];
        if fonts_dir.join(output_name).exists() {
            continue;
        }

        let title_id = font as u64;
        let controller = system.get().get_filesystem_controller();
        let controller = controller.lock().unwrap();
        let romfs = controller
            .get_system_nand_contents()
            .and_then(|contents| contents.get_entry(title_id, ContentRecordType::Data))
            .and_then(|nca| nca.get_romfs())
            .or_else(|| synthesize_system_archive(title_id));
        drop(controller);

        let Some(romfs) = romfs else {
            log::error!("SharedFont RomFS with title_id={title_id:016X} cannot be extracted!");
            continue;
        };
        let Some(extracted_romfs) = extract_romfs(Some(romfs)) else {
            log::error!("SharedFont RomFS with title_id={title_id:016X} failed to extract!");
            continue;
        };
        let Some(font_file) = extracted_romfs.get_file(font_name) else {
            log::error!(
                "SharedFont RomFS with title_id={title_id:016X} has no font file \"{font_name}\"!"
            );
            continue;
        };
        let bytes = font_file.read_all_bytes();
        if bytes.len() < 8 || bytes.len() % 4 != 0 {
            log::error!("Shared font {font_name} has invalid size {}", bytes.len());
            continue;
        }
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()).swap_bytes())
            .collect();
        let mut decrypted_data = vec![0; bytes.len() - 8];
        if !decrypt_shared_font_to_ttf(&words, &mut decrypted_data) {
            log::error!("Shared font {font_name} failed to decrypt");
            continue;
        }

        let Some(filesystem) = system.get().get_filesystem() else {
            log::error!("Virtual filesystem is unavailable while extracting shared fonts");
            return;
        };
        let Some(output_dir) =
            filesystem.create_directory(&fonts_dir.to_string_lossy(), OpenMode::READ_WRITE)
        else {
            log::error!(
                "Failed to create shared-font cache at {}",
                fonts_dir.display()
            );
            return;
        };
        let Some(output_file) = output_dir.create_file(output_name) else {
            log::error!("Failed to create cached shared font {output_name}");
            continue;
        };
        let decrypted_font = VectorVfsFile::new(decrypted_data, output_name.to_string(), None);
        let _ = vfs_raw_copy(&decrypted_font, output_file.as_ref(), 0x1000);
    }
}

fn exit(applet: &Weak<Mutex<Applet>>) {
    let Some(applet) = applet.upgrade() else {
        return;
    };
    let mut applet = applet.lock().unwrap();
    applet.is_completed = true;
    applet.signal_state_changed_event_without_process();
}

fn write_output_tlv(out: &mut [u8], offset: usize, kind: WebArgOutputTlvType, size: u16) {
    out[offset..offset + 2].copy_from_slice(&kind.0.to_le_bytes());
    out[offset + 2..offset + 4].copy_from_slice(&size.to_le_bytes());
}

fn build_exit_output(
    header: WebArgHeader,
    version: WebAppletVersion,
    exit_reason: WebExitReason,
    last_url: &str,
) -> Vec<u8> {
    let use_tlv_output = (header.shim_kind == ShimKind::SHARE
        && version >= WebAppletVersion::VERSION_196608)
        || (header.shim_kind == ShimKind::WEB && version >= WebAppletVersion::VERSION_524288)
        || header.shim_kind == ShimKind::LHUB;

    if !use_tlv_output {
        let mut output = vec![0; 0x1010];
        output[0..4].copy_from_slice(&exit_reason.0.to_le_bytes());
        let copy_size = last_url.len().min(0x1000);
        output[8..8 + copy_size].copy_from_slice(&last_url.as_bytes()[..copy_size]);
        output[0x1008..0x1010].copy_from_slice(&(last_url.len() as u64).to_le_bytes());
        return output;
    }

    let mut output = vec![0; 0x2000];
    let mut offset = 8;
    write_output_tlv(
        &mut output,
        offset,
        WebArgOutputTlvType::SHARE_EXIT_REASON,
        4,
    );
    offset += 8;
    output[offset..offset + 4].copy_from_slice(&exit_reason.0.to_le_bytes());
    offset = (offset + 4 + 7) & !7;

    let url_data_size: u16 = (last_url.len() + 1)
        .try_into()
        .expect("web applet last URL exceeds TLV size");
    write_output_tlv(
        &mut output,
        offset,
        WebArgOutputTlvType::LAST_URL,
        url_data_size,
    );
    offset += 8;
    output[offset..offset + last_url.len()].copy_from_slice(last_url.as_bytes());
    offset += url_data_size as usize;
    offset = (offset + 7) & !7;

    write_output_tlv(&mut output, offset, WebArgOutputTlvType::LAST_URL_SIZE, 8);
    offset += 8;
    output[offset..offset + 8].copy_from_slice(&(last_url.len() as u64).to_le_bytes());

    output[0..2].copy_from_slice(&3u16.to_le_bytes());
    output[4..8].copy_from_slice(&header.shim_kind.0.to_le_bytes());
    output
}

pub struct WebBrowser {
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn WebBrowserApplet>,
    initialized: bool,
    complete: Arc<AtomicBool>,
    frontend_executing: Arc<AtomicBool>,
    status: ResultCode,
    web_applet_version: WebAppletVersion,
    web_arg_header: WebArgHeader,
    web_arg_input_tlv_map: WebArgInputTlvMap,
    title_id: u64,
    nca_type: ContentRecordType,
    offline_cache_dir: PathBuf,
    offline_document: PathBuf,
    offline_romfs: Option<VirtualFile>,
    external_url: String,
}

impl WebBrowser {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn WebBrowserApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            complete: Arc::new(AtomicBool::new(false)),
            frontend_executing: Arc::new(AtomicBool::new(false)),
            status: RESULT_SUCCESS,
            web_applet_version: WebAppletVersion::default(),
            web_arg_header: WebArgHeader::default(),
            web_arg_input_tlv_map: HashMap::new(),
            title_id: 0,
            nca_type: ContentRecordType::Meta,
            offline_cache_dir: PathBuf::new(),
            offline_document: PathBuf::new(),
            offline_romfs: None,
            external_url: String::new(),
        }
    }

    fn input_tlv_exists_in_map(&self, input_tlv_type: WebArgInputTlvType) -> bool {
        self.web_arg_input_tlv_map.contains_key(&input_tlv_type)
    }

    fn get_input_tlv_data(&self, input_tlv_type: WebArgInputTlvType) -> Option<Vec<u8>> {
        self.input_tlv_exists_in_map(input_tlv_type)
            .then(|| self.web_arg_input_tlv_map[&input_tlv_type].clone())
    }

    fn initialize_offline(&mut self) {
        let document_path = parse_string_value(
            &self
                .get_input_tlv_data(WebArgInputTlvType::DOCUMENT_PATH)
                .expect("Offline web applet is missing DocumentPath"),
        );
        let document_kind = DocumentKind(parse_u32(
            &self
                .get_input_tlv_data(WebArgInputTlvType::DOCUMENT_KIND)
                .expect("Offline web applet is missing DocumentKind"),
        ));

        let additional_paths = match document_kind {
            DocumentKind::APPLICATION_LEGAL_INFORMATION => {
                self.title_id = parse_u64(
                    &self
                        .get_input_tlv_data(WebArgInputTlvType::APPLICATION_ID)
                        .expect("Legal-information applet is missing ApplicationID"),
                );
                self.nca_type = ContentRecordType::LegalInformation;
                ""
            }
            DocumentKind::SYSTEM_DATA_PAGE => {
                self.title_id = parse_u64(
                    &self
                        .get_input_tlv_data(WebArgInputTlvType::SYSTEM_DATA_ID)
                        .expect("System-data applet is missing SystemDataID"),
                );
                self.nca_type = ContentRecordType::Data;
                ""
            }
            _ => {
                self.title_id = self.system.get().runtime_program_id();
                self.nca_type = ContentRecordType::HtmlDocument;
                "html-document"
            }
        };

        let resource_type = ["manual", "legal_information", "system_data"]
            .get(document_kind.0.wrapping_sub(1) as usize)
            .expect("Invalid offline web DocumentKind");
        self.offline_cache_dir = get_ruzu_path(RuzuPath::CacheDir).join(format!(
            "offline_web_applet_{resource_type}/{:016X}",
            self.title_id
        ));
        self.offline_document = concat_path_safe(
            &self.offline_cache_dir,
            &Path::new(additional_paths).join(document_path),
        );
    }

    fn initialize_web(&mut self) {
        self.external_url = resolve_url(&parse_string_value(
            &self
                .get_input_tlv_data(WebArgInputTlvType::INITIAL_URL)
                .expect("Web applet is missing InitialURL"),
        ));
    }

    fn web_browser_exit(&self, exit_reason: WebExitReason, last_url: String) {
        Self::finish(
            self.web_arg_header,
            self.web_applet_version,
            exit_reason,
            last_url,
            &self.applet,
            &self.broker,
            &self.complete,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        header: WebArgHeader,
        version: WebAppletVersion,
        exit_reason: WebExitReason,
        last_url: String,
        applet: &Weak<Mutex<Applet>>,
        broker: &AppletDataBroker,
        complete: &AtomicBool,
        frontend_executing: Option<&AtomicBool>,
    ) {
        log::debug!(
            "WebBrowser exit: exit_reason={exit_reason:?}, last_url={last_url}, last_url_size={}",
            last_url.len()
        );
        broker
            .get_out_data()
            .push(build_exit_output(header, version, exit_reason, &last_url));
        complete.store(true, Ordering::Release);
        // Direct Execute exits and inline frontend callbacks run under the
        // accessor's Applet mutex. It publishes completion on return. Only
        // a callback arriving after the frontend call must acquire it here.
        if frontend_executing.is_some_and(|executing| !executing.load(Ordering::Acquire)) {
            exit(applet);
        }
    }

    fn extract_offline_romfs(system: SystemRef, romfs: Option<VirtualFile>, cache_dir: &Path) {
        log::debug!("Extracting RomFS to {}", cache_dir.display());
        let Some(extracted_romfs_dir) = extract_romfs(romfs) else {
            log::error!("Failed to extract offline web RomFS");
            return;
        };
        let Some(filesystem) = system.get().get_filesystem() else {
            log::error!("Virtual filesystem is unavailable for offline web extraction");
            return;
        };
        let Some(output_dir) =
            filesystem.create_directory(&cache_dir.to_string_lossy(), OpenMode::READ_WRITE)
        else {
            log::error!(
                "Failed to create offline web cache at {}",
                cache_dir.display()
            );
            return;
        };
        let _ = vfs_raw_copy_d(extracted_romfs_dir.as_ref(), output_dir.as_ref(), 0x1000);
    }

    fn execute_stub(&self, name: &str) {
        log::warn!("(STUBBED) called, {name} Applet is not implemented");
        self.web_browser_exit(WebExitReason::END_BUTTON_PRESSED, String::new());
    }

    fn execute_offline(&mut self) {
        if self.applet_mode == LibraryAppletMode::AllForegroundInitiallyHidden {
            log::warn!("WebSession is not implemented");
            return;
        }

        let main_url = get_main_url(&self.offline_document.to_string_lossy()).to_string();
        if !Path::new(&main_url).exists() {
            self.offline_romfs = get_offline_romfs(self.system, self.title_id, self.nca_type);
            if self.offline_romfs.is_none() {
                log::error!(
                    "RomFS with title_id={:016X} and nca_type={:?} cannot be extracted!",
                    self.title_id,
                    self.nca_type
                );
                self.web_browser_exit(WebExitReason::WINDOW_CLOSED, String::new());
                return;
            }
        }

        let local_url = self.offline_document.to_string_lossy().into_owned();
        log::info!("Opening offline document at {local_url}");
        let system = self.system;
        let romfs = self.offline_romfs.clone();
        let cache_dir = self.offline_cache_dir.clone();
        let header = self.web_arg_header;
        let version = self.web_applet_version;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let complete = Arc::clone(&self.complete);
        let executing = Arc::clone(&self.frontend_executing);

        executing.store(true, Ordering::Release);
        self.frontend.open_local_web_page(
            &local_url,
            Box::new(move || Self::extract_offline_romfs(system, romfs.clone(), &cache_dir)),
            Box::new(move |reason, last_url| {
                Self::finish(
                    header, version, reason, last_url, &applet, &broker, &complete, Some(&executing),
                )
            }),
        );
        self.frontend_executing.store(false, Ordering::Release);
    }

    fn execute_web(&self) {
        log::info!("Opening external URL at {}", self.external_url);
        let header = self.web_arg_header;
        let version = self.web_applet_version;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let complete = Arc::clone(&self.complete);
        let executing = Arc::clone(&self.frontend_executing);

        executing.store(true, Ordering::Release);
        self.frontend.open_external_web_page(
            &self.external_url,
            Box::new(move |mut reason, last_url| {
                if reason == WebExitReason::EXIT_REQUESTED
                    || reason == WebExitReason::END_BUTTON_PRESSED
                {
                    reason = if header.shim_kind == ShimKind::WEB
                        || header.shim_kind == ShimKind::OFFLINE
                    {
                        WebExitReason::EXIT_REQUESTED
                    } else {
                        WebExitReason::END_BUTTON_PRESSED
                    };
                }
                Self::finish(
                    header, version, reason, last_url, &applet, &broker, &complete, Some(&executing),
                )
            }),
        );
        self.frontend_executing.store(false, Ordering::Release);
    }
}

impl FrontendApplet for WebBrowser {
    fn initialize(&mut self) {
        self.complete.store(false, Ordering::Release);
        self.initialized = true;
        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("WebBrowser::Initialize missing common arguments");
        let common =
            copy_common_arguments(&common_data).expect("WebBrowser common arguments are too small");
        self.web_applet_version = WebAppletVersion(common.library_version);

        let web_arg = self
            .broker
            .get_in_data()
            .pop()
            .expect("WebBrowser::Initialize missing web arguments");
        if web_arg.len() < std::mem::size_of::<WebArgHeader>() {
            return;
        }
        self.web_arg_input_tlv_map = read_web_args(&web_arg, &mut self.web_arg_header);

        if *common::settings::values().disable_web_applet.get_value()
            && self.web_arg_header.shim_kind != ShimKind::WEB
            && self.web_arg_header.shim_kind != ShimKind::LHUB
        {
            return;
        }

        extract_shared_fonts(self.system);
        match self.web_arg_header.shim_kind {
            ShimKind::OFFLINE => self.initialize_offline(),
            ShimKind::WEB => self.initialize_web(),
            ShimKind::SHOP
            | ShimKind::LOGIN
            | ShimKind::SHARE
            | ShimKind::WIFI
            | ShimKind::LOBBY
            | ShimKind::LHUB => {}
            shim => panic!("Invalid ShimKind={:?}", shim),
        }
    }

    fn get_status(&self) -> ResultCode {
        self.status
    }

    fn execute_interactive(&mut self) {
        log::error!("WebSession is not implemented");
    }

    fn execute(&mut self) {
        if self.web_arg_header.shim_kind == ShimKind::WEB {
            self.execute_web();
            return;
        }
        if self.web_arg_header.shim_kind == ShimKind::LHUB {
            self.execute_stub("Lhub");
            return;
        }
        if *common::settings::values().disable_web_applet.get_value() {
            log::warn!(
                "(STUBBED) called, Web Browser Applet is disabled. shim_kind={:?}",
                self.web_arg_header.shim_kind
            );
            self.web_browser_exit(WebExitReason::END_BUTTON_PRESSED, String::new());
            return;
        }

        match self.web_arg_header.shim_kind {
            ShimKind::SHOP => self.execute_stub("Shop"),
            ShimKind::LOGIN => self.execute_stub("Login"),
            ShimKind::OFFLINE => self.execute_offline(),
            ShimKind::SHARE => self.execute_stub("Share"),
            ShimKind::WEB => self.execute_web(),
            ShimKind::WIFI => self.execute_stub("Wifi"),
            ShimKind::LOBBY => self.execute_stub("Lobby"),
            ShimKind::LHUB => self.execute_stub("Lhub"),
            shim => {
                log::error!("Invalid ShimKind={shim:?}");
                self.web_browser_exit(WebExitReason::END_BUTTON_PRESSED, String::new());
            }
        }
    }

    fn request_exit(&mut self) {
        self.frontend.close();
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }
}

fn copy_common_arguments(data: &[u8]) -> Option<CommonArguments> {
    if data.len() < std::mem::size_of::<CommonArguments>() {
        return None;
    }
    let mut common = std::mem::MaybeUninit::<CommonArguments>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            common.as_mut_ptr().cast::<u8>(),
            std::mem::size_of::<CommonArguments>(),
        );
        Some(common.assume_init())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronous_exits_return_while_accessor_owns_applet_lock() {
        use crate::core::System;
        use crate::frontend::applets::web_browser::DefaultWebBrowserApplet;
        use crate::hle::service::os::process::Process;

        let system = Box::new(System::new());
        let system_ref = SystemRef::from_ref(&system);
        for shim in [ShimKind::SHOP, ShimKind::WEB] {
            let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
            let broker = Arc::new(AppletDataBroker::new());
            let mut browser = WebBrowser::new(
                system_ref, Arc::downgrade(&owner), broker.clone(),
                LibraryAppletMode::AllForeground, Arc::new(DefaultWebBrowserApplet),
            );
            browser.web_arg_header.shim_kind = shim;
            let guard = owner.lock().unwrap();
            // SHOP exercises a direct exit; WEB exercises an inline callback.
            browser.execute();
            assert!(browser.is_complete());
            assert!(!guard.is_completed); // accessor publishes this after Execute
            let output = broker.get_out_data().pop().unwrap();
            assert_eq!(output.len(), 0x1010);
            let expected = if shim == ShimKind::SHOP {
                WebExitReason::END_BUTTON_PRESSED
            } else {
                WebExitReason::WINDOW_CLOSED
            };
            assert_eq!(&output[..4], &expected.0.to_le_bytes());
        }
    }

    #[test]
    fn asynchronous_exit_publishes_completion_without_accessor() {
        use crate::core::System;
        use crate::hle::service::os::process::Process;

        let system = Box::new(System::new());
        let owner = Arc::new(Mutex::new(Applet::new(
            SystemRef::from_ref(&system), Process::new(), false,
        )));
        let broker = AppletDataBroker::new();
        let complete = AtomicBool::new(false);
        let executing = AtomicBool::new(false);
        WebBrowser::finish(
            WebArgHeader::default(), WebAppletVersion::default(),
            WebExitReason::WINDOW_CLOSED, String::new(), &Arc::downgrade(&owner),
            &broker, &complete, Some(&executing),
        );
        assert!(complete.load(Ordering::Acquire));
        assert!(owner.lock().unwrap().is_completed);
        assert!(broker.get_out_data().pop().is_ok());
    }

    fn tlv(kind: WebArgInputTlvType, data: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        result.extend_from_slice(&kind.0.to_le_bytes());
        result.extend_from_slice(&(data.len() as u16).to_le_bytes());
        result.extend_from_slice(&0u32.to_le_bytes());
        result.extend_from_slice(data);
        result
    }

    #[test]
    fn reads_declared_tlvs_and_stops_at_truncated_entry() {
        let mut input = Vec::new();
        input.extend_from_slice(&3u16.to_le_bytes());
        input.extend_from_slice(&[0; 2]);
        input.extend_from_slice(&ShimKind::WEB.0.to_le_bytes());
        input.extend_from_slice(&tlv(
            WebArgInputTlvType::INITIAL_URL,
            b"https://example.invalid\0",
        ));
        input.extend_from_slice(&tlv(
            WebArgInputTlvType::APPLICATION_ID,
            &42u64.to_le_bytes(),
        ));
        input.extend_from_slice(&WebArgInputTlvType::DOCUMENT_PATH.0.to_le_bytes());

        let mut header = WebArgHeader::default();
        let map = read_web_args(&input, &mut header);
        assert_eq!(header.total_tlv_entries, 3);
        assert_eq!(header.shim_kind, ShimKind::WEB);
        assert_eq!(map.len(), 2);
        assert_eq!(parse_u64(&map[&WebArgInputTlvType::APPLICATION_ID]), 42);
    }

    #[test]
    fn output_switches_to_tlv_at_upstream_versions() {
        let header = WebArgHeader {
            total_tlv_entries: 0,
            _padding: [0; 2],
            shim_kind: ShimKind::WEB,
        };
        let legacy = build_exit_output(
            header,
            WebAppletVersion::VERSION_393216,
            WebExitReason::WINDOW_CLOSED,
            "http://localhost/",
        );
        assert_eq!(legacy.len(), 0x1010);
        assert_eq!(&legacy[..4], &WebExitReason::WINDOW_CLOSED.0.to_le_bytes());

        let tlv_output = build_exit_output(
            header,
            WebAppletVersion::VERSION_524288,
            WebExitReason::CALLBACK_URL,
            "http://localhost/",
        );
        assert_eq!(tlv_output.len(), 0x2000);
        assert_eq!(u16::from_le_bytes(tlv_output[..2].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(tlv_output[4..8].try_into().unwrap()),
            ShimKind::WEB.0
        );
    }

    #[test]
    fn helpers_match_upstream_url_rules() {
        assert_eq!(get_main_url("index.html?a=1"), "index.html");
        assert_eq!(
            resolve_url("https://example.invalid/%/page"),
            "https://example.invalid/lp1/page"
        );
    }
}
