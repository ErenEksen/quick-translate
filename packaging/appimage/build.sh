#!/usr/bin/env bash
# Build the quick-translate AppImage (tasks.md 9.1, design D11).
#
#   packaging/appimage/build.sh [options]
#
# By default the build runs inside an ubuntu:22.04 container (glibc 2.35
# baseline) using podman (or docker), with the repository mounted read-only.
# Outputs go to dist/:
#   dist/quick-translate-<version>-x86_64.AppImage
#   dist/quick-translate-<version>-x86_64.AppImage.zsync   (when update info is embedded)
#   dist/AppDir/                                            (the assembled AppDir)
#
# Options:
#   --no-container      Run the build steps directly on this machine. Meant for CI
#                       jobs that already run inside ubuntu:22.04 (add --install-deps).
#   --install-deps      With --no-container: apt-get install the build dependencies.
#   --update-info STR   AppImage update information to embed
#                       (default: gh-releases-zsync|quicktranslate|quick-translate|latest|quick-translate-*-x86_64.AppImage.zsync).
#                       A .zsync file is written when zsyncmake is available
#                       (always the case in container mode).
#   --no-update-info    Embed no update information and write no .zsync.
#   --no-smoke          Skip the post-build `--version` smoke tests on the host.
#   -h, --help          Show this help.
#
# Environment:
#   CONTAINER_ENGINE    podman (default if installed) or docker.
#   VERSION             Override the version (default: [workspace.package] version).
#   RUST_TOOLCHAIN      rustup toolchain to install in the container (default: stable).
#   QT_VOLUME_PREFIX    Prefix of the cache volumes (default: qt-appimage).
#   QT_ICON             Icon to bundle (default: packaging/icons/io.github.quicktranslate.QuickTranslate.svg).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${QT_REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
OUT_DIR="${QT_OUT_DIR:-$REPO_ROOT/dist}"

APP_ID="io.github.quicktranslate.QuickTranslate"
BIN_NAME="quick-translate"
ARCH="x86_64"
BUILD_IMAGE="docker.io/library/ubuntu:22.04"

# Pinned tools. appimagetool embeds the runtime given by --runtime-file; we use
# the static type2 runtime, which needs no libfuse2 on the host.
APPIMAGETOOL_VERSION="1.9.1"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-${ARCH}.AppImage"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
RUNTIME_VERSION="20251108"
RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/${RUNTIME_VERSION}/runtime-${ARCH}"
RUNTIME_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"

DEFAULT_UPDATE_INFO="gh-releases-zsync|quicktranslate|quick-translate|latest|quick-translate-*-${ARCH}.AppImage.zsync"

MODE="container"
INSTALL_DEPS=0
UPDATE_INFO="$DEFAULT_UPDATE_INFO"
SMOKE=1
INSIDE=0

usage() { sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --no-container) MODE="host" ;;
        --install-deps) INSTALL_DEPS=1 ;;
        --update-info) UPDATE_INFO="${2:?--update-info needs a value}"; shift ;;
        --no-update-info) UPDATE_INFO="" ;;
        --no-smoke) SMOKE=0 ;;
        --inside-container) INSIDE=1; MODE="host"; INSTALL_DEPS=1 ;; # internal
        -h|--help) usage; exit 0 ;;
        *) echo "build.sh: unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

workspace_version() {
    sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p}' "$REPO_ROOT/Cargo.toml" | head -n1
}
VERSION="${VERSION:-$(workspace_version)}"
VERSION="${VERSION#v}"
[ -n "$VERSION" ] || { echo "build.sh: could not determine the version" >&2; exit 1; }
APPIMAGE_NAME="${BIN_NAME}-${VERSION}-${ARCH}.AppImage"

