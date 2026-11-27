//! GNOME's primary monitor, from Mutter's `org.gnome.Mutter.DisplayConfig.GetCurrentState` on the
//! session bus: each logical monitor has an is-primary flag and lists its monitors' connectors
//! (<https://gitlab.gnome.org/GNOME/mutter/-/blob/main/data/dbus-interfaces/org.gnome.Mutter.DisplayConfig.xml>).

use std::collections::HashMap;
use zbus::zvariant::OwnedValue;

type Properties = HashMap<String, OwnedValue>;
/// Connector, vendor, product, serial.
type MonitorSpec = (String, String, String, String);
type Mode = (String, i32, i32, f64, f64, Vec<f64>, Properties);
type Monitor = (MonitorSpec, Vec<Mode>, Properties);
/// x, y, scale, transform, primary, monitors, properties.
pub type LogicalMonitor = (i32, i32, f64, u32, bool, Vec<MonitorSpec>, Properties);
type State = (u32, Vec<Monitor>, Vec<LogicalMonitor>, Properties);

/// The primary monitor's connector, which is GNOME's wl_output name; `None` when Mutter doesn't answer.
pub fn primary() -> Option<String> {
    let connection = zbus::blocking::Connection::session().ok()?;
    let reply = connection
        .call_method(
            Some("org.gnome.Mutter.DisplayConfig"),
            "/org/gnome/Mutter/DisplayConfig",
            Some("org.gnome.Mutter.DisplayConfig"),
            "GetCurrentState",
            &(),
        )
        .ok()?;
    let (_, _, logical_monitors, _): State = reply.body().deserialize().ok()?;
    primary_connector(&logical_monitors)
}

/// The first connector of the logical monitor marked primary.
pub fn primary_connector(logical_monitors: &[LogicalMonitor]) -> Option<String> {
    let (.., monitors, _) = logical_monitors.iter().find(|logical| logical.4)?;
    Some(monitors.first()?.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logical(primary: bool, connectors: &[&str]) -> LogicalMonitor {
        let specs = connectors
            .iter()
            .map(|c| {
                (
                    (*c).into(),
                    "DEL".into(),
                    "DELL AW2523HF".into(),
                    "4GZY5P3".into(),
                )
            })
            .collect();
        (0, 0, 1.0, 0, primary, specs, Properties::new())
    }

    #[test]
    fn the_primary_logical_monitors_connector_is_the_primary_monitor() {
        let state = [logical(false, &["DP-1"]), logical(true, &["DP-3"])];
        assert_eq!(primary_connector(&state), Some("DP-3".into()));
        // Mirrored monitors share one logical monitor; its first is taken.
        let mirrored = [logical(true, &["DP-2", "HDMI-A-2"])];
        assert_eq!(primary_connector(&mirrored), Some("DP-2".into()));
        assert_eq!(primary_connector(&[logical(false, &["DP-1"])]), None);
        assert_eq!(primary_connector(&[logical(true, &[])]), None);
    }
}
