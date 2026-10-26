#!/bin/sh
# Entry point of psxw-psx-sdk: build oracle/runner (oracle/ is mounted
# read-only at /src) into /out: psxw-runner.exe, its symbol map, and
# runner.json (sha256, image end, SDK facts). Generated disc files
# (xatest.xa) are mounted read-only at /gen.
set -eu
src=/src
out=/out
build=/tmp/runner-build
log=/tmp/runner-build.log
if ! cmake -S "$src/runner" -B "$build" -G Ninja \
     -DCMAKE_TOOLCHAIN_FILE=/opt/psn00bsdk/lib/libpsn00b/cmake/sdk.cmake \
     -DCMAKE_BUILD_TYPE=Release -DPSXW_INCLUDE="$src/include" > "$log" 2>&1 \
   || ! cmake --build "$build" >> "$log" 2>&1; then
  cat "$log" >&2
  exit 1
fi
# The runner image must end below the scratch region BIOS_CALL uses.
end=$(mipsel-none-elf-nm "$build/psxw-runner.elf" | awk '$3 == "_end" { print substr($1, length($1) - 7) }')
scratch=$(awk '/define PSXW_SCRATCH_BASE/ { print $3 }' "$src/include/psxw_oracle_wire.h" | tr -d u)
if [ "$((0x$end))" -gt "$((scratch))" ]; then
  echo "runner image ends at 0x$end, past the scratch region at $scratch" >&2
  exit 1
fi
cp "$build/psxw-runner.exe" "$build/psxw-runner.map" "$out/"

# The runner disc (oracle/runner/disc). Sources get a fixed mtime so the
# directory records, and so the image, are reproducible.
disc=/tmp/runner-disc
mkdir -p "$disc"
cp "$src/runner/disc/disc.xml" "$src/runner/disc/system.cnf" "$build/psxw-runner.exe" /gen/xatest.xa "$disc/"
touch -d '2000-01-01 00:00:00 UTC' "$disc"/*
(cd "$disc" && mkpsxiso -y -q disc.xml) >> "$log" 2>&1 || { cat "$log" >&2; exit 1; }
cp "$disc/psxw-runner.bin" "$disc/psxw-runner.cue" "$out/"

printf '{"exe_sha256": "%s", "disc_bin_sha256": "%s", "disc_cue_sha256": "%s", "image_end": "0x%s", "sdk": %s}\n' \
  "$(sha256sum "$out/psxw-runner.exe" | cut -d' ' -f1)" \
  "$(sha256sum "$out/psxw-runner.bin" | cut -d' ' -f1)" \
  "$(sha256sum "$out/psxw-runner.cue" | cut -d' ' -f1)" \
  "$end" "$(cat /opt/psn00bsdk/psxw-sources.json)" > "$out/runner.json"
cat "$out/runner.json"
