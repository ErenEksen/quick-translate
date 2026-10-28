#!/usr/bin/env bash
# Verify the flake (tasks.md 1.3 and 9.3) on a machine without Nix, using the
# docker.io/nixos/nix image under podman (or docker).
#
#   packaging/nix/check-in-container.sh [build|run|develop|all]   (default: all)
#
# The repository is mounted read-only and copied into the container without
# target/ and dist/. In the copy every file is `git add`-ed, so the plain git
# flake ref `.` sees exactly the working tree (uncommitted and untracked files
# included), the same as after a commit. The Nix store, cargo home and the dev
# shell's cargo target dir live in named volumes so reruns are fast.
# The built package is copied to dist/nix-result/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# QT_SRC: check another checkout/snapshot instead of this repository.
SRC="$(cd "${QT_SRC:-$REPO_ROOT}" && pwd)"
ENGINE="${CONTAINER_ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"
VP="${QT_VOLUME_PREFIX:-qt-nix}"
WHAT="${1:-all}"
# One cargo target volume per source tree: every tree is copied to /work, and
# cargo's mtime-based freshness check would otherwise reuse artifacts built
# from a different tree.
SRC_KEY="$(printf '%s' "$SRC" | sha256sum | cut -c1-12)"
mkdir -p "$REPO_ROOT/dist"

exec "$ENGINE" run --rm \
    -v "$SRC:/src:ro" \
    -v "$REPO_ROOT/dist:/out" \
    -v "${VP}-store:/nix" \
    -v "${VP}-cargo:/root/.cargo" \
    -v "${VP}-target-${SRC_KEY}:/cargo-target" \
    -e WHAT="$WHAT" \
    docker.io/nixos/nix:latest \
    bash -euo pipefail -c '
nix() { command nix --extra-experimental-features "nix-command flakes" --print-build-logs "$@"; }
mkdir /work
tar -C /src --exclude=./target --exclude=./dist --exclude=./result --exclude="./spikes/*/target" -cf - . | tar -C /work -xf -
cd /work
git config --global --add safe.directory /work
git config --global user.email ci@localhost; git config --global user.name ci
[ -d .git ] || git init -q
git add -A >/dev/null
if [ "$WHAT" = build ] || [ "$WHAT" = run ] || [ "$WHAT" = all ]; then
    echo "==> nix build"
    nix build
    ls -l result/bin/
    test -x result/bin/quick-translate
    rm -rf /out/nix-result && cp -rL result /out/nix-result && chmod -R u+w /out/nix-result
fi
if [ "$WHAT" = run ] || [ "$WHAT" = all ]; then
    echo "==> nix run . -- --version"
    nix run . -- --version
fi
if [ "$WHAT" = develop ] || [ "$WHAT" = all ]; then
    echo "==> nix develop -c cargo build"
    CARGO_TARGET_DIR=/cargo-target nix develop -c cargo build --locked
    echo "==> dev shell tools"
    nix develop -c sh -c "rustc --version; cargo --version; pkg-config --modversion wayland-client xkbcommon; command -v sway wl-copy"
fi
'
