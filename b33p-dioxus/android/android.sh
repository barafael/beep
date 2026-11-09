#!/usr/bin/env bash
# Sets up, runs and tears down everything the Android client (b33p-dioxus) needs:
# a JDK, the Android SDK and NDK, an emulator and its virtual device, the Rust targets and `dx`.
#
# All of it lives in the user's home, nothing needs root. Run `android.sh help` for the commands.
#
# What this script never touches: anything that is not ours. The emulator is found by its AVD
# name (`beep_pixel`), `adb` is always told the serial (`emulator-5554`), and no process is
# killed by the name `adb`: the test box (`just testbox`) runs its own emulator and adb servers
# in a Podman pod, whose processes show up in this user's process list too.
set -euo pipefail

ROOT=${B33P_ANDROID_ROOT:-$HOME/Android}
SDK=$ROOT/Sdk
LOGS=$ROOT/logs
JDK_ROOT=$HOME/.local/jdk
JDK_NAME='jdk-17.0.21+9'
JDK_URL='https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.21%2B9/OpenJDK17U-jdk_x64_linux_hotspot_17.0.21_9.tar.gz'
CMDLINE_URL='https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip'
# Google's repository2-3.xml lists this for that archive.
CMDLINE_SHA1='d313adb7aedccf6cf0cfca51ec180f0059f5f8f8'
NDK_VERSION='29.0.14206865'
IMAGE='system-images;android-35;google_apis;x86_64'
DX_VERSION='0.7.10'
RUST_TARGETS=(aarch64-linux-android x86_64-linux-android) # a phone; the emulator

AVD=beep_pixel
PORT=5554
SERIAL=emulator-$PORT
ADB_PORT=5037
PACKAGE=io.github.barafael.b33p
EMU_MEMORY=${B33P_EMU_MEMORY:-4096}
EMU_CORES=${B33P_EMU_CORES:-6}

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
APK=$REPO/target/dx/b33p-dioxus/debug/android/app/app/build/outputs/apk/debug/app-debug.apk
export JAVA_HOME=$JDK_ROOT/$JDK_NAME

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }
adbs() { adb -s "$SERIAL" "$@"; }
# (No `cmd | grep -q` anywhere: under pipefail the early exit of grep fails the pipeline.)
port_busy() { [[ -n $(ss -ltnH "sport = :$1" 2>/dev/null || true) ]]; }

# The Android environment, for `eval "$(android.sh env)"` or `android.sh env fish | source`.
print_env() {
    local path="$JAVA_HOME/bin:$SDK/emulator:$SDK/platform-tools:$SDK/cmdline-tools/latest/bin"
    case ${1:-sh} in
    sh)
        cat <<EOF
export JAVA_HOME="$JAVA_HOME"
export ANDROID_HOME="$SDK"
export ANDROID_SDK_ROOT="$SDK"
export ANDROID_NDK_HOME="$SDK/ndk/$NDK_VERSION"
export NDK_HOME="$SDK/ndk/$NDK_VERSION"
export PATH="$path:\$PATH"
EOF
        ;;
    fish)
        cat <<EOF
set -gx JAVA_HOME $JAVA_HOME
set -gx ANDROID_HOME $SDK
set -gx ANDROID_SDK_ROOT $SDK
set -gx ANDROID_NDK_HOME $SDK/ndk/$NDK_VERSION
set -gx NDK_HOME $SDK/ndk/$NDK_VERSION
fish_add_path ${path//:/ }
EOF
        ;;
    *) die "env: 'sh' or 'fish'" ;;
    esac
}

apply_env() {
    export ANDROID_HOME=$SDK ANDROID_SDK_ROOT=$SDK
    export ANDROID_NDK_HOME=$SDK/ndk/$NDK_VERSION NDK_HOME=$SDK/ndk/$NDK_VERSION
    export PATH="$JAVA_HOME/bin:$SDK/emulator:$SDK/platform-tools:$SDK/cmdline-tools/latest/bin:$PATH"
}

