#!/usr/bin/env bash
# Builds Lis App: installs lis-daemon on this PC, and puts the APK in output/ for the phone.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
built_apk="$root/android/app/build/outputs/apk/debug/app-debug.apk"
output_apk="$root/output/lis-app.apk"
rust_target=aarch64-linux-android
port=47420

export ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"

do_daemon=true
do_apk=true

usage() {
    cat <<EOF
Usage: ./install.sh [options]

Installs lis-daemon to $bin_dir and builds the Android app into output/.

  --daemon-only   Only the PC daemon
  --apk-only      Only the Android app
  -h, --help      Show this
EOF
}

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}
need() { command -v "$1" >/dev/null || die "$1 not found. $2"; }

while (($#)); do
    case $1 in
        --daemon-only) do_apk=false ;;
        --apk-only) do_daemon=false ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown option: $1"
            ;;
    esac
    shift
done

if ! $do_daemon && ! $do_apk; then
    die "--daemon-only and --apk-only can't be used together"
fi

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

    mkdir -p "$(dirname "$output_apk")"
    cp "$built_apk" "$output_apk"
    echo "APK: $output_apk"
}

# Only reads the rules, as changing them needs sudo
firewall_hint() {
    [[ -r /etc/ufw/user.rules ]] || return 0
    grep -q '^ENABLED=yes' /etc/ufw/ufw.conf 2>/dev/null || return 0
    grep -q "$port" /etc/ufw/user.rules && return 0

    step "Firewall"
    cat <<EOF
ufw is on and has no rule for Lis App, so the phone can't reach this PC. To allow it:
  sudo ufw allow from 192.168.0.0/16 to any port $port proto tcp
  sudo ufw allow from 192.168.0.0/16 to any port 5353 proto udp
EOF
}

if $do_daemon; then
    install_daemon
fi

if $do_apk; then
    build_apk
fi

if $do_daemon; then
    firewall_hint
fi

step "Done"
