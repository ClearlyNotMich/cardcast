#!/bin/sh
# packaging/aur/cardcast-bin (#45) against the tarball CI just built, in an Arch container: the
# committed .SRCINFO matches the PKGBUILD, makepkg builds it, namcap passes, and the package
# installs with its dependencies and runs. Usage: aur.sh <cardcast-linux-x86_64.tar.gz>
set -e
tarball=$(realpath "$1")
cd "$(dirname "$0")/../.."
pacman -Syu --noconfirm --needed base-devel namcap pacman-contrib
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
grep -qx "pkgver=$version" packaging/aur/cardcast-bin/PKGBUILD || { echo "PKGBUILD pkgver is not Cargo.toml's $version"; exit 1; }

# makepkg refuses to run as root.
id builder > /dev/null 2>&1 || useradd -m builder
build=/home/builder/cardcast-bin
rm -rf "$build" && mkdir "$build"
cp packaging/aur/cardcast-bin/PKGBUILD packaging/aur/cardcast-bin/.SRCINFO "$build/"
# makepkg takes a source it finds in the build directory instead of downloading it.
cp "$tarball" "$build/cardcast-v$version-linux-x86_64.tar.gz"
chown -R builder "$build"
su builder -c "cd $build && makepkg --printsrcinfo | diff -u .SRCINFO - && updpkgsums && makepkg --noconfirm --nodeps"

pacman -U --noconfirm "$build"/cardcast-bin-*.pkg.tar.zst
# Command mode without a monitor: "not understood" (exit 2) shows the binary starts on Arch.
status=0; cardcast input || status=$?
[ "$status" = 2 ]

# After the install, so namcap finds the linked libraries' packages.
namcap "$build/PKGBUILD" "$build"/cardcast-bin-*.pkg.tar.zst | tee "$build/namcap.log"
# Kept, each explained in packaging/aur/README.md: the dynamic loader the binary names, and the
# dependencies cardcast runs or loads at run time, which namcap can't see used.
kept="Unused shared library '/usr/lib64/ld-linux-x86-64.so.2'\\|Dependency included, but may not be needed ('\\(pipewire\\|pipewire-audio\\|wireplumber\\|coreutils\\|vulkan-icd-loader\\|vulkan-driver\\|libxkbcommon\\|fontconfig\\)')"
if grep ' [EW]: ' "$build/namcap.log" | grep -v "$kept"; then echo "namcap: fix the above or explain it"; exit 1; fi
