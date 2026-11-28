//! The portal's PipeWire stream of the source monitor (`docs/linux-capture.md`): DMA-BUF offered
//! first with the present's modifiers, shared memory as the fallback; the newest frame waits in
//! [`Stream`] for the present to take it.

use super::super::{Ended, Frame, Picture, Pixels, Release};
use super::{FIRST_FRAME, fourcc, no_frame, rows};
use pipewire as pw;
use pw::spa;
use pw::spa::param::video::{VideoFlags, VideoFormat, VideoInfoRaw};
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Rectangle, SpaTypes};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::File;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

/// What the PipeWire thread hands the present.
#[derive(Debug, Default)]
pub struct Latest {
    pub frame: Option<Frame>,
    ended: Option<Ended>,
}

impl Latest {
    /// Puts `frame` up for the present; one it never took is dropped, giving its buffer back.
    pub fn put(&mut self, frame: Frame) {
        self.frame = Some(frame);
    }
}

/// What the PipeWire thread is asked to do from other threads.
enum Message {
    Quit,
    /// Give a buffer a dropped frame held back to the stream.
    Release(Held),
}

/// A buffer dequeued from the stream and held for a frame.
struct Held(NonNull<pw::sys::pw_buffer>);

// SAFETY: the pointer is only passed back to the PipeWire thread, which alone dereferences it,
// and only while the stream still has the buffer (`live`).
unsafe impl Send for Held {}

/// The stream, read on its own PipeWire thread.
pub struct Stream {
    latest: Arc<Mutex<Latest>>,
    messages: pw::channel::Sender<Message>,
    thread: Option<JoinHandle<()>>,
    /// The portal's node, and since when it has been waited on until the first frame came.
    node: u32,
    waiting: Option<Instant>,
    /// Whatever keeps the stream's source alive, such as the portal session; dropped after the thread.
    _source: Box<dyn Send>,
}

