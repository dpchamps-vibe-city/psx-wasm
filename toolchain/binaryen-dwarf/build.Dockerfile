# psxw-binaryen-build: the environment that builds the debug build's
# patched wasm-opt (psxw_inputs::toolchain::dwarf_binaryen). Only the
# compiler and build tools; the hash-checked source and our patches are
# mounted when it runs, with no network.
#
# Build context: oracle/docker (for apt-snapshot.sh). The base image and
# Debian snapshot are inputs.lock's [oracle] pins, so the compiler is pinned
# along with them.

ARG BASE_IMAGE
FROM ${BASE_IMAGE}
SHELL ["/bin/sh", "-eu", "-c"]
ARG DEBIAN_SNAPSHOT
COPY apt-snapshot.sh /usr/local/bin/apt-snapshot.sh
RUN chmod +x /usr/local/bin/apt-snapshot.sh \
 && apt-snapshot.sh "$DEBIAN_SNAPSHOT" cmake make g++ patch
