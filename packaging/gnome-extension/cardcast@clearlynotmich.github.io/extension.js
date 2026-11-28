// cardcast's GNOME Shell extension (docs/research/linux-support.md, sections 2 and 5). On GNOME only
// the Shell can move other programs' windows, so this lists and moves them for cardcast's window
// guard, which decides where each goes. It also keeps cardcast's mirror, a fullscreen window on the
// card display, from taking the focus: whenever the mirror gets it, it goes straight back.
//
// cardcast talks to it on the session bus, as io.github.ClearlyNotMich.cardcast.Shell.

import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const NAME = 'io.github.ClearlyNotMich.cardcast.Shell';
const PATH = '/io/github/ClearlyNotMich/cardcast/Shell';
// The mirror's Wayland app id.
const MIRROR = 'io.github.ClearlyNotMich.cardcast.Mirror';

const INTERFACE = `<node><interface name="${NAME}">
  <method name="Windows"><arg type="a(sssb)" direction="out"/></method>
  <method name="WorkArea"><arg type="s" direction="in"/><arg type="(iiii)" direction="out"/></method>
  <method name="Move">
    <arg type="s" direction="in"/>
    <arg type="i" direction="in"/><arg type="i" direction="in"/>
    <arg type="i" direction="in"/><arg type="i" direction="in"/>
  </method>
  <method name="SendToMonitor"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
  <method name="Focus"><arg type="s" direction="out"/></method>
  <signal name="Changed"/>
</interface></node>`;

// The window types the guard looks at; the others are panels, menus, tooltips and the like.
const GUARDED = [
    Meta.WindowType.NORMAL,
    Meta.WindowType.DIALOG,
    Meta.WindowType.MODAL_DIALOG,
    Meta.WindowType.UTILITY,
    Meta.WindowType.TOOLBAR,
    Meta.WindowType.SPLASHSCREEN,
];

const isMirror = window => window.get_wm_class() === MIRROR;

// GNOME 49 replaced get_maximized with is_maximized.
const isMaximised = window => window.is_maximized
    ? window.is_maximized()
    : window.get_maximized() === Meta.MaximizeFlags.BOTH;

const monitorIndex = connector => {
    const index = global.backend.get_monitor_manager().get_monitor_for_connector(connector);
    if (index < 0)
        throw new Error(`no monitor ${connector}`);
    return index;
};

export default class CardcastExtension extends Extension {
    enable() {
        this._lastFocus = null;
        this._object = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._object.export(Gio.DBus.session, PATH);
        this._name = Gio.bus_own_name_on_connection(Gio.DBus.session, NAME,
            Gio.BusNameOwnerFlags.NONE, null, null);
        global.display.connectObject(
            'window-created', (_display, window) => {
                this._watch(window);
                this._changed();
            },
            'notify::focus-window', () => this._keepFocusOffMirror(),
            this);
        global.backend.get_monitor_manager().connectObject(
            'monitors-changed', () => this._changed(), this);
        for (const window of this._all())
            this._watch(window);
    }

    disable() {
        Gio.bus_unown_name(this._name);
        this._object.unexport();
        this._object = null;
        global.display.disconnectObject(this);
        global.backend.get_monitor_manager().disconnectObject(this);
        for (const window of this._all())
            window.disconnectObject(this);
        this._lastFocus = null;
    }

    _all() {
        return global.get_window_actors().map(actor => actor.meta_window).filter(window => window);
    }

    _watch(window) {
        window.connectObject(
            'position-changed', () => this._changed(),
            'size-changed', () => this._changed(),
            'notify::minimized', () => this._changed(),
            'unmanaged', () => this._changed(),
            this);
    }

    _changed() {
        this._object?.emit_signal('Changed', null);
    }

    _keepFocusOffMirror() {
        const focus = global.display.focus_window;
        if (!focus)
            return;
        if (!isMirror(focus)) {
            this._lastFocus = focus;
            return;
        }
        focus.make_above();
        const time = global.get_current_time();
        // A closed window has no actor any more.
        if (this._lastFocus?.get_compositor_private())
            this._lastFocus.focus(time);
        else
            global.display.unset_input_focus(time);
    }

    _window(id) {
        const window = this._all().find(candidate => String(candidate.get_id()) === id);
        if (!window)
            throw new Error(`no window ${id}`);
        return window;
    }

    // Each window the guard looks at: its id, "<left> <top> <right> <bottom> <maximised>
    // <minimised> <tool>" (the flags 0 or 1, a fullscreen window counting as maximised), its title,
    // and whether it is cardcast's mirror, which the guard never moves. Utility windows, toolbars
    // and windows kept off the taskbar (dialogs, desktop icons) count as tools.
    Windows() {
        return this._all()
            .filter(window => !window.is_override_redirect() &&
                GUARDED.includes(window.get_window_type()))
            .map(window => {
                const frame = window.get_frame_rect();
                const type = window.get_window_type();
                const tool = type === Meta.WindowType.UTILITY || type === Meta.WindowType.TOOLBAR ||
                    window.is_skip_taskbar();
                const state = [
                    frame.x, frame.y, frame.x + frame.width, frame.y + frame.height,
                    isMaximised(window) || window.is_fullscreen(), window.minimized, tool,
                ].map(Number).join(' ');
                return [String(window.get_id()), state, window.get_title() ?? '', isMirror(window)];
            });
    }

    // The work area of the monitor on connector `connector`: left, top, right, bottom.
    WorkArea(connector) {
        const area = global.workspace_manager.get_active_workspace()
            .get_work_area_for_monitor(monitorIndex(connector));
        return [area.x, area.y, area.x + area.width, area.y + area.height];
    }

    // Moves window `id`'s frame to the rectangle, without focusing or raising it.
    Move(id, left, top, right, bottom) {
        this._window(id).move_resize_frame(false, left, top, right - left, bottom - top);
    }

    // Moves window `id` to the monitor on `connector`; a maximised or fullscreen one stays so.
    SendToMonitor(id, connector) {
        this._window(id).move_to_monitor(monitorIndex(connector));
    }

    // The focused window's id, or "" (for cardcast's tests).
    Focus() {
        const focus = global.display.focus_window;
        return focus ? String(focus.get_id()) : '';
    }
}
