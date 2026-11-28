#!/bin/sh
# The window guard on kwin_wayland --virtual (#47), run by CI in an Arch container, which has
# KWin 6 like the sender. KWin composites only with a render device; without one (the CI runner has
# no /dev/dri and no udmabuf module) windows still move, but a native Wayland window never redraws,
# so it can't be maximised. Given /dev/udmabuf (docker run --device /dev/udmabuf) KWin composites
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
log="$XDG_RUNTIME_DIR/test.log"

# The guard's tests, run inside KWin so they get its Wayland and Xwayland displays.
printf '#!/bin/sh\nCARDCAST_GUARD_DESKTOP=kwin cargo test guard_tests -- --nocapture --test-threads=1 > %s 2>&1\necho $? > %s\n' \
  "$log" "$XDG_RUNTIME_DIR/status" > "$XDG_RUNTIME_DIR/session.sh"
chmod +x "$XDG_RUNTIME_DIR/session.sh"
dbus-run-session -- kwin_wayland --virtual --xwayland --output-count 2 --width 1280 --height 1024 \
  --exit-with-session "$XDG_RUNTIME_DIR/session.sh" > /dev/null 2>&1 || true
cat "$log"
failed=$(cat "$XDG_RUNTIME_DIR/status" 2>/dev/null || echo 1)

# KWin restarts under a running guard: the test is not KWin's child, so it outlives the kill.
start='kwin_wayland --virtual --output-count 2 --width 1280 --height 1024 > /dev/null 2>&1'
export CARDCAST_KWIN_START="$start"
dbus-run-session -- sh -c "
  $start &
  for _ in \$(seq 100); do busctl --user status org.kde.KWin > /dev/null 2>&1 && break; sleep 0.1; done
  cargo test desktop_kwin_guard_follows -- --nocapture || echo 1 > '$XDG_RUNTIME_DIR/restart'
  pkill -x kwin_wayland || true
"
[ -e "$XDG_RUNTIME_DIR/restart" ] && failed=1
exit "$failed"
