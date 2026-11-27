//! The desktop's outputs on Wayland: `wl_output` (name, mode) and `xdg-output` (the position and
//! size on the desktop, in logical pixels), which Plasma, GNOME, sway and Hyprland all offer
//! (<https://wayland.app/protocols/wayland#wl_output>, <https://wayland.app/protocols/xdg-output-unstable-v1>).

use super::Output;
use std::error::Error;
use std::sync::mpsc;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::{self, Transform, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};

/// One output as its events describe it.
#[derive(Default)]
struct Listed {
    output: Output,
    /// The current mode, before the output's rotation, and the integer scale.
    mode: (u32, u32),
    sideways: bool,
    scale: u32,
    /// From xdg-output, when the desktop offers it.
    logical_size: Option<(u32, u32)>,
}

#[derive(Default)]
struct Outputs(Vec<Listed>);

pub fn outputs() -> Result<Vec<Output>, Box<dyn Error>> {
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
    let mut outputs = Outputs(wl_outputs.iter().map(|_| Listed::default()).collect());
    if let Ok(manager) = globals.bind::<ZxdgOutputManagerV1, _, _>(&handle, 1..=3, ()) {
        for (index, output) in wl_outputs.iter().enumerate() {
            manager.get_xdg_output(output, &handle, index);
        }
    }
    queue.roundtrip(&mut outputs)?;
    queue.roundtrip(&mut outputs)?;
    Ok(outputs.0.into_iter().map(Listed::output).collect())
}

impl Listed {
    fn output(self) -> Output {
        let (width, height) = self.logical_size.unwrap_or_else(|| {
            let (width, height) =
                super::super::desktop_size(self.mode.0, self.mode.1, self.sideways);
            let scale = self.scale.max(1);
            (width / scale, height / scale)
        });
        Output {
            width,
            height,
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
        let Some(listed) = outputs.0.get_mut(*index) else {
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
            wl_output::Event::Scale { factor } => listed.scale = u32::try_from(factor).unwrap_or(1),
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
        let Some(listed) = outputs.0.get_mut(*index) else {
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
        let mut watch = Watch::default();
        // The outputs' first events describe them as they are; only later ones are changes.
        queue.roundtrip(&mut watch)?;
        Ok((connection, globals, queue))
    };
    let (_connection, _globals, mut queue) = match setup() {
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
