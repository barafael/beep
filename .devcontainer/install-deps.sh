#!/bin/sh
# The Linux build dependencies, on Debian 12: for the dev container (Containerfile) and for the
# CI and release jobs, so all three install the same package list.
#
# cmake, clang: BoringSSL (contact discovery) and Skia's bindings; protoc: protocol types;
# libxkbcommon-x11, libxcb: the GPUI window's X11 backend, which it builds next to Wayland's.
# The packaging tools are for the AppImage; curl fetches linuxdeploy.
set -eu

apt-get update
apt-get install -y --no-install-recommends \
    clang cmake pkg-config protobuf-compiler \
    libasound2-dev libfontconfig-dev libwayland-dev libxkbcommon-dev \
    libxkbcommon-x11-dev libxcb1-dev libxcb-xkb-dev libx11-dev \
    file desktop-file-utils xz-utils curl
