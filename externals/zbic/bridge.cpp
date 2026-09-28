/* ZBIC integration boundary, corresponding to Eden common/zbic_compression.cpp.
 * The third-party implementation and its notices are retained unmodified.
 */
#define ZSTD_ZBIC_SUPPORT 1
#define ZSTDLIB_VISIBLE static
#define ZSTDLIB_HIDDEN static
#define ZSTDERRORLIB_VISIBLE static
#define ZSTDERRORLIB_HIDDEN static
#undef ZSTD_MULTITHREAD
#if defined(__ANDROID__)
#undef _GNU_SOURCE
#endif
#include "vendor/src/zstd.h"
#define g_ZSTD_threading_useless_symbol g_ZSTD_zbic_threading_useless_symbol
#include "vendor/src/zstd.c"
#undef g_ZSTD_threading_useless_symbol

extern "C" int ruzu_zbic_decompress(void* dst, size_t dst_size, const void* src, size_t src_size) {
    const size_t result = ZSTD_decompress(dst, dst_size, src, src_size);
    return ZSTD_isError(result) ? -1 : (int)result;
}

/* Used by Rust regression tests to exercise nontrivial BIC entropy tables. */
extern "C" size_t ruzu_zbic_compress_bound(size_t size) { return ZSTD_compressBound(size); }
extern "C" size_t ruzu_zbic_compress(void* dst, size_t dst_size, const void* src, size_t src_size) {
    const size_t result = ZSTD_compress(dst, dst_size, src, src_size, 3);
    return ZSTD_isError(result) ? 0 : result;
}
