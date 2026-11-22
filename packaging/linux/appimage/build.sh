#!/bin/bash
# Builds the Keepcut AppImage in an AlmaLinux 9 container, whatever Linux the host runs.
#
# Usage (from anywhere):  packaging/linux/appimage/build.sh
# Needs podman (or docker; set CONTAINER_ENGINE=docker). The built third-party libraries,
# downloads and the cargo cache are kept in the volume "keepcut-appimage-work", so later
# builds only rebuild Keepcut. Remove it with:  podman volume rm keepcut-appimage-work
# Output: dist/Keepcut-<version>-x86_64.AppImage
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker || true)}
if [ -z "$engine" ]; then
  echo "podman or docker is needed" >&2
  exit 1
fi
image=keepcut-appimage-el9
volume=keepcut-appimage-work

# Rootless podman uses the systemd user session for cgroups by default; where there is none
# (WSL with a failing user@.service, plain containers) it must manage them itself.
if [ "$(basename "$engine")" = podman ] && ! systemctl --user show-environment >/dev/null 2>&1; then
  engine_args=(--cgroup-manager=cgroupfs --events-backend=file)
  podman() { command podman "${engine_args[@]}" "$@"; }
  engine=podman
fi

"$engine" build -t "$image" -f "$here/Containerfile" "$here"
"$engine" volume inspect "$volume" >/dev/null 2>&1 || "$engine" volume create "$volume" >/dev/null

# SELinux hosts (Fedora, AlmaLinux on bare metal) need the source mount relabelled.
label=""
if command -v selinuxenabled >/dev/null && selinuxenabled; then
  label=":z"
fi

"$engine" run --rm \
  -v "$root:/src$label" \
  -v "$volume:/work" \
  -e KEEPCUT_APPIMAGE_WORK=/work \
  "$image" bash /src/packaging/linux/appimage/build-appimage.sh
