#!/usr/bin/env bash
# Builds Lis App: installs lis-daemon and the OBS plugin on this PC, and puts the APK for the phone
# and the relay server's binary in output/.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/obs-studio/plugins/lis-camera/bin/64bit"
built_apk="$root/android/app/build/outputs/apk/debug/app-debug.apk"
output_dir="$root/output"
rust_target=aarch64-linux-android
relay_target=x86_64-unknown-linux-musl
port=47420

export ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"

do_daemon=true
do_plugin=true
do_apk=true
do_relay=true

usage() {
    cat <<USAGE
Usage: ./install.sh [option]

Installs lis-daemon to $bin_dir and the OBS plugin, and builds the Android app and the relay server
into output/.

  --daemon-only   Only the PC daemon
  --plugin-only   Only the OBS plugin
  --apk-only      Only the Android app
  --relay-only    Only the relay server
  -h, --help      Show this
USAGE
}

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}
need() { command -v "$1" >/dev/null || die "$1 not found. $2"; }

if (($# > 1)); then
    usage >&2
    die "use one option at a time"
fi

case ${1:-} in
    "") ;;
    --daemon-only) do_plugin=false do_apk=false do_relay=false ;;
    --plugin-only) do_daemon=false do_apk=false do_relay=false ;;
    --apk-only) do_daemon=false do_plugin=false do_relay=false ;;
    --relay-only) do_daemon=false do_plugin=false do_apk=false ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        die "unknown option: $1"
        ;;
esac

install_daemon() {
    step "Building the daemon"
    need cargo "Install Rust from https://rustup.rs"
    (cd "$root" && cargo build --release -p lis-daemon)

    local was_running=false
    pgrep -x lis-daemon >/dev/null && was_running=true

    step "Installing the daemon to $bin_dir"
    mkdir -p "$bin_dir"
    install -m755 "$root/target/release/lis-daemon" "$bin_dir/lis-daemon"

    if $was_running; then
        # The shell starts it again while Phone Sync is on
        "$bin_dir/lis-daemon" ctl '{"cmd":"quit"}' >/dev/null 2>&1 || true
        local waited=0
        while ((waited < 10)) && ! "$bin_dir/lis-daemon" ctl '{"cmd":"status"}' >/dev/null 2>&1; do
            sleep 1
            waited=$((waited + 1))
        done
        if "$bin_dir/lis-daemon" ctl '{"cmd":"status"}' >/dev/null 2>&1; then
            echo "Restarted the running daemon."
        else
            echo "Stopped the old daemon. Turn on Settings, Phone Sync to start the new one."
        fi
    else
        echo "Turn on Settings, Phone Sync in the shell to start it."
    fi
}

install_plugin() {
    step "Building the OBS plugin"
    need cargo "Install Rust from https://rustup.rs"
    need pkg-config "Install pkg-config"
    pkg-config --exists libobs || die "OBS's headers weren't found. Install obs-studio."
    pkg-config --exists libavcodec libavutil || die "FFmpeg's headers weren't found. Install ffmpeg."
    (cd "$root" && cargo build --release -p lis-camera)

    step "Installing the OBS plugin to $plugin_dir"
    mkdir -p "$plugin_dir" "$output_dir"
    install -m755 "$root/target/release/liblis_camera.so" "$plugin_dir/lis-camera.so"
    cp "$root/target/release/liblis_camera.so" "$output_dir/lis-camera.so"
    if pgrep -x obs >/dev/null; then
        echo "Restart OBS to load it. Then add the Lis Camera source."
    else
        echo "Add the Lis Camera source in OBS."
    fi
}

# Java ignores https_proxy, and Google's servers can be unreachable without it
gradle_proxy_args=()
read_proxy() {
    local proxy="${https_proxy:-${HTTPS_PROXY:-${all_proxy:-${ALL_PROXY:-}}}}"
    [[ -n $proxy ]] || return 0

    local scheme=http
    [[ $proxy == *://* ]] && scheme="${proxy%%://*}"
    local address="${proxy#*://}"
    address="${address##*@}"
    address="${address%%/*}"
    [[ $address == *:* ]] || return 0

    local host="${address%:*}" proxy_port="${address##*:}"
    case $scheme in
        socks*) gradle_proxy_args=("-DsocksProxyHost=$host" "-DsocksProxyPort=$proxy_port") ;;
        *)
            gradle_proxy_args=(
                "-Dhttp.proxyHost=$host" "-Dhttp.proxyPort=$proxy_port"
                "-Dhttps.proxyHost=$host" "-Dhttps.proxyPort=$proxy_port"
            )
            ;;
    esac
}

# Static with musl, so it runs on a server with an older glibc. Falls back to this PC's glibc.
build_relay() {
    step "Building the relay server"
    need cargo "Install Rust from https://rustup.rs"
    mkdir -p "$output_dir"

    local built=""
    if command -v rustup >/dev/null; then
        rustup target list --installed | grep -qx "$relay_target" || rustup target add "$relay_target" || true
        # ring's C parts need a musl compiler; clang can be one without installing musl
        local cc=()
        if ! command -v x86_64-linux-musl-gcc >/dev/null && command -v clang >/dev/null; then
            cc=("CC_x86_64_unknown_linux_musl=clang")
        fi
        if (cd "$root" && env "${cc[@]}" cargo build --release -p lis-relay --target "$relay_target"); then
            built="$root/target/$relay_target/release/lis-relay"
        fi
    fi
    if [[ -z $built ]]; then
        echo "No static build, so it needs a glibc as new as this PC's."
        (cd "$root" && cargo build --release -p lis-relay)
        built="$root/target/release/lis-relay"
    fi

    install -m755 "$built" "$output_dir/lis-relay"
    cp "$root/relay/deploy/lis-relay.service" "$root/relay/deploy/config.toml" "$output_dir/"
    echo "Relay: $output_dir/lis-relay, config.toml and lis-relay.service, for the server. See the README."
}

build_apk() {
    step "Building the APK"
    need cargo "Install Rust from https://rustup.rs"
    need java "Install a JDK, version 17 or newer"
    [[ -d $ANDROID_HOME ]] || die "no Android SDK in $ANDROID_HOME. Install it, or set ANDROID_HOME. See the README."

    if command -v rustup >/dev/null && ! rustup target list --installed | grep -qx "$rust_target"; then
        rustup target add "$rust_target"
    fi

    read_proxy
    (cd "$root/android" && ./gradlew "${gradle_proxy_args[@]}" assembleDebug --no-daemon)
    [[ -f $built_apk ]] || die "the build finished, but there's no APK at $built_apk"

    mkdir -p "$output_dir"
    cp "$built_apk" "$output_dir/lis-app.apk"
    echo "APK: $output_dir/lis-app.apk"
}

# Only reads the rules, as changing them needs sudo
firewall_hint() {
    [[ -r /etc/ufw/user.rules ]] || return 0
    grep -q '^ENABLED=yes' /etc/ufw/ufw.conf 2>/dev/null || return 0
    grep -q "$port" /etc/ufw/user.rules && return 0

    step "Firewall"
    cat <<HINT
ufw is on and has no rule for Lis App, so the phone can't reach this PC. To allow it:
  sudo ufw allow from 192.168.0.0/16 to any port $port proto tcp
  sudo ufw allow from 192.168.0.0/16 to any port 5353 proto udp
HINT
}

if $do_daemon; then
    install_daemon
fi

if $do_plugin; then
    install_plugin
fi

if $do_apk; then
    build_apk
fi

if $do_relay; then
    build_relay
fi

if $do_daemon; then
    firewall_hint
fi

step "Done"
