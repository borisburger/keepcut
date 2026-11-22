#!/bin/bash
# Builds the Keepcut Flatpak and exports a single-file bundle to dist/.
#
# Usage (from anywhere):  packaging/linux/build-flatpak.sh [--install]
#   --install   also install the result for the current user (flatpak run io.github.borisburger.Keepcut)
#
# Needs flatpak + flatpak-builder and the Flathub remote (the runtime, SDK and Rust extension are
# installed on demand). The build state lives in $KEEPCUT_FLATPAK_WORK (default
# ~/.cache/keepcut-flatpak), outside the source tree, so a checkout on a slow or Windows
# filesystem (WSL /mnt/c) only gets read.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
app=io.github.borisburger.Keepcut
work=${KEEPCUT_FLATPAK_WORK:-$HOME/.cache/keepcut-flatpak}
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml")
bundle="$root/dist/Keepcut-$version-linux-x86_64.flatpak"

mkdir -p "$work" "$root/dist"

# flatpak-builder before 1.3 (e.g. 1.2.3 in AlmaLinux 9) runs appstream-compose, which current
# SDKs no longer ship; build from a copy of the manifest with that step turned off (the
# metainfo file is still installed, only the precomposed AppStream catalog is missing).
manifest="$here/$app.yml"
builder_version=$(flatpak-builder --version | awk '{print $2}')
if [ "$(printf '%s\n1.3\n' "$builder_version" | sort -V | head -1)" != "1.3" ]; then
  manifest="$here/.$app.local.yml"
  trap 'rm -f "$manifest"' EXIT
  sed 's/^command: keepcut$/command: keepcut\nappstream-compose: false/' "$here/$app.yml" > "$manifest"
  echo "flatpak-builder $builder_version: building without appstream-compose"
fi

flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --install-deps-from=flathub \
  --state-dir="$work/state" --repo="$work/repo" \
  "$work/build" "$manifest"

flatpak build-bundle \
  --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$work/repo" "$bundle" "$app"
echo "bundle: $bundle ($(du -h "$bundle" | cut -f1))"

if [ "${1:-}" = "--install" ]; then
  flatpak install --user -y --noninteractive --reinstall "$bundle"
  echo "installed: flatpak run $app"
fi
