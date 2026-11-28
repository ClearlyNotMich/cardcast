#!/bin/sh
# The window guard (#47) and pointer guard (#49) on kwin_wayland --virtual, run by CI in an Arch
# container, which has KWin 6 like the sender. KWin composites only with a render device; without
# one (the CI runner has no /dev/dri and no udmabuf module) windows still move, but a native Wayland
# window never redraws, so it can't be maximised. Given /dev/udmabuf (docker run --device /dev/udmabuf) KWin composites
# with llvmpipe and the tests maximise the Wayland window too.
set -e
cd "$(dirname "$0")/../.."
# libpipewire and clang build the pipewire crate (#42).
pacman -Syu --noconfirm --needed base-devel rust clang pipewire kwin xorg-xwayland foot ttf-dejavu
# The container lacks CAP_SYS_NICE, so kwin_wayland's file capability would stop it starting.
setcap -r /usr/bin/kwin_wayland
cargo test --no-run
export XDG_RUNTIME_DIR="$(mktemp -d)" XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland
[ -e /dev/udmabuf ] && export CARDCAST_KWIN_COMPOSITES=1
failed=0

# Runs `cargo test <arguments>` inside KWin, so the guard's tests get its Wayland and Xwayland displays.
in_kwin() {
  session="$XDG_RUNTIME_DIR/session-$1"
  shift
  printf '#!/bin/sh\nCARDCAST_GUARD_DESKTOP=kwin cargo test %s > %s.log 2>&1\necho $? > %s.status\n' \
    "$*" "$session" "$session" > "$session.sh"
  chmod +x "$session.sh"
  dbus-run-session -- kwin_wayland --virtual --xwayland --output-count 2 --width 1280 --height 1024 \
    --exit-with-session "$session.sh" > /dev/null 2>&1 || true
  cat "$session.log"
  [ "$(cat "$session.status" 2>/dev/null)" = 0 ] || failed=1
}

in_kwin guard guard_tests -- --nocapture --test-threads=1

# With KWin's "interactive window move" off (#61) the guard still returns windows and still leaves
# cardcast's overlays on the card display.
export XDG_CONFIG_HOME="$XDG_RUNTIME_DIR/move-off"
mkdir -p "$XDG_CONFIG_HOME"
printf '[Windows]\nInteractiveWindowMoveEnabled=false\n' > "$XDG_CONFIG_HOME/kwinrc"
in_kwin move-off -- --nocapture --test-threads=1 desktop_window_guard_returns desktop_kwin_guard_returns \
  desktop_kwin_guard_leaves
unset XDG_CONFIG_HOME

# KWin restarts under a running guard: the test is not KWin's child, so it outlives the kill. The
# Wayland socket keeps its name, as a desktop's does, so the pointer guard finds the new KWin too.
start='kwin_wayland --virtual --socket wayland-cardcast --output-count 2 --width 1280 --height 1024 > /dev/null 2>&1'
export CARDCAST_KWIN_START="$start" WAYLAND_DISPLAY=wayland-cardcast
dbus-run-session -- sh -c "
  $start &
  for _ in \$(seq 100); do busctl --user status org.kde.KWin > /dev/null 2>&1 && break; sleep 0.1; done
  cargo test desktop_kwin_guard_follows -- --nocapture || echo 1 > '$XDG_RUNTIME_DIR/restart'
  pkill -x kwin_wayland || true
"
[ -e "$XDG_RUNTIME_DIR/restart" ] && failed=1
exit "$failed"
