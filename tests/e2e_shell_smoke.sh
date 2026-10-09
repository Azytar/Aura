#!/usr/bin/env bash
# End-to-end milestone check for AURA Terminal.
#
# Drives the real `aura` binary on a live X11 display and asserts that all of
# the following work together:
#
#   keyboard event -> X11 -> window -> PTY write -> real shell -> PTY read
#     -> VT/OSC parser -> window title     (round trip through /bin/bash)
#   shell output -> parser -> Vulkan pixels (the framebuffer is not blank)
#   closing the window leaves no orphaned child process behind
#
# Every command is followed by `sleep 3`: the system PS1 embeds its own OSC 0
# title escape, so bash rewrites the title at the *next* prompt. Keeping a
# command running is what makes the title observable at all.
#
# The script exits nonzero on the first failed assertion and prints a reason.
# It skips (exit 77) when no usable X11 display is available, so it is safe to
# run in CI containers.

set -u

BIN="${1:-target/release/aura}"
export DISPLAY="${DISPLAY:-:0}"

skip() { printf 'SKIP: %s\n' "$1"; exit 77; }
fail() { printf 'FAIL: %s\n' "$1"; exit 1; }

command -v xdotool >/dev/null || skip "xdotool is not installed"
command -v xwd >/dev/null || skip "xwd is not installed"
[ -x "$BIN" ] || skip "binary $BIN is not built (cargo build --release)"
timeout 5 xdpyinfo >/dev/null 2>&1 || skip "no X11 display on $DISPLAY"

printf '== launching aura on %s\n' "$DISPLAY"
"$BIN" >/tmp/aura-e2e.log 2>&1 &
AURA_PID=$!
cleanup() {
    if [ -n "${AURA_PID:-}" ] && kill -0 "$AURA_PID" 2>/dev/null; then
        kill "$AURA_PID" 2>/dev/null
        wait "$AURA_PID" 2>/dev/null
    fi
}
trap cleanup EXIT

# --- wait for the window, identified via _NET_WM_PID -------------------
WID=""
for _ in $(seq 1 60); do
    WID="$(xdotool search --onlyvisible --pid "$AURA_PID" 2>/dev/null | head -1)"
    [ -n "$WID" ] && break
    sleep 0.1
done
[ -n "$WID" ] || fail "aura did not map a window (log: /tmp/aura-e2e.log)"
printf '   window 0x%x (found through _NET_WM_PID)\n' "$WID"

xdotool windowfocus --sync "$WID" >/dev/null 2>&1
xdotool windowactivate --sync "$WID" >/dev/null 2>&1

CHILD_PID="$(pgrep -P "$AURA_PID" | head -1)"
[ -n "$CHILD_PID" ] || fail "aura did not spawn a child shell process"
printf '   child shell pid %s\n' "$CHILD_PID"

# A live window manager can hand focus elsewhere between steps (another
# window, an OSD, leftovers from an earlier run). Re-assert focus before every
# input so a keystroke never lands in somebody else's window.
focus_window() {
    xdotool windowfocus --sync "$WID" >/dev/null 2>&1 || true
    xdotool windowactivate --sync "$WID" >/dev/null 2>&1 || true
    [ "$(xdotool getwindowfocus 2>/dev/null)" = "$WID" ] || return 1
}

focus_window || fail "could not focus the aura window"

send_line() {
    focus_window || fail "lost focus before typing"
    xdotool type --delay 12 "$1"
    xdotool key --clearmodifiers Return
}

# Polls the window title, which bash rewrites at every prompt unless a command
# is still running.
wait_for_title() {
    local want="$1" i
    for i in $(seq 1 30); do
        [ "$(xdotool getwindowname "$WID" 2>/dev/null)" = "$want" ] && return 0
        sleep 0.2
    done
    return 1
}

# --- 1. keyboard -> PTY -> real bash -> parser -> window title ----------
send_line 'printf "\033]0;%s\a" "AURA_E2E_TITLE"; sleep 3'
wait_for_title "AURA_E2E_TITLE" \
    || fail "window title never became AURA_E2E_TITLE (got '$(xdotool getwindowname "$WID")')"
printf '   keyboard -> pty -> bash -> parser -> title: OK\n'

# --- 2. shell arithmetic proves a real interactive bash ----------------
send_line 'printf "\033]0;%s\a" "$((6 * 7))"; sleep 3'
wait_for_title "42" \
    || fail "shell arithmetic reported '$(xdotool getwindowname "$WID")', expected '42'"
printf '   interactive bash session: OK\n'

# --- 3. shell output reaches the Vulkan framebuffer --------------------
send_line 'clear'
sleep 0.5
send_line 'printf "AURA_RENDERED_OUTPUT_%s\n" "$(id -un)"'
sleep 1.5
SHOT_XWD="/tmp/aura-e2e.xwd"
SHOT_PNG="/tmp/aura-e2e.png"
xwd -id "$WID" -silent >"$SHOT_XWD" 2>/dev/null || fail "could not capture the aura window"
magick "$SHOT_XWD" "$SHOT_PNG" >/dev/null 2>&1 || fail "could not convert the capture"
COLOURS="$(magick identify -format '%k' "$SHOT_PNG" 2>/dev/null)"
PIXELS="$(magick identify -format '%wx%h' "$SHOT_PNG" 2>/dev/null)"
[ -n "$PIXELS" ] || fail "capture has no geometry"
case "$COLOURS" in
    ''|*[!0-9]*) fail "could not count colours in $PIXELS" ;;
esac
[ "$COLOURS" -gt 4 ] 2>/dev/null \
    || fail "framebuffer looks blank (geometry=$PIXELS colours=$COLOURS)"
printf '   framebuffer %s with %s distinct colours: OK\n' "$PIXELS" "$COLOURS"

# --- 4. clean shutdown leaves no orphan -------------------------------
kill -TERM "$AURA_PID" 2>/dev/null
for _ in $(seq 1 60); do
    kill -0 "$AURA_PID" 2>/dev/null || break
    sleep 0.1
done
kill -0 "$AURA_PID" 2>/dev/null && fail "aura did not exit after SIGTERM"
sleep 0.5
kill -0 "$CHILD_PID" 2>/dev/null && fail "child shell $CHILD_PID survived aura"
AURA_PID=""
printf '   no orphaned child process: OK\n'

printf '== PASS: keyboard, real shell, parser round trip, rendering, clean exit\n'
exit 0