impl Stream {
    /// Connects to `node` on the PipeWire `remote`. `modifiers` are the DMA-BUF layouts the
    /// present can import for these formats; with none, frames come as shared memory.
    pub fn open(
        remote: OwnedFd,
        node: u32,
        modifiers: &[u64],
        source: Box<dyn Send>,
    ) -> Result<Self, Ended> {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let (messages, receiver) = pw::channel::channel();
        let (ready, started) = mpsc::channel();
        let thread = {
            let (latest, modifiers, sender) =
                (latest.clone(), modifiers.to_vec(), messages.clone());
            std::thread::spawn(move || {
                let channel = (sender, receiver);
                if let Err(error) = run(remote, node, &modifiers, latest, channel, &ready) {
                    let _ = ready.send(Err(error));
                }
            })
        };
        let stream = Self {
            latest,
            messages,
            thread: Some(thread),
            node,
            waiting: Some(Instant::now()),
            _source: source,
        };
        match started.recv() {
            Ok(Ok(())) => Ok(stream),
            Ok(Err(error)) => Err(Ended::Restart(format!(
                "Could not open the screen capture stream. ({error})"
            ))),
            Err(_) => Err(Ended::Restart(
                "Could not open the screen capture stream.".into(),
            )),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.messages.send(Message::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Picture for Stream {
    fn take_frame(&mut self) -> Result<Option<Frame>, Ended> {
        let frame = {
            let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            match latest.ended.clone() {
                Some(ended) => return Err(ended),
                None => latest.frame.take(),
            }
        };
        if frame.is_some() {
            self.waiting = None;
        } else if let Some(since) = self.waiting {
            // Asked without the lock: pw-dump may take seconds, and the PipeWire thread needs it.
            let waited = since.elapsed();
            if let Some(ended) = no_frame(waited, || crate::audio::node_linked(self.node)) {
                return Err(ended);
            }
            if waited >= FIRST_FRAME {
                self.waiting = Some(Instant::now());
            }
        }
        Ok(frame)
    }
}

/// The PipeWire thread: says on `ready` whether the stream connected, then runs until `quit`.
fn run(
    remote: OwnedFd,
    node: u32,
    modifiers: &[u64],
    latest: Arc<Mutex<Latest>>,
    (releases, messages): (pw::channel::Sender<Message>, pw::channel::Receiver<Message>),
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|error| error.to_string())?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_fd_rc(remote, None)
        .map_err(|error| error.to_string())?;
    let stream = pw::stream::StreamRc::new(
        core,
        "cardcast mirror",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| error.to_string())?;
    let ended = latest.clone();
    // The buffers the stream has now; a held one it removed (on a new format) isn't given back.
    let live = Rc::new(RefCell::new(HashSet::<usize>::new()));
    let _listener = stream
        .add_local_listener_with_user_data(VideoInfoRaw::new())
        .state_changed(move |_, _, _, state| {
            let why = match state {
                pw::stream::StreamState::Error(error) => error,
                pw::stream::StreamState::Unconnected => "disconnected".into(),
                _ => return,
            };
            ended.lock().unwrap_or_else(PoisonError::into_inner).ended = Some(Ended::Restart(
                format!("The screen capture stopped; restarting it. ({why})"),
            ));
        })
        .param_changed(|stream, format, id, param| {
            let Some(param) = param.filter(|_| id == spa::param::ParamType::Format.as_raw()) else {
                return;
            };
            if format.parse(param).is_err() {
                return;
            }
            let dma_buf = format.flags().contains(VideoFlags::MODIFIER);
            if let Some(bytes) = pod(buffers(dma_buf))
                && let Some(buffers) = Pod::from_bytes(&bytes)
            {
                let _ = stream.update_params(&mut [buffers]);
            }
        })
        .add_buffer({
            let live = live.clone();
            move |_, _, buffer| {
                live.borrow_mut().insert(buffer as usize);
            }
        })
        .remove_buffer({
            let live = live.clone();
            move |_, _, buffer| {
                live.borrow_mut().remove(&(buffer as usize));
            }
        })
        .process(move |stream, format| {
            // SAFETY: `stream` is this thread's live stream; the buffer it hands out is cardcast's
            // until queued back.
            let Some(buffer) =
                NonNull::new(unsafe { pw::sys::pw_stream_dequeue_buffer(stream.as_raw_ptr()) })
            else {
                return;
            };
            let give_back = || {
                // SAFETY: dequeued from this stream just above and not queued since.
                unsafe { pw::sys::pw_stream_queue_buffer(stream.as_raw_ptr(), buffer.as_ptr()) };
            };
            // SAFETY: a dequeued buffer stays valid until queued back.
            let Some(pixels) = (unsafe { pixels(buffer, format) }) else {
                give_back();
                return;
            };
            let (Some(fourcc), size) = (fourcc(format.format()), format.size()) else {
                give_back();
                return;
            };
            // A copy needs the buffer no longer; a DMA-BUF plane is held until its frame is dropped.
            let release = if matches!(pixels, Pixels::DmaBuf { .. }) {
                let (releases, held) = (releases.clone(), Held(buffer));
                Release::new(move || {
                    let _ = releases.send(Message::Release(held));
                })
            } else {
                give_back();
                Release::default()
            };
            latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(Frame {
                    width: size.width,
                    height: size.height,
                    fourcc,
                    pixels,
                    release,
                });
        })
        .register()
        .map_err(|error| error.to_string())?;

    let formats: Vec<Vec<u8>> = [Some(modifiers).filter(|list| !list.is_empty()), None]
        .into_iter()
        .filter_map(|modifiers| pod(enum_format(modifiers)))
        .collect();
    let mut params: Vec<&Pod> = formats
        .iter()
        .filter_map(|bytes| Pod::from_bytes(bytes))
        .collect();
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT,
            &mut params,
        )
        .map_err(|error| error.to_string())?;

    let _messages = messages.attach(main_loop.loop_(), {
        let (main_loop, stream) = (main_loop.clone(), stream.clone());
        move |message| match message {
            Message::Quit => main_loop.quit(),
            Message::Release(Held(buffer)) => {
                if live.borrow().contains(&(buffer.as_ptr() as usize)) {
                    // SAFETY: the stream still has this buffer, dequeued for a frame now dropped.
                    unsafe {
                        pw::sys::pw_stream_queue_buffer(stream.as_raw_ptr(), buffer.as_ptr())
                    };
                }
            }
        }
    });
    let _ = ready.send(Ok(()));
    main_loop.run();
    Ok(())
}

/// The formats cardcast takes, 8-bit RGB in the orders `fourcc` knows, any size and rate: one
/// `EnumFormat` with `modifiers` for DMA-BUF, or without for shared memory.
fn enum_format(modifiers: Option<&[u64]>) -> Object {
    let id = |format: VideoFormat| spa::utils::Id(format.as_raw());
    let mut properties = vec![
        Property::new(
            spa::param::format::FormatProperties::MediaType.as_raw(),
            Value::Id(spa::utils::Id(
                spa::param::format::MediaType::Video.as_raw(),
            )),
        ),
        Property::new(
            spa::param::format::FormatProperties::MediaSubtype.as_raw(),
            Value::Id(spa::utils::Id(
                spa::param::format::MediaSubtype::Raw.as_raw(),
            )),
        ),
        Property::new(
            spa::param::format::FormatProperties::VideoFormat.as_raw(),
            Value::Choice(ChoiceValue::Id(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: id(VideoFormat::BGRx),
                    alternatives: [
                        VideoFormat::BGRx,
                        VideoFormat::BGRA,
                        VideoFormat::RGBx,
                        VideoFormat::RGBA,
                    ]
                    .map(id)
                    .to_vec(),
                },
            ))),
        ),
        Property::new(
            spa::param::format::FormatProperties::VideoSize.as_raw(),
            Value::Choice(ChoiceValue::Rectangle(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Rectangle {
                        width: 1920,
                        height: 1080,
                    },
                    min: Rectangle {
                        width: 1,
                        height: 1,
                    },
                    max: Rectangle {
                        width: 16384,
                        height: 16384,
                    },
                },
            ))),
        ),
        Property::new(
            spa::param::format::FormatProperties::VideoFramerate.as_raw(),
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Fraction { num: 60, denom: 1 },
                    min: Fraction { num: 0, denom: 1 },
                    max: Fraction {
                        num: 1000,
                        denom: 1,
                    },
                },
            ))),
        ),
    ];
    if let Some(modifiers) = modifiers {
        let modifiers: Vec<i64> = modifiers.iter().map(|&m| m.cast_signed()).collect();
        properties.push(Property {
            key: spa::param::format::FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: modifiers.first().copied().unwrap_or_default(),
                    alternatives: modifiers,
                },
            ))),
        });
    }
    Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties,
    }
}

