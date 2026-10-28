#!/usr/bin/env bash
# Self-test for audit.sh (tasks.md 9.2): a clean AppDir must pass, and every
# deliberately polluted AppDir must fail.
#
#   packaging/appimage/test-audit.sh [path/to.AppImage ...]
#
# Any AppImages given as arguments are audited too and must pass.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AUDIT="$SCRIPT_DIR/audit.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A host binary whose ldd closure is glibc-only stands in for quick-translate.
CLEAN_BIN=""
for c in /usr/bin/true /bin/true /usr/bin/sleep /bin/sleep; do
    [ -x "$c" ] || continue
    if ! ldd "$c" 2>/dev/null | awk '{print $1}' | sed 's|.*/||' \
        | grep -Evq '^(linux-vdso\.so\.1|ld-linux-x86-64\.so\.2|libc\.so\.6)$'; then
        CLEAN_BIN="$c"; break
    fi
done
[ -n "$CLEAN_BIN" ] || { echo "test-audit: no glibc-only host binary found" >&2; exit 2; }

# A host binary that links something outside the allowlist (e.g. libz, libssl).
DIRTY_BIN=""
for c in /usr/bin/curl /usr/bin/ssh /usr/bin/git /usr/bin/python3 /usr/bin/gpg /usr/bin/ls /usr/bin/tar; do
    [ -x "$c" ] || continue
    if readelf -d "$c" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\].*/\1/p' \
        | grep -Evq '^(ld-linux-x86-64\.so\.2|libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libwayland-client\.so\.0|libxkbcommon\.so\.0)$'; then
        DIRTY_BIN="$c"; break
    fi
done

make_appdir() { # dir [binary]
    local d="$1" bin="${2:-$CLEAN_BIN}"
    mkdir -p "$d/usr/bin" "$d/usr/share/applications"
    cp "$bin" "$d/usr/bin/quick-translate"
    cp "$SCRIPT_DIR/AppRun" "$d/AppRun"
    cp "$SCRIPT_DIR/io.github.quicktranslate.QuickTranslate.desktop" "$d/usr/share/applications/"
    ln -s usr/share/applications/io.github.quicktranslate.QuickTranslate.desktop \
        "$d/io.github.quicktranslate.QuickTranslate.desktop"
}

PASS=0 FAILED=0
# The synthetic AppDirs use host binaries, which may be built against a newer
# glibc than the 2.35 baseline, so the baseline check is relaxed for them.
SYNTHETIC_BASELINE=999
expect() { # pass|fail name target [glibc-baseline]
    local want="$1" name="$2" target="$3" baseline="${4:-$SYNTHETIC_BASELINE}" rc=0
    QT_GLIBC_BASELINE="$baseline" bash "$AUDIT" "$target" >"$WORK/$name.log" 2>&1 || rc=$?
    if { [ "$want" = pass ] && [ "$rc" -eq 0 ]; } || { [ "$want" = fail ] && [ "$rc" -eq 1 ]; }; then
        echo "ok    $name (expected $want, exit $rc)"
        PASS=$((PASS + 1))
    else
        echo "WRONG $name (expected $want, exit $rc)"
        sed 's/^/      /' "$WORK/$name.log"
        FAILED=$((FAILED + 1))
    fi
}

# 1. Clean AppDir.
make_appdir "$WORK/clean"
expect pass clean "$WORK/clean"

# 2. Polluted AppDirs: one forbidden item each.
pollute() { # name relative-path
    local d="$WORK/$1"
    make_appdir "$d"
    mkdir -p "$(dirname "$d/$2")"
    printf '\x7fELF-fake' >"$d/$2"
    expect fail "$1" "$d"
}
pollute glibc             usr/lib/libc.so.6
pollute ld-linux          usr/lib64/ld-linux-x86-64.so.2
pollute libGL             usr/lib/x86_64-linux-gnu/libGL.so.1
pollute libEGL            usr/lib/libEGL.so.1.1.0
pollute libvulkan         usr/lib/libvulkan.so.1
pollute libgbm            usr/lib/libgbm.so.1
pollute libdrm            usr/lib/libdrm_amdgpu.so.1
pollute mesa-dri          usr/lib/x86_64-linux-gnu/dri/radeonsi_dri.so
pollute mesa-glapi        usr/lib/libglapi.so.0
pollute mesa-gallium      usr/lib/libgallium-25.0.so
pollute vulkan-icd        usr/share/vulkan/icd.d/radeon_icd.x86_64.json
pollute libwayland        usr/lib/libwayland-client.so.0
pollute libxkbcommon      usr/lib/libxkbcommon.so.0

# 3. A binary whose dependencies are outside the allowlist.
if [ -n "$DIRTY_BIN" ]; then
    make_appdir "$WORK/bad-ldd" "$DIRTY_BIN"
    expect fail "bad-ldd($(basename "$DIRTY_BIN"))" "$WORK/bad-ldd"
else
    echo "skip  bad-ldd (no host binary with non-allowlisted dependencies found)"
fi

# 4. A binary needing glibc symbols newer than the baseline (any real binary
#    needs at least GLIBC_2.2.5, so a 2.0 baseline must fail).
expect fail glibc-baseline "$WORK/clean" 2.0

# 5. Missing binary.
make_appdir "$WORK/no-binary"
rm "$WORK/no-binary/usr/bin/quick-translate"
expect fail no-binary "$WORK/no-binary"

# 6. Real AppImages given on the command line must pass.
for img in "$@"; do
    expect pass "appimage:$(basename "$img")" "$img" 2.35
done

echo "== $PASS ok, $FAILED wrong"
[ "$FAILED" -eq 0 ]
