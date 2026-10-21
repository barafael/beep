#!/bin/sh
# The Linux build dependencies, on Debian 12: for the dev container (Containerfile) and for the
# CI and release jobs, so all three install the same package list.
#
# cmake, clang: BoringSSL (contact discovery); protoc: protocol types; Qt 6: the UI backend.
# The GTK platform theme and the packaging tools are for the AppImage; curl fetches linuxdeploy.
set -eu

apt-get update
apt-get install -y --no-install-recommends \
    clang cmake pkg-config protobuf-compiler \
    qt6-base-dev qt6-base-dev-tools qt6-wayland qt6-gtk-platformtheme \
    libasound2-dev libfontconfig-dev libwayland-dev libxkbcommon-dev \
    file desktop-file-utils xz-utils curl
