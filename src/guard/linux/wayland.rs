//! The desktop's outputs on Wayland: `wl_output` (name, mode) and `xdg-output` (the position and
//! size on the desktop, in logical pixels), which Plasma, GNOME, sway and Hyprland all offer
//! (<https://wayland.app/protocols/wayland#wl_output>, <https://wayland.app/protocols/xdg-output-unstable-v1>).
//! Plasma names its primary monitor first in `kde-output-order-v1`
//! (<https://invent.kde.org/libraries/plasma-wayland-protocols/-/blob/master/src/protocols/kde-output-order-v1.xml>).

use super::Output;
use std::error::Error;
use std::sync::mpsc;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::{self, Transform, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1::{
    self, KdeOutputOrderV1,
};

/// One output as its events describe it.
#[derive(Default)]
struct Listed {
    output: Output,
    /// The current mode, before the output's rotation.
    mode: (u32, u32),
    sideways: bool,
    /// From xdg-output, when the desktop offers it.
    logical_size: Option<(u32, u32)>,
}

#[derive(Default)]
struct Outputs {
    listed: Vec<Listed>,
    /// Output names in kde-output-order-v1's order, the primary first; complete once `order_done`.
    order: Vec<String>,
    order_done: bool,
}

/// The outputs, and the primary output's name where the desktop gives one (Plasma).
pub fn outputs() -> Result<(Vec<Output>, Option<String>), Box<dyn Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Outputs>(&connection)?;
    let handle = queue.handle();
    let wl_outputs: Vec<WlOutput> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|global| global.interface == "wl_output")
            .enumerate()
            .map(|(index, global)| {
                globals
                    .registry()
                    .bind(global.name, global.version.min(4), &handle, index)
            })
            .collect()
    });
    let mut outputs = Outputs {
        listed: wl_outputs.iter().map(|_| Listed::default()).collect(),
        ..Outputs::default()
    };
    // Bound for its events only: it sends the order when bound.
    let _order = globals.bind::<KdeOutputOrderV1, _, _>(&handle, 1..=1, ());
    if let Ok(manager) = globals.bind::<ZxdgOutputManagerV1, _, _>(&handle, 1..=3, ()) {
        for (index, output) in wl_outputs.iter().enumerate() {
            manager.get_xdg_output(output, &handle, index);
        }
    }
    queue.roundtrip(&mut outputs)?;
    queue.roundtrip(&mut outputs)?;
    let primary = outputs
        .order_done
        .then(|| outputs.order.first().cloned())
        .flatten();
    Ok((
        outputs.listed.into_iter().map(Listed::output).collect(),
        primary,
    ))
}

impl Listed {
    fn output(self) -> Output {
        let (width, height) = super::super::desktop_size(self.mode.0, self.mode.1, self.sideways);
        Output {
            width,
            height,
            // Without xdg-output the desktop gives no logical size; wl_output's integer scale is
            // not the real (possibly fractional) one, so the resolution is the closest there is.
            layout_size: self.logical_size.unwrap_or((width, height)),
            ..self.output
        }
    }
}

