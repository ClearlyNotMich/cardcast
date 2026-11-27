//! Which Linux desktop cardcast runs on, from its environment; a `match` on it picks the per-desktop
//! helper (docs/research/linux-support.md, section 8). No platform calls.

/// The desktops cardcast tells apart; sway and Hyprland make up the research's wlroots column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    PlasmaWayland,
    GnomeWayland,
    Sway,
    Hyprland,
    /// A Wayland compositor with no helper of its own.
    OtherWayland,
    X11,
}

/// The desktop from environment variables (`var` reads one, as `std::env::var(name).ok()` does), or
/// `None` outside a graphical session.
pub fn detect(var: impl Fn(&str) -> Option<String>) -> Option<Desktop> {
    let set = |name| var(name).is_some_and(|value| !value.is_empty());
    // A compositor's own socket and WAYLAND_DISPLAY name the compositor cardcast runs in, even nested
    // in another session whose XDG_SESSION_TYPE and XDG_CURRENT_DESKTOP it inherited.
    if set("SWAYSOCK") {
        return Some(Desktop::Sway);
    }
    if set("HYPRLAND_INSTANCE_SIGNATURE") {
        return Some(Desktop::Hyprland);
    }
    let session = var("XDG_SESSION_TYPE");
    // Xwayland sets DISPLAY on Wayland too, so DISPLAY counts only without a Wayland session.
    if !set("WAYLAND_DISPLAY") && session.as_deref() != Some("wayland") {
        return (session.as_deref() == Some("x11") || set("DISPLAY")).then_some(Desktop::X11);
    }
    // XDG_CURRENT_DESKTOP is a colon-separated list, for example "ubuntu:GNOME".
    let current = var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let named = |name: &str| {
        current
            .split(':')
            .any(|part| part.eq_ignore_ascii_case(name))
    };
    Some(if named("KDE") {
        Desktop::PlasmaWayland
    } else if named("GNOME") {
        Desktop::GnomeWayland
    } else if named("sway") {
        Desktop::Sway
    } else if named("Hyprland") {
        Desktop::Hyprland
    } else {
        Desktop::OtherWayland
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `detect` over these environment variables only.
    fn on(vars: &[(&str, &str)]) -> Option<Desktop> {
        detect(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn plasma_on_wayland() {
        let plasma = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ];
        assert_eq!(on(&plasma), Some(Desktop::PlasmaWayland));
    }

    #[test]
    fn gnome_on_wayland_also_under_a_distribution_name() {
        let gnome = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ];
        assert_eq!(on(&gnome), Some(Desktop::GnomeWayland));
        let ubuntu = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ];
        assert_eq!(on(&ubuntu), Some(Desktop::GnomeWayland));
    }

    #[test]
    fn sway_and_hyprland_by_their_ipc_socket_or_desktop_name() {
        let sway = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("SWAYSOCK", "/run/user/1000/sway-ipc.sock"),
        ];
        assert_eq!(on(&sway), Some(Desktop::Sway));
        let named = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "sway"),
        ];
        assert_eq!(on(&named), Some(Desktop::Sway));
        let hyprland = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123"),
        ];
        assert_eq!(on(&hyprland), Some(Desktop::Hyprland));
        let named = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "Hyprland"),
        ];
        assert_eq!(on(&named), Some(Desktop::Hyprland));
    }

    #[test]
    fn another_wayland_compositor_has_no_helper() {
        let river = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "river"),
        ];
        assert_eq!(on(&river), Some(Desktop::OtherWayland));
    }

    #[test]
    fn an_x11_session_is_x11_whatever_the_desktop() {
        let plasma = [("XDG_SESSION_TYPE", "x11"), ("XDG_CURRENT_DESKTOP", "KDE")];
        assert_eq!(on(&plasma), Some(Desktop::X11));
    }

    #[test]
    fn a_nested_compositor_wins_over_the_session_it_runs_in() {
        // sway or Hyprland started inside Plasma inherit Plasma's desktop name.
        let sway_in_plasma = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("SWAYSOCK", "/run/user/1000/sway-ipc.sock"),
        ];
        assert_eq!(on(&sway_in_plasma), Some(Desktop::Sway));
        let hyprland_in_gnome = [
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123"),
        ];
        assert_eq!(on(&hyprland_in_gnome), Some(Desktop::Hyprland));
        // A Wayland compositor nested in an X11 session.
        let river_in_x11 = [
            ("XDG_SESSION_TYPE", "x11"),
            ("XDG_CURRENT_DESKTOP", "river"),
            ("DISPLAY", ":0"),
            ("WAYLAND_DISPLAY", "wayland-1"),
        ];
        assert_eq!(on(&river_in_x11), Some(Desktop::OtherWayland));
    }

    #[test]
    fn without_a_session_type_the_display_variables_decide() {
        // Xwayland sets DISPLAY beside WAYLAND_DISPLAY; the Wayland session wins.
        let wayland = [
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ];
        assert_eq!(on(&wayland), Some(Desktop::PlasmaWayland));
        assert_eq!(on(&[("DISPLAY", ":0")]), Some(Desktop::X11));
        let tty = [("XDG_SESSION_TYPE", "tty")];
        assert_eq!(on(&tty), None);
        assert_eq!(on(&[]), None);
    }
}
