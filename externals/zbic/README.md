# Pinned ZBIC codec

The unmodified `vendor/src/zstd.c`, `zstd.h`, `zstd_errors.h` and `vendor/LICENSE`
come from https://github.com/kinnay/zbic at
`11b08f2712264bbed731545085cbd9702096ceb7`, the same revision pinned by Eden
`815325cceca948030cf476dce7c46d901e4e4df5`.

These files implement the modified Zstandard entropy-table coding used by ZBIC;
ordinary Zstandard cannot replace them. Original copyright/license notices are
retained. See `vendor/LICENSE` and the notices within each source file.

`bridge.cpp` gives the codec private ZSTD API symbols and exports Ruzu-prefixed
entry points. Like Eden it compiles the amalgamation as C++, keeping internal
symbols distinct from the normal C zstd library. Common's build
script compiles it with the existing Cargo C compiler discovery mechanism. No
network fetch is required during a build.