# The pids among the arguments that are not zombies (a killed emulator's parent may not have
# reaped it yet; `[emulator] <defunct>` is gone for all purposes).
alive() {
    local pid
    for pid in "$@"; do
        [[ $(ps -o stat= -p "$pid" 2>/dev/null) == Z* ]] || printf '%s\n' "$pid"
    done
}

# The emulator and its qemu, found by the AVD they run: never by `adb`, never the test box's.
emulator_pids() {
    local pids pid comm
    pids=$(pgrep -u "$(id -u)" -f -- "-avd $AVD( |\$)" || true)
    for pid in $pids; do
        comm=$(ps -o comm= -p "$pid" 2>/dev/null || true)
        case $comm in emulator* | qemu-system*) alive "$pid" ;; esac
    done
}

gradle_pids() {
    local pids
    pids=$(pgrep -u "$(id -u)" -f 'org.gradle.launcher.daemon.bootstrap.GradleDaemon' || true)
    [[ -z $pids ]] || alive $pids
}

booted() {
    [[ $(adbs shell getprop sys.boot_completed 2>/dev/null | tr -d '\r' || true) == 1 ]] &&
        adbs shell pm path android >/dev/null 2>&1
}

dir_size() { du -sh "$1" 2>/dev/null | cut -f1 || true; }

# --- setup ------------------------------------------------------------------------------------

TMP=''
scratch() {
    if [[ -z $TMP ]]; then
        TMP=$(mktemp -d "${TMPDIR:-/tmp}/b33p-android.XXXXXX")
        trap 'rm -rf "$TMP"' EXIT
    fi
}

setup_jdk() {
    if [[ -x $JAVA_HOME/bin/java ]]; then
        say "jdk          present ($JAVA_HOME)"
        return
    fi
    scratch
    say "jdk          downloading Temurin $JDK_NAME"
    curl -fL --retry 3 -o "$TMP/jdk.tar.gz" "$JDK_URL"
    local want
    want=$(curl -fsSL --retry 3 "$JDK_URL.sha256.txt" | awk '{print $1}')
    [[ -n $want && $(sha256sum "$TMP/jdk.tar.gz" | awk '{print $1}') == "$want" ]] ||
        die "the JDK's checksum does not match"
    mkdir -p "$JDK_ROOT"
    tar -xzf "$TMP/jdk.tar.gz" -C "$JDK_ROOT"
    [[ -x $JAVA_HOME/bin/java ]] || die "the JDK archive did not unpack to $JAVA_HOME"
}

setup_cmdline_tools() {
    if [[ -x $SDK/cmdline-tools/latest/bin/sdkmanager ]]; then
        say "cmdline      present"
        return
    fi
    have unzip || die "unzip is needed"
    scratch
    say "cmdline      downloading the Android command-line tools"
    curl -fL --retry 3 -o "$TMP/cmdline.zip" "$CMDLINE_URL"
    [[ $(sha1sum "$TMP/cmdline.zip" | awk '{print $1}') == "$CMDLINE_SHA1" ]] ||
        die "the command-line tools' checksum does not match"
    mkdir -p "$TMP/unpacked" "$SDK/cmdline-tools"
    unzip -q "$TMP/cmdline.zip" -d "$TMP/unpacked"
    rm -rf "$SDK/cmdline-tools/latest"
    mv "$TMP/unpacked/cmdline-tools" "$SDK/cmdline-tools/latest"
}

