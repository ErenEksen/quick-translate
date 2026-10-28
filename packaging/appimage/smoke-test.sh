#!/usr/bin/env bash
# Smoke test for the quick-translate AppImage (tasks.md 9.4, design D11). Shared by the
# release workflow (.github/workflows/release.yml) and local dry runs.
#
#   packaging/appimage/smoke-test.sh [options] <file.AppImage> [expected-version]
#
# Checks, all without a Wayland session:
#   1. `--version` with APPIMAGE_EXTRACT_AND_RUN=1 exits 0 and prints
#      "quick-translate <version>"; <version> must equal expected-version when given
#      (a leading "v" is ignored, so a tag name works).
#   2. `--help` exits 0 and an unknown argument exits 2.
#   3. Wayland gate (distribution spec, "Unsupported session refused"): in an X11-like
#      environment (WAYLAND_DISPLAY and WAYLAND_SOCKET unset, DISPLAY=:0,
#      XDG_SESSION_TYPE=x11) starting the app (no arguments), `--daemon`, `--toggle`
#      and `--status` exit 1 with "requires a Wayland session" on stderr; `--daemon`
#      with WAYLAND_DISPLAY naming a missing socket is refused the same way.
#   4. When /dev/fuse and fusermount(3) exist: `--version` and the no-argument gate
#      check again through the normal FUSE mount (skipped in containers).
#   5. The embedded update information, and that a .zsync next to the AppImage
#      describes this exact file (Filename, Length, SHA-1).
#   6. The bundle audit (audit.sh). It needs ldd and readelf or objdump (binutils).
#
# Options:
#   --audit          Fail if the audit tools are missing (default: skip the audit with a note).
#   --no-audit       Skip the audit.
#   --require-zsync  Fail if the AppImage embeds update information but has no .zsync.
#   -h, --help       Show this help.
#
# Every run of the AppImage uses `env -i` with a throwaway HOME, XDG_RUNTIME_DIR and
# TMPDIR, so the caller's Wayland session, D-Bus session, daemon and config are never
# reached; it is safe on a desktop. Needs only bash, coreutils and grep: the AppImage
# uses the static type2 runtime, so extract-and-run needs no libfuse.
#
# Exit codes: 0 = all checks passed, 1 = a check failed, 2 = usage error.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE_MSG="requires a Wayland session"

usage() { sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }

AUDIT=auto
REQUIRE_ZSYNC=0
POSITIONAL=()
while [ $# -gt 0 ]; do
    case "$1" in
        --audit) AUDIT=require ;;
        --no-audit) AUDIT=skip ;;
        --require-zsync) REQUIRE_ZSYNC=1 ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "smoke-test: unknown option: $1" >&2; usage >&2; exit 2 ;;
        *) POSITIONAL+=("$1") ;;
    esac
    shift
done
[ "${#POSITIONAL[@]}" -ge 1 ] && [ "${#POSITIONAL[@]}" -le 2 ] || { usage >&2; exit 2; }
SRC="${POSITIONAL[0]}"
EXPECTED="${POSITIONAL[1]:-}"
EXPECTED="${EXPECTED#v}"
[ -f "$SRC" ] || { echo "smoke-test: no such file: $SRC" >&2; exit 2; }
SRC="$(readlink -f "$SRC")"

W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
mkdir -p "$W/home" "$W/run" "$W/tmp"
chmod 700 "$W/run"

# A read-only mount or a downloaded artifact may have lost the executable bit.
IMG="$SRC"
if [ ! -x "$IMG" ]; then
    cp "$SRC" "$W/app.AppImage"
    chmod +x "$W/app.AppImage"
    IMG="$W/app.AppImage"
fi

PASS=0 FAIL=0 SKIP=0
ok()   { echo "PASS: $*"; PASS=$((PASS + 1)); }
bad()  { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }
skip() { echo "SKIP: $*"; SKIP=$((SKIP + 1)); }

