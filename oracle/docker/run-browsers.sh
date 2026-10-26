#!/bin/sh
# run-browsers.sh PAGE TIMEOUT_SECONDS BROWSER...: serve /www, open PAGE in
# each headless browser, wait for its /out/result-BROWSER.json.
set -u
page=$1
timeout=$2
shift 2
python3 /opt/psxw/browser-serve.py 8000 /www /out &
server=$!
sleep 0.5
for b in "$@"; do
  rm -f "/out/result-$b.json"
  case $page in
    *\?*) url="http://127.0.0.1:8000/$page&browser=$b" ;;
    *) url="http://127.0.0.1:8000/$page?browser=$b" ;;
  esac
  case $b in
    chromium)
      chromium --headless=new --no-sandbox --disable-dev-shm-usage --enable-unsafe-swiftshader \
        --use-angle=swiftshader --user-data-dir="/tmp/chromium" "$url" > "/out/$b.log" 2>&1 &
      ;;
    firefox)
      mkdir -p /tmp/firefox
      # Software GL (llvmpipe) counts as a "major performance caveat",
      # which makes WebGL creation fail unless told otherwise; and there
      # is no GPU process in a container.
      cat > /tmp/firefox/user.js <<'PREFS'
user_pref("webgl.force-enabled", true);
user_pref("webgl.disable-fail-if-major-performance-caveat", true);
user_pref("webgl.out-of-process", false);
user_pref("layers.gpu-process.enabled", false);
user_pref("browser.shell.checkDefaultBrowser", false);
PREFS
      xvfb-run -a -s "-screen 0 1280x1024x24" firefox-esr --no-remote --profile /tmp/firefox "$url" > "/out/$b.log" 2>&1 &
      ;;
    *) echo "unknown browser $b" > "/out/$b.log"; continue ;;
  esac
  pid=$!
  t=0
  while [ ! -s "/out/result-$b.json" ] && [ "$t" -lt $((timeout * 10)) ]; do
    sleep 0.1
    t=$((t + 1))
  done
  kill "$pid" 2>/dev/null
  sleep 0.5
  kill -9 "$pid" 2>/dev/null
done
kill "$server"
