//! Read-only diagnostic: inspect installed firmware without printing encryption keys.
use core::file_sys::{
    content_archive::NCA,
    fs_filesystem::OpenMode,
    nca_metadata::ContentRecordType,
    registered_cache::{ContentProvider, RegisteredCache},
    vfs::vfs_real::RealVfsFilesystem,
};

fn main() {
    let directory = std::env::args()
        .nth(1)
        .expect("registered directory required");
    let vfs = RealVfsFilesystem::new();
    let dir = vfs
        .arc_open_directory(&directory, OpenMode::READ)
        .expect("open directory");
    for file in dir.get_files() {
        let name = file.get_name();
        let nca = NCA::new(file, None);
        println!(
            "{name}: {:?} title={:016X} type={:?} generation={:02X} romfs={}",
            nca.get_status(),
            nca.get_title_id(),
            nca.get_type(),
            nca.get_key_generation(),
            nca.get_romfs().is_some()
        );
    }
    let cache = RegisteredCache::new(dir);
    println!(
        "Indexed entries: {}",
        cache.list_entries_filter(None, None, None).len()
    );
    let mii = cache.get_entry(0x0100000000000802, ContentRecordType::Data);
    println!(
        "MiiModel indexed={} romfs={}",
        mii.is_some(),
        mii.is_some_and(|nca| nca.get_romfs().is_some())
    );
}
