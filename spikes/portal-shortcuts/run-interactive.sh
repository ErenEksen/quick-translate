#!/usr/bin/env bash
# Interactive part of spike 2.4 (GlobalShortcuts portal) on KDE Plasma.
# Needs a human: KDE shows a confirmation dialog on first bind, and the keys must be pressed.
# Every phase is logged to spikes/portal-shortcuts/logs/<phase>.log.
#
#   ./run-interactive.sh            # all phases
#   ./run-interactive.sh B D        # only some phases (P, A..F; G is optional and never default)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/agent-portal-spike}"
BIN="$CARGO_TARGET_DIR/debug/portal-shortcuts-spike"
LOGS="$HERE/logs"
SOCK="${XDG_RUNTIME_DIR:?}/qt-portal-spike.sock"
mkdir -p "$LOGS"

PHASES=("$@")
[ ${#PHASES[@]} -eq 0 ] && PHASES=(P A B C D E F)
want() { local p; for p in "${PHASES[@]}"; do [ "$p" = "$1" ] && return 0; done; return 1; }

say()   { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
note()  { printf '\033[0;33m%s\033[0m\n' "$*"; }
pause() { read -rp $'\033[1mPress Enter to continue (Ctrl+C aborts)... \033[0m' _; }
run()   { local log="$LOGS/$1.log"; shift; echo "\$ $BIN $*" | tee "$log"; "$BIN" "$@" 2>&1 | tee -a "$log"; }

say "Build"
(cd "$HERE" && cargo build -j 8 --quiet)
{ echo "date: $(date -Is)"; uname -r; pacman -Q xdg-desktop-portal xdg-desktop-portal-kde kwin 2>/dev/null || true; } | tee "$LOGS/versions.log"

if want P; then
  say "Phase P: non-interactive probe (no dialog expected)"
  run P-probe --probe
  note "Expect: Register OK, GlobalShortcuts version = 2, CreateSession OK, ListShortcuts 0 shortcuts (first ever run)."
fi

if want A; then
  say "Phase A: first bind (dialog) + activation + latency"
  note "1. A KDE dialog titled like 'Quick Translate (portal spike) wants to register global shortcuts'"
  note "   appears, proposing Ctrl+Alt+C for 'Translate selection (Quick Translate spike)'. Accept it (OK/Allow)."
  note "2. Then press Ctrl+Alt+C about 10 times, ~1 s apart. Release the keys fully each time."
  note "Expect per press: KGLOBALACCEL pressed component=io.github...Spike, then ACTIVATED #n with"
  note "  activation_token (KDE 6.8 source suggests ABSENT), the portal ts vs clocks line, and"
  note "  'kglobalaccel signal arrival -> portal Activated arrival' (the portal hop)."
  note "The listener stops after 45 s."
  pause
  run A-first-bind --bind --listen 45
fi

if want B; then
  say "Phase B: re-bind after restart, unchanged trigger"
  note "A NEW process calls BindShortcuts again (as the daemon does on every start)."
  note "Expect: 'persisted-before-bind' lists translate-selection with Ctrl+Alt+C, NO dialog,"
  note "BindShortcuts returns in well under 1 s, and Ctrl+Alt+C still fires. Press it 3 times. 20 s."
  pause
  run B-rebind --bind --listen 20
fi

if want C; then
  say "Phase C: user changes the trigger (ConfigureShortcuts, portal v2)"
  note "System Settings should open on the spike's shortcut (ConfigureShortcuts)."
  note "If it does not, open System Settings > Keyboard > Shortcuts > 'Quick Translate (portal spike)'."
  note "Change 'Translate selection' to Meta+Alt+T (free on this machine; Meta+T belongs to KWin) and Apply."
  note "Expect SHORTCUTS_CHANGED with the new trigger. Then press Meta+Alt+T twice. 90 s."
  pause
  run C-configure --bind --configure --listen 90
fi

if want D; then
  say "Phase D: restart after the user rebind (spec: 'Binding survives restart')"
  note "Expect: persisted-before-bind AND bound/list-after-bind show Meta+Alt+T (NOT reset to Ctrl+Alt+C),"
  note "no dialog. Press Meta+Alt+T twice (fires), then Ctrl+Alt+C once (must NOT fire). 25 s."
  pause
  run D-restart-after-rebind --bind --listen 25
fi

if want E; then
  say "Phase E: portal vs native KDE custom shortcut latency"
  note "One-time setup (manual): System Settings > Keyboard > Shortcuts > Add New > Command or Script..."
  note "  Command: $BIN --stamp"
  note "  Shortcut: Ctrl+Alt+V   (free on this machine)"
  note "Optional ground truth for key-press time: in a SECOND terminal run (needs root, reads only"
  note "timestamps of non-modifier keys pressed while Ctrl/Alt/Meta is held):"
  for k in /dev/input/by-id/*-event-kbd; do [ -e "$k" ] && note "  sudo $BIN --evdev-log --sock $SOCK $(readlink -f "$k")"; done
  note "Then, during the 90 s window, alternate: Ctrl+Alt+V (native) and the portal shortcut"
  note "(Meta+Alt+T after phase C, else Ctrl+Alt+C), 10 times each, ~1 s apart."
  note "The summary at the end prints min/median/max for each path."
  pause
  run E-latency --bind --listen 90
fi

if want G; then
  # Optional, not in the default list: ./run-interactive.sh G   (run before F)
  GP="$CARGO_TARGET_DIR/debug/gnome-primary-spike"
  (cd "$ROOT/spikes/gnome-primary" && cargo build -j 8 --quiet)
  say "Phase G (optional): spike 2.5 path on KDE: portal trigger -> token -> winit window -> PRIMARY"
  note "G1: select some text in another app (e.g. a word in Kate), then click the window that opens"
  note "    within 10 s. Expect 'Focused(true)' and 'PRIMARY OK ... chars' (selection read at focus time)."
  pause
  "$GP" --timeout 10 --redact 2>&1 | tee "$LOGS/G1-click-focus.log" || true
  note "G2: portal trigger re-launches gnome-primary-spike with XDG_ACTIVATION_TOKEN from Activated."
  note "    Select text elsewhere, press the portal shortcut 3 times. On KDE the token is expected ABSENT,"
  note "    so expect TIMEOUT (no focus, exit 2) unless KWin focuses it anyway; record which."
  pause
  run G2-portal-exec --bind --listen 30 --exec "$GP --timeout 3 --redact"
fi

if want F; then
  say "Phase F: cleanup"
  note "Manual: remove the native custom shortcut from phase E in System Settings."
  pause
  run F-cleanup --cleanup
  note "Check System Settings > Keyboard > Shortcuts: no 'Quick Translate (portal spike)' entry should remain."
fi

say "Done. Logs in $LOGS — paste the relevant parts into docs/spikes/portal-shortcuts.md"