setup_sdk_packages() {
    local accept=$1 want=() pkg
    # sdkmanager install name | what it leaves behind
    while IFS='|' read -r pkg path; do
        [[ -e $SDK/$path ]] || want+=("$pkg")
    done <<EOF
platform-tools|platform-tools/adb
emulator|emulator/emulator
platforms;android-34|platforms/android-34
platforms;android-35|platforms/android-35
build-tools;34.0.0|build-tools/34.0.0
build-tools;35.0.0|build-tools/35.0.0
ndk;$NDK_VERSION|ndk/$NDK_VERSION
$IMAGE|system-images/android-35/google_apis/x86_64
EOF
    if ((${#want[@]} == 0)); then
        say "sdk          present (platforms 34 and 35, NDK $NDK_VERSION, system image)"
        return
    fi
    if [[ $accept == 1 ]]; then
        say "sdk          accepting the Android SDK licenses (--accept-licenses)"
        sdkmanager --sdk_root="$SDK" --licenses < <(yes) >/dev/null
    elif [[ ! -d $SDK/licenses ]]; then
        say "sdk          the Android SDK licenses are to be read and accepted (or: --accept-licenses)"
        sdkmanager --sdk_root="$SDK" --licenses
    fi
    say "sdk          installing: ${want[*]}"
    sdkmanager --sdk_root="$SDK" --install "${want[@]}"
}

setup_avd() {
    local config=$HOME/.android/avd/$AVD.avd/config.ini
    if [[ -f $HOME/.android/avd/$AVD.ini ]]; then
        say "avd          present ($AVD)"
        return
    fi
    say "avd          creating $AVD (Pixel 7, Android 15)"
    echo no | avdmanager create avd -n "$AVD" -k "$IMAGE" -d pixel_7 >/dev/null
    # Room for a 290 MB debug APK, the app's data and the copies Gradle makes.
    if grep -q '^disk.dataPartition.size' "$config"; then
        sed -i 's/^disk.dataPartition.size.*/disk.dataPartition.size = 6442450944/' "$config"
    else
        echo 'disk.dataPartition.size = 6442450944' >>"$config"
    fi
}

setup_rust() {
    have rustup || die "rustup is needed"
    local installed target missing=()
    installed=$(rustup target list --installed)
    for target in "${RUST_TARGETS[@]}"; do
        grep -qx "$target" <<<"$installed" || missing+=("$target")
    done
    if ((${#missing[@]})); then
        say "rust         adding targets: ${missing[*]}"
        rustup target add "${missing[@]}"
    else
        say "rust         targets present (${RUST_TARGETS[*]})"
    fi
    if have dx && [[ $(dx --version 2>/dev/null) == *"$DX_VERSION"* ]]; then
        say "dx           present ($DX_VERSION)"
    else
        say "dx           installing dioxus-cli $DX_VERSION (compiles it: a few minutes)"
        cargo install dioxus-cli --locked --version "$DX_VERSION"
    fi
}

cmd_setup() {
    local accept=0 arg
    for arg in "$@"; do
        case $arg in
        --accept-licenses) accept=1 ;;
        *) die "setup: unknown option $arg" ;;
        esac
    done
    have curl || die "curl is needed"
    mkdir -p "$ROOT" "$LOGS"
    setup_jdk
    setup_cmdline_tools
    setup_sdk_packages "$accept"
    setup_avd
    setup_rust
    print_env sh >"$ROOT/env.sh"
    print_env fish >"$ROOT/env.fish"
    say "env          $ROOT/env.sh and env.fish written"
    local tool
    for tool in cmake clang protoc; do
        have "$tool" || warn "$tool is missing: the real client (--features signal) needs it to build"
    done
    [[ -w /dev/kvm ]] || warn "/dev/kvm is not writable: the x86_64 emulator needs it"
    say ""
    say "Next: android.sh up, then build, install, run."
}

# --- the emulator -----------------------------------------------------------------------------

cmd_up() {
    local headless=0 wipe=0 arg
    for arg in "$@"; do
        case $arg in
        --headless) headless=1 ;;
        --wipe) wipe=1 ;;
        *) die "up: unknown option $arg" ;;
        esac
    done
    have emulator || die "no emulator: run 'android.sh setup'"
    [[ -w /dev/kvm ]] || die "/dev/kvm is not writable: the x86_64 emulator needs it"
    adb start-server >/dev/null

    local log=''
    if [[ -n $(emulator_pids) ]]; then
        say "$AVD is running"
    else
        port_busy "$PORT" && die "port $PORT is taken by something that is not $AVD"
        port_busy $((PORT + 1)) && die "port $((PORT + 1)) is taken by something that is not $AVD"
        mkdir -p "$LOGS"
        log=$LOGS/emulator-$(date +%Y%m%d-%H%M%S).log
        local args=(-avd "$AVD" -port "$PORT" -no-snapshot-save -no-boot-anim -gpu auto
            -memory "$EMU_MEMORY" -cores "$EMU_CORES")
        ((headless)) && args+=(-no-window -no-audio)
        ((wipe)) && args+=(-wipe-data)
        say "starting $AVD (log: $log)"
        setsid nohup emulator "${args[@]}" >"$log" 2>&1 </dev/null &
        disown
    fi

    local waited
    for ((waited = 0; waited < 300; waited += 2)); do
        if booted; then
            say "$SERIAL is up after about ${waited}s"
            return
        fi
        [[ -n $(emulator_pids) ]] || die "the emulator exited${log:+: see $log}"
        sleep 2
    done
    die "$SERIAL did not finish booting in 300 s${log:+: see $log}"
}

cmd_down() {
    # 1. the emulator, politely, after checking that port 5554 is ours
    # (Bounded: an emulator that has hung would hang adb's questions to it as well.)
    if have adb && port_busy "$ADB_PORT" && [[ $(timeout 10 adb -s "$SERIAL" get-state 2>/dev/null || true) == device ]]; then
        local name
        name=$(timeout 10 adb -s "$SERIAL" emu avd name 2>/dev/null | tr -d '\r' | head -n1) || true
        if [[ $name == "$AVD" ]]; then
            say "asking $SERIAL ($AVD) to shut down"
            timeout 10 adb -s "$SERIAL" emu kill >/dev/null 2>&1 || true
        elif [[ -z $name ]]; then
            warn "$SERIAL does not answer; ending its processes instead"
        else
            warn "$SERIAL is '$name', not $AVD: leaving it alone"
        fi
    fi
    local i
    for ((i = 0; i < 20; i++)); do
        [[ -z $(emulator_pids) ]] && break
        sleep 1
    done
    # 2. whatever is left of it
    local pids
    pids=$(emulator_pids)
    if [[ -n $pids ]]; then
        say "terminating the emulator's processes: $(tr '\n' ' ' <<<"$pids")"
        # shellcheck disable=SC2086 # one pid per word
        kill $pids 2>/dev/null || true
        for ((i = 0; i < 10; i++)); do
            [[ -z $(emulator_pids) ]] && break
            sleep 1
        done
        pids=$(emulator_pids)
        if [[ -n $pids ]]; then
            warn "killing: $(tr '\n' ' ' <<<"$pids")"
            # shellcheck disable=SC2086
            kill -9 $pids 2>/dev/null || true
            for ((i = 0; i < 5; i++)); do
                [[ -z $(emulator_pids) ]] && break
                sleep 1
            done
        fi
    fi
    # 3. Gradle's daemons, which dx's generated project leaves running after a build
    local gradlew found=0
    for gradlew in "$REPO"/target/dx/*/*/android/app/gradlew; do
        [[ -x $gradlew ]] || continue
        found=1
        say "stopping Gradle's daemons (every one of this Gradle version)"
        (cd "$(dirname "$gradlew")" && ./gradlew --stop >/dev/null 2>&1) || true
    done
    # (`--stop` returns before the daemons have exited.)
    for ((i = 0; i < 15; i++)); do
        pids=$(gradle_pids)
        [[ -z $pids ]] && break
        sleep 1
    done
    if [[ -n $pids ]]; then
        warn "Gradle daemons still running (pids: $(tr '\n' ' ' <<<"$pids")); they may be another project's"
    elif ((!found)); then
        say "no Gradle daemons"
    fi
    # 4. the adb server, if it has nothing left to serve (a phone on USB, or another emulator, keeps it)
    if have adb && port_busy "$ADB_PORT"; then
        for ((i = 0; i < 10; i++)); do
            [[ $(timeout 10 adb devices) == *"$SERIAL"* ]] || break
            sleep 1
        done
        if [[ $(adb devices | tail -n +2 | grep -c .) == 0 ]]; then
            say "stopping the adb server (no devices left)"
            adb kill-server
        else
            say "leaving the adb server running: devices are still attached"
        fi
    fi
    # 5. said and done?
    if [[ -n $(emulator_pids) ]]; then
        die "the emulator is still running"
    fi
    say "down: nothing of ours is running"
}

# --- the app ----------------------------------------------------------------------------------

need_device() {
    have adb || die "no adb: run 'android.sh setup'"
    booted || die "$SERIAL is not up: run 'android.sh up'"
}

cmd_build() {
    local kind=${1:-mock}
    have just || die "just is needed (the recipes are in the justfile)"
    cd "$REPO"
    case $kind in
    mock) exec just android-mock ;; # offline fake backend, x86_64
    signal) exec just android ;;    # real backend, x86_64
    phone) exec just android-phone ;; # real backend, arm64
    *) die "build: mock (default), signal or phone" ;;
    esac
}