# ---------------------------------------------------------------------------
# Container mode: re-run this script inside ubuntu:22.04.
# ---------------------------------------------------------------------------
run_in_container() {
    local engine="${CONTAINER_ENGINE:-}"
    if [ -z "$engine" ]; then
        if command -v podman >/dev/null; then engine=podman
        elif command -v docker >/dev/null; then engine=docker
        else echo "build.sh: need podman or docker (or use --no-container)" >&2; exit 1; fi
    fi
    local vp="${QT_VOLUME_PREFIX:-qt-appimage}"
    mkdir -p "$OUT_DIR"

    local args=() mounts=()
    if [ -n "$UPDATE_INFO" ]; then args+=(--update-info "$UPDATE_INFO"); else args+=(--no-update-info); fi
    # QT_ICON overrides the icon path; make it visible inside the container.
    if [ -n "${QT_ICON:-}" ]; then
        mounts+=(-v "$(readlink -f "$QT_ICON"):/icon.svg:ro" -e QT_ICON=/icon.svg)
    fi

    log "Building in $BUILD_IMAGE with $engine (version $VERSION)"
    # Source is read-only; the target dir, cargo home, rustup and the tool
    # downloads live in named volumes so reruns are incremental.
    "$engine" run --rm \
        -v "$REPO_ROOT:/src:ro" \
        -v "$OUT_DIR:/out" \
        -v "${vp}-target:/target" \
        -v "${vp}-cargo:/root/.cargo" \
        -v "${vp}-rustup:/root/.rustup" \
        -v "${vp}-tools:/tools" \
        -e VERSION="$VERSION" \
        -e RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-stable}" \
        -e QT_REPO_ROOT=/src \
        -e QT_OUT_DIR=/out \
        -e QT_TARGET_DIR=/target \
        -e QT_TOOLS_DIR=/tools \
        -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}" \
        "${mounts[@]}" \
        -w /src \
        "$BUILD_IMAGE" \
        bash /src/packaging/appimage/build.sh --inside-container "${args[@]}"
}

# ---------------------------------------------------------------------------
# Build steps (inside the container, or directly with --no-container).
# ---------------------------------------------------------------------------
install_deps() {
    log "Installing build dependencies"
    export DEBIAN_FRONTEND=noninteractive
    local sudo=""
    [ "$(id -u)" -eq 0 ] || sudo="sudo"
    $sudo apt-get update -qq
    $sudo apt-get install -y -qq --no-install-recommends \
        ca-certificates curl build-essential pkg-config \
        libwayland-dev libxkbcommon-dev \
        zsync desktop-file-utils file >/dev/null
}

ensure_rust() {
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    if [ "$INSIDE" -eq 1 ]; then
        if ! command -v rustup >/dev/null; then
            log "Installing rustup"
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
                | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
        fi
        rustup toolchain install "${RUST_TOOLCHAIN:-stable}" --profile minimal
        rustup default "${RUST_TOOLCHAIN:-stable}"
    fi
    command -v cargo >/dev/null || { echo "build.sh: cargo not found" >&2; exit 1; }
    cargo --version
}

fetch() { # url sha256 dest
    local url="$1" sum="$2" dest="$3"
    if [ -f "$dest" ] && echo "$sum  $dest" | sha256sum -c --status; then
        return 0
    fi
    log "Downloading $url"
    curl -fsSL --retry 3 -o "$dest.part" "$url"
    echo "$sum  $dest.part" | sha256sum -c --quiet
    mv "$dest.part" "$dest"
}

