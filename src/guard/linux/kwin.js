// cardcast's window guard on Plasma: src/guard/linux/kwin.rs loads this KWin script over D-Bus.
// KWin tells the script about every window; cardcast decides where each goes (placement.rs) and
// the script moves it. Once cardcast's D-Bus connection is gone (it quit or crashed) the script
// unloads itself, so it never outlives cardcast.
const service = "@SERVICE@";
const plugin = "@PLUGIN@";
const guard = "@INTERFACE@";
const path = "/guard";
let stopped = false;
let sweep = -1;
const pending = new Set();
// Windows unmaximised to see where they restore to, to be maximised again (Guarding::check).
const remaximising = new Set();

// Checks windows once they have been still for a moment, so a burst of changes is checked once.
const settle = new QTimer();
settle.singleShot = true;
settle.interval = 50;
settle.timeout.connect(() => {
    const windows = Array.from(pending);
    pending.clear();
    windows.forEach(check);
});

function queue(window) {
    if (stopped) return;
    pending.add(window);
    settle.start();
}

function id(window) {
    return window.internalId.toString();
}

function check(window) {
    if (stopped || window.deleted) return;
    // A window being dragged or resized is checked when that ends (moveResizedChanged).
    if (window.move || window.resize) return;
    // Not unmaximised yet: its restored frame comes with a later change.
    if (remaximising.has(window) && window.maximizeMode === 3) return;
    const g = window.frameGeometry;
    const state = [g.x, g.y, g.x + g.width, g.y + g.height]
        .map(Math.round)
        .concat([window.maximizeMode === 3, window.minimized,
            !(window.normalWindow || window.dialog), remaximising.has(window)].map(Number))
        .join(" ");
    callDBus(service, path, guard, "Check", id(window), window.caption, state,
        reply => apply(window, reply));
}

// The reply is "stay", "move <left> <top> <right> <bottom>", "unmaximise" or "maximise <restore>".
function apply(window, reply) {
    if (stopped || window.deleted) return;
    const [action, left, top, right, bottom] = reply.split(" ").map((word, i) => i ? Number(word) : word);
    const rect = { x: left, y: top, width: right - left, height: bottom - top };
    const remaximise = remaximising.delete(window);
    if (action === "move") {
    } else if (action === "unmaximise") {
        remaximising.add(window);
        window.setMaximize(false, false);
    } else if (action === "maximise") {
        window.frameGeometry = rect;
        window.setMaximize(true, true);
    } else if (remaximise) {
        window.setMaximize(true, true);
    }
}

function watch(window) {
    for (const changed of [window.frameGeometryChanged, window.maximizedChanged,
        window.minimizedChanged, window.moveResizedChanged]) {
        changed.connect(() => queue(window));
    }
    queue(window);
}

// Each output's work area, "<name> <left> <top> <right> <bottom>" joined by ";".
function workAreas() {
    return workspace.screens.map(output => {
        const a = workspace.clientArea(KWin.MaximizeArea, output, workspace.currentDesktop);
        return [output.name].concat([a.x, a.y, a.x + a.width, a.y + a.height].map(Math.round)).join(" ");
    }).join(";");
}

// Every second: unload if cardcast is gone, else send the work areas and check every window when
// cardcast asks (its sweep number changed: the guard started or the displays changed).
function poll() {
    callDBus("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
        "NameHasOwner", service, alive => {
            if (stopped) return;
            if (!alive) return stop();
            callDBus(service, path, guard, "Poll", workAreas(), next => {
                if (stopped || next === sweep) return;
                sweep = next;
                workspace.windowList().forEach(queue);
            });
        });
}

function stop() {
    stopped = true;
    timer.stop();
    settle.stop();
    callDBus("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "unloadScript", plugin);
}

workspace.windowAdded.connect(watch);
workspace.windowRemoved.connect(window => {
    pending.delete(window);
    remaximising.delete(window);
    if (!stopped) callDBus(service, path, guard, "Forget", id(window));
});
workspace.windowList().forEach(window => watch(window));
const timer = new QTimer();
timer.interval = 1000;
timer.timeout.connect(poll);
timer.start();
poll();