cmd_install() {
    need_device
    [[ -f $APK ]] || die "no APK at $APK: run 'android.sh build'"
    local abi
    abi=$(adbs shell getprop ro.product.cpu.abi | tr -d '\r')
    [[ $(unzip -Z1 "$APK") == *"lib/$abi/libmain.so"* ]] ||
        die "the APK was not built for the emulator's $abi: 'android.sh build mock' or 'signal'"
    adbs install -r "$APK"
}

cmd_run() {
    need_device
    local out
    out=$(adbs shell monkey -p "$PACKAGE" -c android.intent.category.LAUNCHER 1 2>&1) ||
        die "could not start $PACKAGE: $out"
    [[ $out == *"No activities found"* ]] && die "$PACKAGE is not installed: 'android.sh install'"
    say "started $PACKAGE; its log: android.sh logs"
}

cmd_logs() {
    need_device
    adbs logcat "$@" -s RustStdoutStderr
}

cmd_reset() {
    need_device
    adbs shell pm clear "$PACKAGE"
}

cmd_shot() {
    need_device
    local file=${1:-$LOGS/shot-$(date +%Y%m%d-%H%M%S).png}
    mkdir -p "$(dirname "$file")"
    adbs exec-out screencap -p >"$file"
    say "$file"
}

# --- state ------------------------------------------------------------------------------------

