#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$script_dir/.." && pwd)
eden_dir=${EDEN_SOURCE_DIR:-"$repo_dir/../eden"}
eden_build_dir=${EDEN_BUILD_DIR:-"$eden_dir/build"}
output_dir="$repo_dir/target/eden-oracle"
cxx=${CXX:-c++}

mkdir -p "$output_dir"

"$cxx" -std=c++20 -O2 -DNDEBUG \
    -DARCHITECTURE_x86_64=1 \
    -DDYNARMIC_ENABLE_CPU_FEATURE_DETECTION=1 \
    -DMCL_IGNORE_ASSERTS=1 \
    -DXBYAK_OLD_DISP_CHECK=1 \
    -DXBYAK_STRICT_CHECK_MEM_REG_SIZE=0 \
    -I"$eden_dir/src" \
    -I"$eden_dir/src/dynarmic/src" \
    -I"$eden_dir/.cache/cpm/xbyak/v7.35.2" \
    -I"$eden_dir/.cache/cpm/unordered_dense/7b55cab841/include" \
    -I"$eden_dir/.cache/cpm/fmt/12.1.0/include" \
    "$repo_dir/tools/a32_oracle.cpp" \
    -Wl,--start-group \
    "$eden_build_dir/src/dynarmic/src/dynarmic/libdynarmic.a" \
    "$eden_build_dir/src/common/libcommon.a" \
    "$eden_build_dir/_deps/fmt-build/libfmt.a" \
    -Wl,--end-group \
    -lboost_context -lboost_filesystem -lboost_atomic -lpthread -ldl \
    -o "$output_dir/a32_oracle"

printf 'Built %s\n' "$output_dir/a32_oracle"