/// The `Buffers` param once the format is known: DMA-BUF if a modifier was agreed, else memory.
fn buffers(dma_buf: bool) -> Object {
    let types = if dma_buf {
        1 << spa::buffer::DataType::DmaBuf.as_raw()
    } else {
        (1 << spa::buffer::DataType::MemFd.as_raw()) | (1 << spa::buffer::DataType::MemPtr.as_raw())
    };
    Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            // One held by the present, one waiting for it, and one for the compositor to draw in.
            Property::new(
                spa::sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: 4,
                        min: 3,
                        max: 16,
                    },
                ))),
            ),
            Property::new(
                spa::sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: types,
                        flags: vec![types],
                    },
                ))),
            ),
        ],
    }
}

/// The pixels of `buffer`'s first plane as `format` says: a DMA-BUF fd to hold, or a copy of
/// shared memory. `None` if it has no usable plane.
///
/// # Safety
/// `buffer` is dequeued from the stream and not queued back.
unsafe fn pixels(buffer: NonNull<pw::sys::pw_buffer>, format: &VideoInfoRaw) -> Option<Pixels> {
    // SAFETY: the caller's buffer is valid; PipeWire fills `buffer` and `datas` before handing it out.
    let data = unsafe {
        let spa_buffer = buffer.as_ref().buffer.as_mut()?;
        if spa_buffer.n_datas == 0 {
            return None;
        }
        // `Data` is a transparent wrapper of `spa_data`.
        spa_buffer.datas.cast::<spa::buffer::Data>().as_mut()?
    };
    let size = format.size();
    let chunk = data.chunk();
    if chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
        return None;
    }
    let (offset, stride) = (chunk.offset(), u32::try_from(chunk.stride()).unwrap_or(0));
    let stride = if stride == 0 { size.width * 4 } else { stride };
    let kind = data.type_();
    if kind == spa::buffer::DataType::MemPtr {
        // In cardcast's own memory, so PipeWire gives the pointer.
        let bytes = data
            .data()
            .and_then(|bytes| rows(bytes, offset, stride, size.height))?;
        return Some(Pixels::Memory {
            bytes: bytes.to_vec(),
            stride,
        });
    }
    let raw_fd = data.fd();
    if raw_fd < 0 {
        return None;
    }
    // SAFETY: the fd is not negative, and PipeWire keeps it open while the buffer is dequeued,
    // which outlasts this borrow: it is only duplicated.
    let fd = unsafe { BorrowedFd::borrow_raw(raw_fd) }
        .try_clone_to_owned()
        .ok()?;
    if kind == spa::buffer::DataType::DmaBuf {
        return Some(Pixels::DmaBuf {
            fd,
            offset,
            stride,
            modifier: format.modifier(),
        });
    }
    // Shared memory is read, not mapped: producers such as xdg-desktop-portal-wlr don't mark it
    // readable, so PipeWire's own mapping can't be read.
    let mut bytes = vec![0; usize::try_from(stride * size.height).ok()?];
    let at = u64::from(data.as_raw().mapoffset) + u64::from(offset);
    File::from(fd).read_exact_at(&mut bytes, at).ok()?;
    Some(Pixels::Memory { bytes, stride })
}

/// `object` as pod bytes.
fn pod(object: Object) -> Option<Vec<u8>> {
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(object),
    )
    .ok()
    .map(|(cursor, _)| cursor.into_inner())
}
