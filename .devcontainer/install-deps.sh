#!/bin/sh
# The Linux build dependencies, on Debian 12: for the dev container (Containerfile) and for the
# CI and release jobs, so all three install the same package list.
#
# cmake, clang: BoringSSL (contact discovery) and Skia's bindings; protoc: protocol types;
# The packaging tools are for the AppImage; curl fetches linuxdeploy.
set -eu

apt-get update
apt-get install -y --no-install-recommends \
    clang cmake pkg-config protobuf-compiler \
    libasound2-dev libfontconfig-dev libwayland-dev libxkbcommon-dev \
    file desktop-file-utils xz-utils curl