row() { printf '%-14s %s\n' "$1" "$2"; }

cmd_status() {
    row jdk "$([[ -x $JAVA_HOME/bin/java ]] && "$JAVA_HOME/bin/java" -version 2>&1 | head -n1 || echo missing)"
    row sdk "$([[ -d $SDK ]] && echo "$SDK ($(dir_size "$SDK"))" || echo missing)"
    row ndk "$([[ -d $SDK/ndk/$NDK_VERSION ]] && echo "$NDK_VERSION" || echo missing)"
    row avd "$([[ -f $HOME/.android/avd/$AVD.ini ]] && echo "$AVD ($(dir_size "$HOME/.android/avd/$AVD.avd"))" || echo missing)"
    row dx "$(have dx && dx --version 2>/dev/null | head -n1 || echo missing)"
    row rust-targets "$(rustup target list --installed 2>/dev/null | grep android | tr '\n' ' ' || true)"
    row build-output "$([[ -d $REPO/target/dx ]] && dir_size "$REPO/target/dx" || echo none)"
    local pids
    pids=$(emulator_pids)
    row emulator "$([[ -n $pids ]] && echo "running (pids $(tr '\n' ' ' <<<"$pids"))" || echo stopped)"
    if have adb && port_busy "$ADB_PORT"; then
        row adb "server on $ADB_PORT; devices: $(adb devices | tail -n +2 | awk 'NF {printf "%s(%s) ", $1, $2}')"
        if booted; then
            row android "$(adbs shell getprop ro.build.version.release | tr -d '\r') ($(adbs shell getprop ro.product.cpu.abi | tr -d '\r'))"
            local installed
            installed=$(adbs shell pm path "$PACKAGE" 2>/dev/null | tr -d '\r' || true)
            row app "$([[ -n $installed ]] && echo "installed, pid: $(adbs shell pidof "$PACKAGE" 2>/dev/null | tr -d '\r' || true)" || echo 'not installed')"
        fi
    else
        row adb "no server"
    fi
    row gradle "$(gradle_pids | wc -l) daemon(s)"
}