TIMEOUT=()
command -v timeout >/dev/null && TIMEOUT=(timeout 60)

# run [VAR=value ...] -- [args ...]: run the AppImage in a clean environment.
# Sets RC, OUT and ERR. APPIMAGE_EXTRACT_AND_RUN=1 unless QT_SMOKE_FUSE=1 is passed.
run() {
    local envs=() fuse=0
    while [ "$1" != "--" ]; do
        if [ "$1" = "QT_SMOKE_FUSE=1" ]; then fuse=1; else envs+=("$1"); fi
        shift
    done
    shift
    [ "$fuse" -eq 1 ] || envs+=(APPIMAGE_EXTRACT_AND_RUN=1)
    set +e
    env -i PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
        HOME="$W/home" XDG_RUNTIME_DIR="$W/run" TMPDIR="$W/tmp" LANG=C.UTF-8 \
        "${envs[@]}" "${TIMEOUT[@]}" "$IMG" "$@" >"$W/out" 2>"$W/err" </dev/null
    RC=$?
    set -e
    OUT="$(cat "$W/out")"
    ERR="$(cat "$W/err")"
}

show() { # describe the last run
    echo "      exit=$RC"
    [ -z "$OUT" ] || sed 's/^/      stdout: /' "$W/out"
    [ -z "$ERR" ] || sed 's/^/      stderr: /' "$W/err"
}

check_version() { # label [run env...]
    local label="$1"; shift
    run "$@" -- --version
    local got="${OUT#quick-translate }"
    if [ "$RC" -ne 0 ] || [[ "$OUT" != "quick-translate "* ]] || [[ "$got" == *$'\n'* ]]; then
        bad "$label: --version"; show; return
    fi
    if [ -n "$EXPECTED" ] && [ "$got" != "$EXPECTED" ]; then
        bad "$label: --version prints $got, expected $EXPECTED"; show; return
    fi
    ok "$label: --version -> '$OUT' (exit 0)"
}

check_refused() { # label [run env...] -- args...
    local label="$1"; shift
    run "$@"
    if [ "$RC" -eq 1 ] && grep -qF "$GATE_MSG" <<<"$ERR" && [ -z "$OUT" ]; then
        ok "$label -> exit $RC, stderr: $ERR"
    else
        bad "$label: expected exit 1 and '$GATE_MSG' on stderr"; show
    fi
}

X11_ENV=(DISPLAY=:0 XDG_SESSION_TYPE=x11)

echo "== Smoke-testing $SRC"
echo "   host: $(sed -n 's/^PRETTY_NAME=//p' /etc/os-release 2>/dev/null | tr -d '"'), glibc $(ldd --version 2>/dev/null | sed -n '1s/.* //p')"
echo "   size: $(stat -c %s "$SRC") bytes, sha256 $(sha256sum "$SRC" | cut -d' ' -f1)"
run QT_SMOKE_FUSE=1 -- --appimage-version
echo "   runtime: ${ERR:-$OUT}"

# --- 1-2. Version and argument handling (extract-and-run) -----------------------
check_version "extract-and-run"

run -- --help
if [ "$RC" -eq 0 ] && grep -q '^Usage: quick-translate' <<<"$OUT"; then
    ok "extract-and-run: --help (exit 0)"
else
    bad "extract-and-run: --help"; show
fi

run -- --no-such-flag
if [ "$RC" -eq 2 ] && grep -qF "unknown argument '--no-such-flag'" <<<"$ERR"; then
    ok "extract-and-run: unknown argument -> exit 2"
else
    bad "extract-and-run: unknown argument should exit 2"; show
fi