impl Dispatch<WlOutput, usize> for Outputs {
    fn event(
        outputs: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(listed) = outputs.listed.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x,
                y,
                transform,
                make,
                model,
                ..
            } => {
                (listed.output.left, listed.output.top) = (x, y);
                listed.sideways = matches!(
                    transform,
                    WEnum::Value(
                        Transform::_90
                            | Transform::_270
                            | Transform::Flipped90
                            | Transform::Flipped270
                    )
                );
                if listed.output.description.is_empty() {
                    listed.output.description = format!("{make} {model}");
                }
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current) => {
                listed.mode = (
                    u32::try_from(width).unwrap_or(0),
                    u32::try_from(height).unwrap_or(0),
                );
                // In millihertz.
                listed.output.refresh_hz = u32::try_from((refresh + 500) / 1000).unwrap_or(0);
            }
            wl_output::Event::Name { name } => listed.output.name = name,
            wl_output::Event::Description { description } => {
                listed.output.description = description;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for Outputs {
    fn event(
        outputs: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(listed) = outputs.listed.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                (listed.output.left, listed.output.top) = (x, y);
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                listed.logical_size = u32::try_from(width).ok().zip(u32::try_from(height).ok());
            }
            // wl_output's name comes first from version 4; older outputs have only this one.
            zxdg_output_v1::Event::Name { name } if listed.output.name.is_empty() => {
                listed.output.name = name;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Outputs {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Outputs: ignore ZxdgOutputManagerV1);

impl Dispatch<KdeOutputOrderV1, ()> for Outputs {
    fn event(
        outputs: &mut Self,
        _: &KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // After `done`, the next name begins a new list.
            kde_output_order_v1::Event::Output { output_name } => {
                if std::mem::take(&mut outputs.order_done) {
                    outputs.order.clear();
                }
                outputs.order.push(output_name);
            }
            kde_output_order_v1::Event::Done => outputs.order_done = true,
            _ => {}
        }
    }
}

/// Whether an output came, went or changed since the watcher last looked.
#[derive(Default)]
struct Watch {
    changed: bool,
}

/// Sends whether it could start to `started`, then calls `wake` after every output change until
/// `wake` returns false or the desktop closes the connection.
pub fn watch(started: &mpsc::Sender<Result<(), String>>, wake: impl Fn() -> bool) {
    let setup = || -> Result<_, Box<dyn Error>> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Watch>(&connection)?;
        let handle = queue.handle();
        globals.contents().with_list(|list| {
            for global in list.iter().filter(|global| global.interface == "wl_output") {
                globals.registry().bind::<WlOutput, _, _>(
                    global.name,
                    global.version.min(4),
                    &handle,
                    (),
                );
            }
        });
        // A new primary monitor on Plasma changes only the order.
        let order = globals
            .bind::<KdeOutputOrderV1, _, _>(&handle, 1..=1, ())
            .ok();
        let mut watch = Watch::default();
        // The outputs' first events describe them as they are; only later ones are changes.
        queue.roundtrip(&mut watch)?;
        Ok((connection, globals, order, queue))
    };
    let (_connection, _globals, _order, mut queue) = match setup() {
        Ok(watching) => watching,
        Err(error) => {
            let _ = started.send(Err(error.to_string()));
            return;
        }
    };
    if started.send(Ok(())).is_err() {
        return;
    }
    let mut watch = Watch::default();
    while queue.blocking_dispatch(&mut watch).is_ok() {
        if std::mem::take(&mut watch.changed) && !wake() {
            return;
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Watch {
    fn event(
        watch: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                registry.bind::<WlOutput, _, _>(name, version.min(4), handle, ());
                watch.changed = true;
            }
            // Ids are not kept, so any removed global counts; a spare wake only re-reads the list.
            wl_registry::Event::GlobalRemove { .. } => watch.changed = true,
            _ => {}
        }
    }
}

impl Dispatch<KdeOutputOrderV1, ()> for Watch {
    fn event(
        watch: &mut Self,
        _: &KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, kde_output_order_v1::Event::Done) {
            watch.changed = true;
        }
    }
}

impl Dispatch<WlOutput, ()> for Watch {
    fn event(
        watch: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Every change of an output ends with `done` (version 2 and later).
        if matches!(event, wl_output::Event::Done) {
            watch.changed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sender's DP-1 as Plasma describes it: mode 2560×1440, turned 270°, scale 1.3.
    fn dp_1(logical_size: Option<(u32, u32)>) -> Output {
        Listed {
            output: Output {
                name: "DP-1".into(),
                left: 0,
                top: 373,
                refresh_hz: 144,
                ..Output::default()
            },
            mode: (2560, 1440),
            sideways: true,
            logical_size,
        }
        .output()
    }

    #[test]
    fn a_turned_scaled_output_has_its_real_resolution_and_its_logical_size() {
        let output = dp_1(Some((1108, 1969)));
        assert_eq!(
            (output.width, output.height, output.layout_size),
            (1440, 2560, (1108, 1969))
        );
        assert_eq!((output.left, output.top), (0, 373));
    }

    #[test]
    fn without_xdg_output_the_desktop_size_is_the_resolution() {
        let output = dp_1(None);
        assert_eq!(
            (output.width, output.height, output.layout_size),
            (1440, 2560, (1440, 2560))
        );
    }
}
