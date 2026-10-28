#!/usr/bin/env bash
# Bundle audit for the quick-translate AppImage (tasks.md 9.2, design D11).
#
# Usage: audit.sh <AppDir | file.AppImage>
#
# Fails (exit 1) if:
#   * glibc, the GPU stack (libGL, libEGL, libvulkan, libgbm, libdrm, Mesa/DRI
#     drivers, Vulkan ICDs, GLVND vendor files) or the host-provided Wayland
#     libraries (libwayland-*, libxkbcommon) are bundled inside the AppDir;
#   * the binary's direct dependencies (DT_NEEDED) or its full `ldd` closure
#     contain anything outside the allowlist below, or anything is "not found";
#   * the binary requires a glibc symbol version newer than the 2.35 baseline.
#
# Exit codes: 0 = clean, 1 = audit failure, 2 = usage / environment error.
set -euo pipefail

BIN_REL="usr/bin/quick-translate"
GLIBC_BASELINE="${QT_GLIBC_BASELINE:-2.35}"

# Libraries the binary may need from the host.
# glibc family + libgcc_s + the vDSO, plus libwayland-client and libxkbcommon.
ALLOW_RE='^(linux-vdso\.so\.1|ld-linux-x86-64\.so\.2|ld-linux-aarch64\.so\.1|libc\.so\.6|libm\.so\.6|libdl\.so\.2|libpthread\.so\.0|librt\.so\.1|libgcc_s\.so\.1|libwayland-client\.so\.0|libxkbcommon\.so\.0)$'
# Libraries that may appear in the `ldd` closure only as dependencies of the
# allowlisted host libraries (libwayland-client links libffi). They are never
# allowed as direct DT_NEEDED entries of our binary.
TRANSITIVE_RE='^(libffi\.so\.[0-9]+)$'

# Files that must never be inside the AppDir (matched against the basename).
FORBIDDEN_RE='^(libc\.so.*|libc-[0-9.]+\.so|ld-linux.*\.so.*|libm\.so.*|libdl\.so.*|libpthread\.so.*|librt\.so.*|libresolv\.so.*|libutil\.so.*|libnss_.*\.so.*|libGL\.so.*|libGLX.*\.so.*|libGLdispatch\.so.*|libGLESv[0-9].*\.so.*|libOpenGL\.so.*|libEGL.*\.so.*|libvulkan.*\.so.*|libgbm\.so.*|libdrm.*\.so.*|libglapi\.so.*|libgallium.*\.so.*|.*_dri\.so|.*_drv_video\.so|libxatracker\.so.*|libOSMesa\.so.*|libnvidia.*\.so.*|libwayland-.*\.so.*|libxkbcommon.*\.so.*)$'
# Directories / data files that indicate a bundled GPU driver stack.
FORBIDDEN_PATH_RE='(^|/)(dri|gbm|vdpau)(/|$)|(^|/)vulkan/(icd|implicit_layer|explicit_layer)\.d(/|$)|(^|/)glvnd/egl_vendor\.d(/|$)|(^|/)egl/egl_external_platform\.d(/|$)'

