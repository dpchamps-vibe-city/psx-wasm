# psxw-browsers: headless Chromium and Firefox ESR for the browser host's
# tests (plans/M05-browser.md, D44), from the Debian snapshot the oracle
# images use, on the same base image. A page under test POSTs its result to
# the in-container server (browser-serve.py); run-browsers.sh drives each
# browser and waits for it. No network: everything is served from /www.
#
# Build context: oracle/. Pins: [oracle] in inputs.lock.

ARG BASE_IMAGE
FROM ${BASE_IMAGE}
SHELL ["/bin/sh", "-eu", "-c"]
ARG DEBIAN_SNAPSHOT
COPY docker/apt-snapshot.sh /usr/local/bin/apt-snapshot.sh
RUN chmod +x /usr/local/bin/apt-snapshot.sh \
 && apt-snapshot.sh "$DEBIAN_SNAPSHOT" chromium firefox-esr python3 libgl1-mesa-dri libegl-mesa0

COPY docker/browser-serve.py docker/run-browsers.sh /opt/psxw/
RUN chmod +x /opt/psxw/run-browsers.sh \
 && printf '{"debian_snapshot": "%s", "chromium": "%s", "firefox_esr": "%s", "mesa": "%s"}\n' \
      "$DEBIAN_SNAPSHOT" "$(dpkg-query -W -f '${Version}' chromium)" \
      "$(dpkg-query -W -f '${Version}' firefox-esr)" "$(dpkg-query -W -f '${Version}' libegl-mesa0)" \
      > /opt/psxw/sources.json

ENV HOME=/tmp LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe MOZ_HEADLESS=1
LABEL org.psxw.image="psxw-browsers"
ENTRYPOINT ["/opt/psxw/run-browsers.sh"]