build_appimage() {
    local target_dir="${QT_TARGET_DIR:-$REPO_ROOT/target/appimage}"
    local tools_dir="${QT_TOOLS_DIR:-$OUT_DIR/.tools}"
    mkdir -p "$OUT_DIR" "$tools_dir"

    log "cargo build --release --locked -p $BIN_NAME"
    (cd "$REPO_ROOT" && cargo build --release --locked -p "$BIN_NAME" --target-dir "$target_dir")
    local bin="$target_dir/release/$BIN_NAME"
    [ -x "$bin" ] || { echo "build.sh: $bin not built" >&2; exit 1; }

    log "Assembling AppDir"
    local appdir="$OUT_DIR/AppDir"
    rm -rf "$appdir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
             "$appdir/usr/share/icons/hicolor/scalable/apps"
    install -m755 "$bin" "$appdir/usr/bin/$BIN_NAME"
    install -m755 "$SCRIPT_DIR/AppRun" "$appdir/AppRun"
    sed "s/@VERSION@/$VERSION/" "$SCRIPT_DIR/$APP_ID.desktop" > "$appdir/usr/share/applications/$APP_ID.desktop"
    ln -s "usr/share/applications/$APP_ID.desktop" "$appdir/$APP_ID.desktop"

    local icon="${QT_ICON:-$REPO_ROOT/packaging/icons/$APP_ID.svg}"
    [ -f "$icon" ] || { echo "build.sh: icon not found: $icon" >&2; exit 1; }
    install -m644 "$icon" "$appdir/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
    ln -s "usr/share/icons/hicolor/scalable/apps/$APP_ID.svg" "$appdir/$APP_ID.svg"
    ln -s "$APP_ID.svg" "$appdir/.DirIcon"

    if command -v desktop-file-validate >/dev/null; then
        desktop-file-validate "$appdir/usr/share/applications/$APP_ID.desktop"
    fi

    log "Auditing AppDir"
    bash "$SCRIPT_DIR/audit.sh" "$appdir"

    local appimagetool="$tools_dir/appimagetool-$APPIMAGETOOL_VERSION-$ARCH.AppImage"
    local runtime="$tools_dir/runtime-$ARCH-$RUNTIME_VERSION"
    fetch "$APPIMAGETOOL_URL" "$APPIMAGETOOL_SHA256" "$appimagetool"
    fetch "$RUNTIME_URL" "$RUNTIME_SHA256" "$runtime"
    chmod +x "$appimagetool"

    local out="$OUT_DIR/$APPIMAGE_NAME"
    rm -f "$out" "$out.zsync"
    local tool_args=(--runtime-file "$runtime")
    if [ -n "$UPDATE_INFO" ]; then
        tool_args+=(--updateinformation "$UPDATE_INFO")
        command -v zsyncmake >/dev/null || echo "note: zsyncmake not installed; no .zsync will be written"
    fi
    log "Running appimagetool $APPIMAGETOOL_VERSION (static type2 runtime $RUNTIME_VERSION)"
    # appimagetool is itself an AppImage; extract-and-run avoids needing FUSE in containers.
    # appimagetool writes the .zsync into the current directory.
    (cd "$OUT_DIR" && APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" VERSION="$VERSION" \
        "$appimagetool" --no-appstream "${tool_args[@]}" "$appdir" "$out")
    [ -f "$out" ] || { echo "build.sh: appimagetool produced no $out" >&2; exit 1; }
    chmod +x "$out"

    log "Auditing $APPIMAGE_NAME"
    bash "$SCRIPT_DIR/audit.sh" "$out"

    log "Smoke test (extract-and-run): $APPIMAGE_NAME --version"
    APPIMAGE_EXTRACT_AND_RUN=1 "$out" --version

    log "Done"
    ls -l "$out" "$out.zsync" 2>/dev/null || true
}

smoke_host() {
    local out="$OUT_DIR/$APPIMAGE_NAME"
    log "Host smoke test (FUSE mount): $APPIMAGE_NAME --version"
    "$out" --version
    log "Host smoke test (APPIMAGE_EXTRACT_AND_RUN=1): $APPIMAGE_NAME --version"
    APPIMAGE_EXTRACT_AND_RUN=1 "$out" --version
}

if [ "$MODE" = "container" ]; then
    run_in_container
    if [ "$SMOKE" -eq 1 ]; then smoke_host; fi
else
    if [ "$INSTALL_DEPS" -eq 1 ]; then install_deps; fi
    ensure_rust
    build_appimage
fi
