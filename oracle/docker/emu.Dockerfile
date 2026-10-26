# psxw-oracle-emu: the device target. Beetle PSX (Mednafen's PlayStation
# core, software renderer only, interpreter only), OpenBIOS built from
# PCSX-Redux, and psxw-emu-host (oracle/emulator-host).
#
# Build context: oracle/. Every input is pinned in inputs.lock; `psxw oracle
# images build` passes the pins as build args. Nothing comes from the host
# machine except our own sources in the context.

ARG BASE_IMAGE
FROM ${BASE_IMAGE} AS build
SHELL ["/bin/sh", "-eu", "-c"]
ARG DEBIAN_SNAPSHOT
COPY docker/apt-snapshot.sh /usr/local/bin/apt-snapshot.sh
RUN chmod +x /usr/local/bin/apt-snapshot.sh \
 && apt-snapshot.sh "$DEBIAN_SNAPSHOT" ca-certificates git make gcc g++ libc6-dev \
      gcc-mipsel-linux-gnu g++-mipsel-linux-gnu binutils-mipsel-linux-gnu

# Beetle PSX at the pinned commit. HAVE_HW=0: no OpenGL/Vulkan renderer;
# HAVE_LIGHTREC=0: no dynarec. CHD support stays on: it is a disc-image
# reader, and at this commit the HAVE_CHD=0 build does not link.
ARG BEETLE_REPO=https://github.com/libretro/beetle-psx-libretro
ARG BEETLE_COMMIT
RUN git init -q /src/beetle \
 && git -C /src/beetle fetch -q --depth 1 "$BEETLE_REPO" "$BEETLE_COMMIT" \
 && git -C /src/beetle checkout -q FETCH_HEAD \
 && test "$(git -C /src/beetle rev-parse HEAD)" = "$BEETLE_COMMIT"
RUN make -C /src/beetle -j"$(nproc)" HAVE_HW=0 HAVE_OPENGL=0 HAVE_VULKAN=0 HAVE_LIGHTREC=0 \
 && test -f /src/beetle/mednafen_psx_libretro.so

# OpenBIOS from PCSX-Redux at the pinned commit. src/mips is the nugget
# submodule; its commit must be the one the PCSX-Redux commit records.
ARG REDUX_REPO=https://github.com/grumpycoders/pcsx-redux
ARG REDUX_COMMIT
ARG NUGGET_REPO=https://github.com/pcsx-redux/nugget
ARG NUGGET_COMMIT
RUN git init -q /src/redux \
 && git -C /src/redux fetch -q --depth 1 "$REDUX_REPO" "$REDUX_COMMIT" \
 && git -C /src/redux checkout -q FETCH_HEAD \
 && test "$(git -C /src/redux rev-parse HEAD)" = "$REDUX_COMMIT" \
 && test "$(git -C /src/redux ls-tree HEAD src/mips | awk '{print $3}')" = "$NUGGET_COMMIT" \
 && git init -q /src/redux/src/mips \
 && git -C /src/redux/src/mips fetch -q --depth 1 "$NUGGET_REPO" "$NUGGET_COMMIT" \
 && git -C /src/redux/src/mips checkout -q FETCH_HEAD \
 && test "$(git -C /src/redux/src/mips rev-parse HEAD)" = "$NUGGET_COMMIT"
# Debian's binutils call the little-endian MIPS ELF target
# elf32-tradlittlemips (the nugget default, elf32-littlemips, is the
# mipsel-none-elf name).
RUN make -C /src/redux/src/mips/openbios -j"$(nproc)" PREFIX=mipsel-linux-gnu FORMAT=elf32-tradlittlemips \
 && test "$(stat -c %s /src/redux/src/mips/openbios/openbios.bin)" = 524288

# The headless libretro frontend.
COPY include /src/psxw/include
COPY emulator-host /src/psxw/emulator-host
RUN make -C /src/psxw/emulator-host clean \
 && make -C /src/psxw/emulator-host LIBRETRO_INCLUDE=/src/beetle/libretro-common/include WIRE_INCLUDE=/src/psxw/include

RUN mkdir -p /out/bin /out/lib /out/system /out/share \
 && cp /src/psxw/emulator-host/psxw-emu-host /out/bin/ \
 && cp /src/beetle/mednafen_psx_libretro.so /out/lib/ \
 && cp /src/redux/src/mips/openbios/openbios.bin /out/system/openbios.bin \
 && cp /src/redux/src/mips/openbios/openbios.elf /src/redux/src/mips/openbios/openbios.map /out/share/ \
 && printf '{"beetle": "%s", "pcsx_redux": "%s", "nugget": "%s", "debian_snapshot": "%s", "openbios_sha1": "%s", "mipsel_gcc": "%s", "host_gcc": "%s"}\n' \
      "$(git -C /src/beetle rev-parse HEAD)" "$(git -C /src/redux rev-parse HEAD)" \
      "$(git -C /src/redux/src/mips rev-parse HEAD)" "$DEBIAN_SNAPSHOT" \
      "$(sha1sum /out/system/openbios.bin | cut -d' ' -f1)" \
      "$(mipsel-linux-gnu-gcc -dumpfullversion)" "$(gcc -dumpfullversion)" > /out/share/sources.json \
 && dpkg-query -W -f '${Package} ${Version}\n' | sort > /out/share/build-packages.txt

FROM ${BASE_IMAGE}
COPY --from=build /out/ /opt/psxw/
LABEL org.psxw.image="psxw-oracle-emu"
WORKDIR /work
ENTRYPOINT ["/opt/psxw/bin/psxw-emu-host"]