# --- removing it ------------------------------------------------------------------------------

# rm -rf, but only below the home directory, only what we name.
remove() {
    [[ $1 == "$HOME"/?* ]] || die "refusing to remove $1"
    [[ -e $1 ]] && rm -rf -- "$1"
}

cmd_clean() {
    local item
    # dx's output, the Android targets, and the host half of dx's own profiles (android-dev, -release)
    for item in "$REPO/target/dx" "$REPO"/target/*-linux-android* "$REPO"/target/android-*; do
        [[ -e $item ]] || continue
        say "removing $item ($(dir_size "$item"))"
        rm -rf -- "$item"
    done
    say "clean: the Android build output is gone (the desktop builds in target/ are not touched)"
}

cmd_purge() {
    local yes=0 logs=0 arg
    for arg in "$@"; do
        case $arg in
        --yes) yes=1 ;;
        --logs) logs=1 ;;
        *) die "purge: unknown option $arg" ;;
        esac
    done
    local items=("$SDK" "$JDK_ROOT/$JDK_NAME" "$HOME/.android/avd/$AVD.avd" "$HOME/.android/avd/$AVD.ini"
        "$ROOT/env.sh" "$ROOT/env.fish" "$ROOT/tools")
    ((logs)) && items+=("$LOGS")
    local item
    if ((!yes)); then
        say "purge would remove (nothing is removed without --yes):"
        for item in "${items[@]}"; do [[ -e $item ]] && say "  $item ($(dir_size "$item"))"; done
        ((logs)) || say "  (the logs in $LOGS stay, unless --logs)"
        say "It would leave: ~/.gradle (shared with other Gradle builds), ~/.android's adb keys, the Rust"
        say "targets and dx ('rustup target remove ${RUST_TARGETS[*]}'; 'cargo uninstall dioxus-cli')."
        return
    fi
    cmd_down
    for item in "${items[@]}"; do
        [[ -e $item ]] || continue
        say "removing $item"
        remove "$item"
    done
    rmdir "$ROOT" 2>/dev/null || true
    rmdir "$JDK_ROOT" 2>/dev/null || true
    say "purge: done. 'android.sh setup' brings it back."
}

usage() {
    cat <<EOF
android.sh: everything the Android client needs, set up and torn down. (Under $ROOT.)

  setup [--accept-licenses]  JDK, Android SDK and NDK, emulator image, AVD $AVD, Rust targets,
                             dx. Idempotent: it installs what is missing and leaves the rest.
  up [--headless] [--wipe]   starts the emulator and waits until Android has booted
  down                       stops the emulator, Gradle's daemons and, if idle, the adb server
  status                     what is installed and what is running

  build [mock|signal|phone]  dx build, as the justfile has it (mock: offline fake, x86_64;
                             signal: real backend, x86_64; phone: real backend, arm64)
  install                    the last build onto the emulator (checks the CPU architecture)
  run                        starts the app
  logs [-d]                  the app's log (follows; -d prints what there is)
  reset                      wipes the app's data on the emulator
  shot [file]                a screenshot of the emulator

  clean                      removes the Android build output under target/
  purge [--yes] [--logs]     removes all of the above from the machine (lists it without --yes)

  env [sh|fish]              the environment, to evaluate:  eval "\$(android.sh env)"
EOF
}

main() {
    local cmd=${1:-help}
    shift || true
    apply_env
    case $cmd in
    setup) cmd_setup "$@" ;;
    up) cmd_up "$@" ;;
    down) cmd_down ;;
    status) cmd_status ;;
    build) cmd_build "$@" ;;
    install) cmd_install ;;
    run) cmd_run ;;
    logs) cmd_logs "$@" ;;
    reset) cmd_reset ;;
    shot) cmd_shot "$@" ;;
    clean) cmd_clean ;;
    purge) cmd_purge "$@" ;;
    env) print_env "$@" ;;
    help | -h | --help) usage ;;
    *)
        usage >&2
        exit 2
        ;;
    esac
}

main "$@"
