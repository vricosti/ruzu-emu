# Same gperf 3.3 recipe and source hash as vcpkg's ports/gperf/portfile.cmake.
# Try GNU mirrors first when the central GNU servers time out on Windows.
set(VCPKG_POLICY_EMPTY_INCLUDE_FOLDER enabled)
set(VCPKG_BUILD_TYPE release) # tool only

vcpkg_download_distfile(ARCHIVE
    URLS
        "https://mirrors.kernel.org/gnu/gperf/gperf-${VERSION}.tar.gz"
        "https://mirrors.ocf.berkeley.edu/gnu/gperf/gperf-${VERSION}.tar.gz"
        "https://ftpmirror.gnu.org/gnu/gperf/gperf-${VERSION}.tar.gz"
        "https://ftp.gnu.org/pub/gnu/gperf/gperf-${VERSION}.tar.gz"
    FILENAME gperf-${VERSION}.tar.gz
    SHA512 246b75b8ce7d77d6a8725cd15f1cf2e68da404812573af1d5bf32dbe6ad4228f48757baefc77bcb1f5597c2397043c04d31d8a04ab507bfa7a80f85e1ab6045f
)

vcpkg_extract_source_archive(
    SOURCE_PATH
    ARCHIVE ${ARCHIVE}
)

vcpkg_make_configure(
    SOURCE_PATH "${SOURCE_PATH}"
    AUTORECONF
    OPTIONS_RELEASE
        "--bindir=\\\${prefix}/tools/${PORT}"
)

vcpkg_make_install()

vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/COPYING")
