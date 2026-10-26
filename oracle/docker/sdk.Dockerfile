# psxw-psx-sdk: PSn00bSDK at a pinned release, with the GCC (mipsel-none-elf)
# toolchain and mkpsxiso bundled in that release, for building oracle/runner.
#
# Build context: oracle/. The release archive is verified by sha256 from
# inputs.lock; `psxw oracle images build` passes the pins as build args.
# Run with oracle/ mounted read-only at /src and a writable /out:
#   docker run --rm --network=none -v $PWD/oracle:/src:ro -v OUT:/out psxw-psx-sdk

ARG BASE_IMAGE
FROM ${BASE_IMAGE}
SHELL ["/bin/sh", "-eu", "-c"]
ARG DEBIAN_SNAPSHOT
COPY docker/apt-snapshot.sh /usr/local/bin/apt-snapshot.sh
RUN chmod +x /usr/local/bin/apt-snapshot.sh \
 && apt-snapshot.sh "$DEBIAN_SNAPSHOT" ca-certificates curl unzip cmake make

ARG PSN00BSDK_RELEASE
ARG PSN00BSDK_URL
ARG PSN00BSDK_SHA256
ARG PSN00BSDK_GCC=12.3.0
RUN curl -fsSL --retry 5 -o /tmp/sdk.zip "$PSN00BSDK_URL" \
 && echo "$PSN00BSDK_SHA256  /tmp/sdk.zip" | sha256sum -c - \
 && unzip -q /tmp/sdk.zip -d /opt \
 && mv /opt/PSn00bSDK-* /opt/psn00bsdk \
 && rm /tmp/sdk.zip \
 && test "$(/opt/psn00bsdk/bin/mipsel-none-elf-gcc -dumpfullversion)" = "$PSN00BSDK_GCC" \
 && printf '{"psn00bsdk_release": "%s", "psn00bsdk_sha256": "%s", "gcc": "%s", "debian_snapshot": "%s"}\n' \
      "$PSN00BSDK_RELEASE" "$PSN00BSDK_SHA256" "$PSN00BSDK_GCC" "$DEBIAN_SNAPSHOT" > /opt/psn00bsdk/psxw-sources.json
ENV PATH=/opt/psn00bsdk/bin:$PATH
COPY docker/build-runner.sh /usr/local/bin/build-runner
RUN chmod +x /usr/local/bin/build-runner
LABEL org.psxw.image="psxw-psx-sdk"
ENTRYPOINT ["/usr/local/bin/build-runner"]
