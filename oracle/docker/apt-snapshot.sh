#!/bin/sh
# Install Debian packages from snapshot.debian.org at a pinned timestamp, so
# an image rebuild gets the same package versions. Usage:
#   apt-snapshot.sh <YYYYMMDDTHHMMSSZ> <package>...
set -eu
snap="$1"
shift
. /etc/os-release
cat > /etc/apt/sources.list.d/debian.sources <<EOF
Types: deb
URIs: http://snapshot.debian.org/archive/debian/$snap
Suites: $VERSION_CODENAME $VERSION_CODENAME-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp

Types: deb
URIs: http://snapshot.debian.org/archive/debian-security/$snap
Suites: $VERSION_CODENAME-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
EOF
apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@"
rm -rf /var/lib/apt/lists/*
