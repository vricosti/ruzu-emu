# Fixed Rust compiler/base and Debian package snapshot. Updating these pins is
# deliberate; do not replace them with rolling tags/repositories.
FROM rust:1.92.0-slim-trixie@sha256:ce021d99731cbf433fd565ad0d6da709864ada6ef6db382075ca27d35ceed5c1

RUN rm /etc/apt/sources.list.d/debian.sources && \
    printf '%s\n' 'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20260114T000000Z/ trixie main' > /etc/apt/sources.list && \
    apt-get update && apt-get install -y --no-install-recommends \
      build-essential ca-certificates clang cmake curl git glslang-tools \
      libasound2-dev libavcodec-dev libavutil-dev libclang-dev libdbus-1-dev \
      libdecor-0-dev libdrm-dev libegl1-mesa-dev libgbm-dev libgl1-mesa-dev \
      libgtk-4-dev libjack-jackd2-dev libopus-dev libpipewire-0.3-dev libpulse-dev \
      libssl-dev libudev-dev libvulkan-dev libva-dev libwayland-dev libx11-dev \
      libxcursor-dev libxext-dev libxfixes-dev libxi-dev libxkbcommon-dev \
      libxrandr-dev libxss-dev libxtst-dev ninja-build pkg-config wayland-protocols \
      python3 file patchelf librsvg2-common gobject-introspection \
      mesa-vulkan-drivers libgl1-mesa-dri libegl-mesa0 \
      desktop-file-utils zstd xz-utils squashfs-tools xvfb xauth fonts-dejavu-core

# This script pins and verifies its own sharun/appimagetool/helper downloads.
ADD https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/45964bcbcb5456a6c902c55185635c06700d3284/useful-tools/quick-sharun.sh /opt/quick-sharun
RUN echo '8471838e86f4dce73cc49bc7549105b3e4fc130c173cbece897b093b77383e7d  /opt/quick-sharun' | sha256sum -c - && chmod 755 /opt/quick-sharun

WORKDIR /source
