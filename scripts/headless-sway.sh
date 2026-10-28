#!/usr/bin/env bash
# Start, stop or run commands against a headless sway compositor for the Wayland
# integration tests (crates/quick-translate-platform/tests/wayland_*.rs, selection_*.rs).
#
# Usage:
#   scripts/headless-sway.sh start      # start sway, print the env for the tests
#   scripts/headless-sway.sh env        # print the env again (eval-able)
#   scripts/headless-sway.sh github-env # print KEY=VALUE lines for "$GITHUB_ENV"
#   scripts/headless-sway.sh exec CMD…  # run CMD against the headless compositor
#   scripts/headless-sway.sh stop
#
#   eval "$(scripts/headless-sway.sh start)"
#   cargo test -p quick-translate-platform
#   scripts/headless-sway.sh stop
#
# If `sway` is on PATH (CI: apt install sway wl-clipboard) it runs natively. Otherwise it runs
# rootless in podman (image built on first use from archlinux with sway + wl-clipboard), with a
# host directory mounted as the container's XDG_RUNTIME_DIR so host processes can use the socket.
# Set QT_HEADLESS_SWAY_MODE=podman|native to force a mode.
#
# The tests never touch the live session: they only read QT_TEST_XDG_RUNTIME_DIR and
# QT_TEST_WAYLAND_DISPLAY, and skip when those are unset.

set -euo pipefail

STATE_DIR="${QT_HEADLESS_SWAY_DIR:-${TMPDIR:-/tmp}/quick-translate-headless-sway}"
RUNTIME_DIR="$STATE_DIR/runtime"
IMAGE="${QT_HEADLESS_SWAY_IMAGE:-localhost/quick-translate-headless-sway:latest}"
CONTAINER="${QT_HEADLESS_SWAY_CONTAINER:-quick-translate-headless-sway}"
CONTAINER_RUNTIME_DIR=/run/qt

mode() {
    if [[ -n "${QT_HEADLESS_SWAY_MODE:-}" ]]; then
        echo "$QT_HEADLESS_SWAY_MODE"
    elif command -v sway >/dev/null 2>&1; then
        echo native
    else
        echo podman
    fi
}

socket_name() {
    local s
    for s in "$RUNTIME_DIR"/wayland-*; do
        [[ -S "$s" ]] && { basename "$s"; return 0; }
    done
    return 1
}

print_env() {
    local name
    name="$(socket_name)" || { echo "headless sway is not running" >&2; exit 1; }
    echo "export QT_TEST_XDG_RUNTIME_DIR='$RUNTIME_DIR'"
    echo "export QT_TEST_WAYLAND_DISPLAY='$name'"
}

build_image() {
    podman image exists "$IMAGE" && return 0
    echo "building $IMAGE" >&2
    podman build -t "$IMAGE" -f - "$STATE_DIR" >&2 <<'EOF'
FROM docker.io/library/archlinux:latest
# sway ships with cap_sys_nice, which a rootless user namespace refuses to exec.
RUN pacman -Syu --noconfirm --needed sway wl-clipboard libcap && pacman -Scc --noconfirm \
    && setcap -r /usr/bin/sway
EOF
}

start() {
    if socket_name >/dev/null 2>&1; then
        print_env
        return 0
    fi
    mkdir -p "$RUNTIME_DIR"
    chmod 700 "$RUNTIME_DIR"
    : >"$STATE_DIR/sway.config"

    case "$(mode)" in
    native)
        env -u WAYLAND_DISPLAY -u DISPLAY -u SWAYSOCK \
            XDG_RUNTIME_DIR="$RUNTIME_DIR" \
            WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
            sway -c "$STATE_DIR/sway.config" >"$STATE_DIR/sway.log" 2>&1 &
        echo $! >"$STATE_DIR/sway.pid"
        ;;
    podman)
        build_image
        podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
        podman run -d --rm --name "$CONTAINER" --userns=keep-id \
            -v "$RUNTIME_DIR:$CONTAINER_RUNTIME_DIR:z" \
            -v "$STATE_DIR/sway.config:/etc/qt-sway.config:ro,z" \
            -e XDG_RUNTIME_DIR="$CONTAINER_RUNTIME_DIR" \
            -e WLR_BACKENDS=headless -e WLR_LIBINPUT_NO_DEVICES=1 -e WLR_RENDERER=pixman \
            "$IMAGE" sway -c /etc/qt-sway.config >/dev/null
        ;;
    *)
        echo "unknown mode: $(mode)" >&2
        exit 2
        ;;
    esac

    for _ in $(seq 100); do
        socket_name >/dev/null 2>&1 && { print_env; return 0; }
        sleep 0.1
    done
    echo "headless sway did not create a socket in $RUNTIME_DIR" >&2
    [[ -f "$STATE_DIR/sway.log" ]] && cat "$STATE_DIR/sway.log" >&2
    podman logs "$CONTAINER" >&2 2>/dev/null || true
    exit 1
}

stop() {
    if [[ -f "$STATE_DIR/sway.pid" ]]; then
        kill "$(cat "$STATE_DIR/sway.pid")" 2>/dev/null || true
        rm -f "$STATE_DIR/sway.pid"
    fi
    if command -v podman >/dev/null 2>&1; then
        podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
    fi
    rm -rf "$RUNTIME_DIR"
}

run() {
    local name
    name="$(socket_name)" || { echo "headless sway is not running" >&2; exit 1; }
    if [[ "$(mode)" == podman ]] && ! command -v "$1" >/dev/null 2>&1; then
        exec podman exec -e WAYLAND_DISPLAY="$name" "$CONTAINER" "$@"
    fi
    exec env -u DISPLAY XDG_RUNTIME_DIR="$RUNTIME_DIR" WAYLAND_DISPLAY="$name" "$@"
}

case "${1:-}" in
start) start ;;
stop) stop ;;
env) print_env ;;
github-env) print_env | sed -e 's/^export //' -e "s/'//g" ;;
exec)
    shift
    [[ $# -gt 0 ]] || { echo "exec needs a command" >&2; exit 2; }
    run "$@"
    ;;
*)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