die() { echo "audit: $*" >&2; exit 2; }
[ $# -eq 1 ] || die "usage: $0 <AppDir | file.AppImage>"
TARGET="$1"
[ -e "$TARGET" ] || die "no such file or directory: $TARGET"
command -v ldd >/dev/null || die "ldd not found"
command -v readelf >/dev/null || command -v objdump >/dev/null || die "need readelf or objdump (binutils)"

CLEANUP=""
trap '[ -n "$CLEANUP" ] && rm -rf "$CLEANUP"' EXIT

if [ -d "$TARGET" ]; then
    APPDIR="$(cd "$TARGET" && pwd)"
else
    # An AppImage: extract it (works with the static runtime, no FUSE needed).
    img="$(readlink -f "$TARGET")"
    [ -x "$img" ] || die "$TARGET is not executable"
    CLEANUP="$(mktemp -d)"
    (cd "$CLEANUP" && "$img" --appimage-extract >/dev/null) || die "failed to extract $TARGET"
    APPDIR="$CLEANUP/squashfs-root"
fi

FAIL=0
fail() { echo "FAIL: $*"; FAIL=1; }

echo "== Auditing $TARGET"

# --- 1. Forbidden bundled files -------------------------------------------------
while IFS= read -r -d '' f; do
    rel="${f#"$APPDIR"/}"
    base="${f##*/}"
    if [[ "$base" =~ $FORBIDDEN_RE ]]; then
        fail "forbidden library bundled: $rel"
    elif [[ "$rel" =~ $FORBIDDEN_PATH_RE ]]; then
        fail "GPU driver stack file bundled: $rel"
    elif [[ "$base" == *.so || "$base" == *.so.* ]]; then
        echo "note: other shared library bundled (allowed, but unexpected): $rel"
    fi
done < <(find "$APPDIR" -mindepth 1 \( -type f -o -type l -o -type d \) -print0)

# --- 2. Required entries --------------------------------------------------------
[ -x "$APPDIR/AppRun" ] || fail "AppRun missing or not executable"
compgen -G "$APPDIR/*.desktop" >/dev/null || fail "no .desktop file at the AppDir root"
BIN="$APPDIR/$BIN_REL"
if [ ! -x "$BIN" ]; then
    fail "binary missing or not executable: $BIN_REL"
    echo "== Audit FAILED"
    exit 1
fi

# --- 3. Direct dependencies (DT_NEEDED) -----------------------------------------
if command -v readelf >/dev/null; then
    needed="$(readelf -d "$BIN" | sed -n 's/.*(NEEDED).*\[\(.*\)\].*/\1/p')"
else
    needed="$(objdump -p "$BIN" | awk '$1 == "NEEDED" { print $2 }')"
fi
echo "-- DT_NEEDED:"
for lib in $needed; do
    if [[ "$lib" =~ $ALLOW_RE ]]; then
        echo "   ok   $lib"
    else
        echo "   BAD  $lib"
        fail "binary directly links non-allowlisted library: $lib"
    fi
done

# --- 4. ldd closure -------------------------------------------------------------
echo "-- ldd $BIN_REL:"
if ! ldd_out="$(ldd "$BIN" 2>&1)"; then
    if grep -q "not a dynamic executable\|statically linked" <<<"$ldd_out"; then
        echo "   (static executable)"
        ldd_out=""
    else
        echo "$ldd_out"
        fail "ldd failed"
        ldd_out=""
    fi
fi
while IFS= read -r line; do
    [ -n "$line" ] || continue
    name="$(awk '{ print $1 }' <<<"$line")"
    name="${name##*/}"
    if [[ "$line" == *"not found"* ]]; then
        echo "   MISSING $line"
        fail "library not found on this host: $name"
    elif [[ "$name" =~ $ALLOW_RE ]]; then
        echo "   ok   $line"
    elif [[ "$name" =~ $TRANSITIVE_RE ]]; then
        echo "   ok   $line   (transitive, via an allowlisted host library)"
    else
        echo "   BAD  $line"
        fail "ldd closure contains non-allowlisted library: $name"
    fi
done <<<"$ldd_out"

# --- 5. glibc symbol-version baseline -------------------------------------------
if command -v objdump >/dev/null; then
    max_glibc="$(objdump -T "$BIN" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n1 || true)"
    if [ -n "$max_glibc" ]; then
        newest="$(printf '%s\n%s\n' "$max_glibc" "$GLIBC_BASELINE" | sort -V | tail -n1)"
        if [ "$newest" != "$GLIBC_BASELINE" ]; then
            fail "binary requires GLIBC_$max_glibc, newer than the $GLIBC_BASELINE baseline"
        else
            echo "-- newest glibc symbol version: GLIBC_$max_glibc (baseline $GLIBC_BASELINE)"
        fi
    fi
fi

if [ "$FAIL" -ne 0 ]; then
    echo "== Audit FAILED"
    exit 1
fi
echo "== Audit passed"
