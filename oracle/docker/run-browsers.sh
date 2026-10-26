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
      printf 'user_pref("webgl.force-enabled", true);\nuser_pref("browser.shell.checkDefaultBrowser", false);\n' > /tmp/firefox/user.js
      firefox-esr --headless --no-remote --profile /tmp/firefox "$url" > "/out/$b.log" 2>&1 &
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