# --- 3. Wayland gate ------------------------------------------------------------
check_refused "no Wayland (X11 env): quick-translate"           "${X11_ENV[@]}" --
check_refused "no Wayland (X11 env): quick-translate --daemon"  "${X11_ENV[@]}" -- --daemon
check_refused "no Wayland (X11 env): quick-translate --toggle"  "${X11_ENV[@]}" -- --toggle
check_refused "no Wayland (X11 env): quick-translate --status"  "${X11_ENV[@]}" -- --status
check_refused "no Wayland (bare env): quick-translate"          --
check_refused "missing socket: quick-translate --daemon" \
    WAYLAND_DISPLAY=wayland-smoke-missing -- --daemon

# --- 4. FUSE mount --------------------------------------------------------------
if [ -e /dev/fuse ] && { command -v fusermount3 >/dev/null || command -v fusermount >/dev/null; }; then
    check_version "FUSE mount" QT_SMOKE_FUSE=1
    check_refused "FUSE mount, no Wayland (X11 env): quick-translate" \
        QT_SMOKE_FUSE=1 "${X11_ENV[@]}" --
else
    skip "FUSE mount: no /dev/fuse or fusermount here (expected in containers)"
fi

# --- 5. Update information and .zsync -------------------------------------------
run QT_SMOKE_FUSE=1 -- --appimage-updateinformation
UPDATE_INFO="$OUT"
echo "   update information: ${UPDATE_INFO:-(none)}"
ZSYNC="$SRC.zsync"
if [ -f "$ZSYNC" ]; then
    header="$(head -c 4096 "$ZSYNC" | tr -d '\r' | sed -n '1,/^$/p' 2>/dev/null || true)"
    field() { sed -n "s/^$1: //p" <<<"$header" | head -n1; }
    z_name="$(field Filename)" z_len="$(field Length)" z_sha1="$(field SHA-1)"
    a_name="$(basename "$SRC")" a_len="$(stat -c %s "$SRC")" a_sha1="$(sha1sum "$SRC" | cut -d' ' -f1)"
    if [ "$z_name" = "$a_name" ] && [ "$z_len" = "$a_len" ] && [ "$z_sha1" = "$a_sha1" ]; then
        ok ".zsync matches the AppImage (Filename $z_name, Length $z_len, SHA-1 $z_sha1)"
    else
        bad ".zsync does not describe this AppImage"
        echo "      zsync:    Filename=$z_name Length=$z_len SHA-1=$z_sha1"
        echo "      AppImage: Filename=$a_name Length=$a_len SHA-1=$a_sha1"
    fi
    if [ -n "$UPDATE_INFO" ] && [[ "$UPDATE_INFO" == *"|"* ]]; then
        pattern="${UPDATE_INFO##*|}"
        # shellcheck disable=SC2053 # glob match against the update-info file pattern
        if [[ "$(basename "$ZSYNC")" == $pattern ]]; then
            ok ".zsync name matches the update-information pattern $pattern"
        else
            bad ".zsync name $(basename "$ZSYNC") does not match the update-information pattern $pattern"
        fi
    fi
elif [ -n "$UPDATE_INFO" ] && [ "$REQUIRE_ZSYNC" -eq 1 ]; then
    bad "update information is embedded but $ZSYNC is missing"
else
    skip ".zsync: none next to the AppImage"
fi

# --- 6. Bundle audit ------------------------------------------------------------
have_audit_tools() { command -v ldd >/dev/null && { command -v readelf >/dev/null || command -v objdump >/dev/null; }; }
if [ "$AUDIT" = skip ]; then
    skip "bundle audit (--no-audit)"
elif have_audit_tools; then
    set +e
    (cd "$W/tmp" && TMPDIR="$W/tmp" bash "$SCRIPT_DIR/audit.sh" "$IMG") >"$W/audit" 2>&1
    rc=$?
    set -e
    sed 's/^/      /' "$W/audit"
    if [ "$rc" -eq 0 ]; then ok "bundle audit"; else bad "bundle audit (exit $rc)"; fi
elif [ "$AUDIT" = require ]; then
    bad "bundle audit: ldd and readelf/objdump are required (install binutils)"
else
    skip "bundle audit: binutils (readelf/objdump) not installed"
fi

echo "== $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
