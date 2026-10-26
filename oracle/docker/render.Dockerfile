# psxw-render-gl: the GPU backend's headless environment (plans/M04-pixels.md).
# Mesa's llvmpipe with EGL surfaceless, from the Debian snapshot the oracle
# images use, on the same base image; and the pinned Rust toolchain, which
# builds psxw-render-server from the workspace mounted read-only at run time.
#
# Build context: oracle/. Every input is pinned in inputs.lock ([oracle] and
# [oracle.render_gl]); the GPU oracle passes the pins as build args and
# records the image in .oracle/render-gl.json.

ARG BASE_IMAGE
FROM ${BASE_IMAGE}
SHELL ["/bin/sh", "-eu", "-c"]
ARG DEBIAN_SNAPSHOT
COPY docker/apt-snapshot.sh /usr/local/bin/apt-snapshot.sh
RUN chmod +x /usr/local/bin/apt-snapshot.sh \
 && apt-snapshot.sh "$DEBIAN_SNAPSHOT" ca-certificates curl xz-utils gcc libc6-dev \
      libegl1 libegl-mesa0 libgles2 libgl1-mesa-dri

# Rust, the standalone release archive, verified by sha256.
ARG RUST_VERSION
ARG RUST_URL
ARG RUST_SHA256
RUN curl -fsSL --retry 5 -o /tmp/rust.tar.xz "$RUST_URL" \
 && echo "$RUST_SHA256  /tmp/rust.tar.xz" | sha256sum -c - \
 && mkdir /tmp/rust \
 && tar -xJf /tmp/rust.tar.xz -C /tmp/rust --strip-components=1 \
 && /tmp/rust/install.sh --prefix=/opt/rust --disable-ldconfig \
      --components=rustc,cargo,rust-std-x86_64-unknown-linux-gnu \
 && rm -rf /tmp/rust /tmp/rust.tar.xz \
 && test "$(/opt/rust/bin/rustc --version | cut -d' ' -f2)" = "$RUST_VERSION"

ENV PATH=/opt/rust/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    EGL_PLATFORM=surfaceless \
    GALLIUM_DRIVER=llvmpipe \
    LIBGL_ALWAYS_SOFTWARE=1 \
    MESA_SHADER_CACHE_DISABLE=true \
    HOME=/tmp

RUN mkdir -p /opt/psxw \
 && printf '{"debian_snapshot": "%s", "rustc": "%s", "mesa": "%s", "libglvnd": "%s"}\n' \
      "$DEBIAN_SNAPSHOT" "$(rustc --version)" \
      "$(dpkg-query -W -f '${Version}' libegl-mesa0)" "$(dpkg-query -W -f '${Version}' libegl1)" \
      > /opt/psxw/sources.json \
 && dpkg-query -W -f '${Package} ${Version}\n' | sort > /opt/psxw/packages.txt

LABEL org.psxw.image="psxw-render-gl"
WORKDIR /work
