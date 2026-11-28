//! The Linux mirror (#43; docs/research/linux-support.md, section 2): the source monitor's frames
//! full-screen on the card display, in a surface that never takes the keyboard focus (a
//! layer-shell overlay on Plasma and wlroots, a fullscreen window that cardcast's Shell extension
//! keeps from taking the focus on GNOME, an override-redirect window on X11), drawn with wgpu
//! at the card display's refresh (Fifo), on its own thread like the Windows mirror window.

use super::super::{Ended, Mirror, Picture};
use super::gpu::{Gpu, Taken};
use super::screen::Screen;
use super::{Capture, capture};
use crate::guard::Monitor;
use crate::guard::desktop::{self, Desktop};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What the status line says on GNOME without cardcast's Shell extension: GNOME has no
/// layer-shell, and a fullscreen window there takes the game's focus unless the extension hands it back.
pub fn needs_gnome_extension() -> String {
    format!(
        "On GNOME the mirror needs cardcast's GNOME Shell extension, which is not running, so it doesn't take the game's focus. {}",
        crate::guard::gnome::INSTALL_EXTENSION
    )
}

/// How the mirror's surface goes on the card display without taking the keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// `zwlr_layer_shell_v1` overlay pinned to the card display's `wl_output` (Plasma, wlroots).
    LayerShell,
    /// An override-redirect window over the card display's rectangle; no window manager sees it.
    OverrideRedirect,
    /// A fullscreen `xdg_toplevel` on the card display's `wl_output` (GNOME), which cardcast's
    /// Shell extension keeps from taking the focus.
    Fullscreen,
}

/// How the mirror shows on `desktop`, or why it can't.
pub fn shown_on(desktop: Option<Desktop>) -> Result<Shown, Ended> {
    match desktop {
        Some(Desktop::X11) => Ok(Shown::OverrideRedirect),
        Some(Desktop::GnomeWayland) => Ok(Shown::Fullscreen),
        Some(_) => Ok(Shown::LayerShell),
        None => Err(Ended::Stop(
            "No desktop session to show the mirror on.".into(),
        )),
    }
}

/// The portal's restore token (`docs/linux-capture.md`): the next capture restores with it, and a
/// new one waits for the settings window to save it.
#[derive(Debug, Default)]
pub struct Token {
    current: Option<String>,
    unsaved: bool,
}

/// What the mirror thread shares with [`LinuxMirror`].
struct Running {
    /// How the mirror failed to start or ended on its own; `None` after Stop.
    thread: JoinHandle<Option<Ended>>,
    quit: Arc<AtomicBool>,
    shown: Arc<AtomicBool>,
    presents: Arc<AtomicU64>,
}

pub struct LinuxMirror {
    running: Option<Running>,
    wake: Arc<dyn Fn() + Send + Sync>,
    token: Arc<Mutex<Token>>,
    /// Set once a DMA-BUF frame failed to import, so later starts ask for shared memory only.
    no_dma_buf: Arc<AtomicBool>,
}

impl LinuxMirror {
    /// `restore_token` is the one saved in settings.
    pub fn new(wake: impl Fn() + Send + Sync + 'static, restore_token: Option<String>) -> Self {
        Self {
            running: None,
            wake: Arc::new(wake),
            token: Arc::new(Mutex::new(Token {
                current: restore_token,
                unsaved: false,
            })),
            no_dma_buf: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for LinuxMirror {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Mirror for LinuxMirror {
    /// Starts the mirror thread and returns; the portal's dialog never holds up the settings window.
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), Ended> {
        self.stop();
        let shown = shown_on(desktop::detect(|name| std::env::var(name).ok()))?;
        let quit = Arc::new(AtomicBool::new(false));
        let showing = Arc::new(AtomicBool::new(false));
        let presents = Arc::new(AtomicU64::new(0));
        let thread = {
            let thread = Thread {
                source_monitor: source_monitor.clone(),
                card_display: card_display.clone(),
                quit: quit.clone(),
                shown: showing.clone(),
                presents: presents.clone(),
                token: self.token.clone(),
                no_dma_buf: self.no_dma_buf.clone(),
                wake: self.wake.clone(),
            };
            std::thread::spawn(move || {
                let _wake = WakeOnEnd(thread.wake.clone());
                match thread.run(shown) {
                    Err(_) if thread.quit.load(Ordering::Relaxed) => None,
                    ended => ended.err(),
                }
            })
        };
        self.running = Some(Running {
            thread,
            quit,
            shown: showing,
            presents,
        });
        Ok(())
    }

    /// Joins the mirror thread; a capture still waiting on the portal's dialog is left to finish
    /// on its own thread. A mirror thread stuck in the compositor (a present while the card display
    /// sleeps) is left too, and ends on its own once it returns, so Stop never freezes the window.
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.quit.store(true, Ordering::Relaxed);
            let _ = join_within(running.thread, Duration::from_secs(1));
        }
    }

    fn is_running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| !running.thread.is_finished())
    }

    fn shows_picture(&self) -> bool {
        self.running.as_ref().is_some_and(|running| {
            !running.thread.is_finished() && running.shown.load(Ordering::Acquire)
        })
    }

    fn presents(&self) -> Option<u64> {
        Some(self.running.as_ref()?.presents.load(Ordering::Relaxed))
    }

    fn take_end(&mut self) -> Option<Ended> {
        if !self.running.as_ref()?.thread.is_finished() {
            return None;
        }
        self.running.take()?.thread.join().unwrap_or_else(|_| {
            Some(Ended::Stop(
                "The mirror stopped unexpectedly. Press Start to try again.".into(),
            ))
        })
    }

    fn take_restore_token(&mut self) -> Option<Option<String>> {
        take_unsaved(&self.token)
    }
}

/// The token to save, once a capture gave a new one.
pub fn take_unsaved(token: &Mutex<Token>) -> Option<Option<String>> {
    let mut token = token.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut token.unsaved).then(|| token.current.clone())
}

/// `thread`'s result if it ends `within` that time; otherwise it is left running.
pub fn join_within<T>(thread: JoinHandle<T>, within: Duration) -> Option<std::thread::Result<T>> {
    let deadline = Instant::now() + within;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Some(thread.join())
}

/// When to present next where the present doesn't wait for the card display's refresh (X11: a
/// driver without vsync, or a software one): one refresh after the last planned present, or `now`
/// after a stall, so missed refreshes don't come as a burst.
pub fn next_present(planned: Instant, now: Instant, period: Duration) -> Instant {
    let next = planned + period;
    if next + period < now { now } else { next }
}

/// Wakes the settings window when the mirror thread ends, however it ends (a panic too).
struct WakeOnEnd(Arc<dyn Fn() + Send + Sync>);

impl Drop for WakeOnEnd {
    fn drop(&mut self) {
        (self.0)();
    }
}

/// The mirror thread's part.
struct Thread {
    source_monitor: Monitor,
    card_display: Monitor,
    quit: Arc<AtomicBool>,
    shown: Arc<AtomicBool>,
    presents: Arc<AtomicU64>,
    token: Arc<Mutex<Token>>,
    no_dma_buf: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Thread {
    /// Opens the surface and graphics card, captures, then shows and draws until Stop.
    fn run(&self, shown: Shown) -> Result<(), Ended> {
        // A monitor missing here may be mid-replug, so it is retried.
        let monitors = crate::guard::monitors().map_err(Ended::Restart)?;
        let card_display = monitors
            .iter()
            .find(|monitor| monitor.device_path == self.card_display.device_path)
            .ok_or_else(|| Ended::Restart("The card display is not connected.".into()))?
            .clone();
        if shown == Shown::Fullscreen {
            let running = zbus::blocking::Connection::session()
                .is_ok_and(|bus| crate::guard::gnome::extension_running(&bus));
            if !running {
                return Err(Ended::Stop(needs_gnome_extension()));
            }
        }
        let mut screen = Screen::open(shown, &card_display)?;
        let mut gpu = Gpu::open(&screen)?;
        let modifiers = if self.no_dma_buf.load(Ordering::Relaxed) {
            Vec::new()
        } else {
            gpu.modifiers()
        };
        let source_monitor = self.source_monitor.clone();
        let captured = start_capture(
            move |token| capture(&source_monitor, &monitors, token, &modifiers),
            self.token.clone(),
            self.wake.clone(),
        );
        let Some(mut picture) = wait_for_picture(&captured, &self.quit)? else {
            return Ok(());
        };
        // Shown only once capture works, so a start that keeps failing never covers the card display.
        screen.show()?;
        // On X11 each loop is a full MIT-SHM grab too, so it keeps to the refresh whatever the driver does.
        let paced = match &screen {
            Screen::X11(window) => Some(window.refresh),
            Screen::Wayland(_) => None,
        };
        let mut planned = Instant::now();
        while !self.quit.load(Ordering::Relaxed) {
            if let Some(refresh) = paced {
                let now = Instant::now();
                planned = next_present(planned, now, refresh);
                std::thread::sleep(planned.saturating_duration_since(now));
            }
            screen.follow()?;
            match picture.take_frame()?.map(|frame| gpu.take(frame)) {
                Some(Taken::NotImported(why)) => {
                    self.no_dma_buf.store(true, Ordering::Relaxed);
                    return Err(Ended::Restart(format!(
                        "The graphics card could not take the captured picture directly; restarting the mirror with a copy. ({why})"
                    )));
                }
                Some(Taken::Failed(ended)) => return Err(ended),
                Some(Taken::Yes) | None => {}
            }
            // Fifo waits here for the card display's next refresh.
            if gpu.present()? {
                self.presents.fetch_add(1, Ordering::Relaxed);
                if !self.shown.swap(true, Ordering::AcqRel) {
                    (self.wake)();
                }
            } else {
                // No picture yet, or the compositor skipped this refresh.
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }
}

type Captured = Result<Box<dyn Picture + Send>, Ended>;

/// Runs `capture` with the saved token on a thread of its own, which nothing joins: the portal's
/// dialog blocks it until answered, and Stop must not wait for that. Its new token is kept and the
/// settings window woken to save it, even after Stop; its picture then has no one to take it and
/// is dropped, ending the capture.
pub fn start_capture(
    capture: impl FnOnce(Option<&str>) -> Capture + Send + 'static,
    token: Arc<Mutex<Token>>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> mpsc::Receiver<Captured> {
    let (sender, receiver) = mpsc::channel();
    let saved = token
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .current
        .clone();
    std::thread::spawn(move || {
        let Capture {
            picture,
            restore_token,
        } = capture(saved.as_deref());
        {
            let mut token = token.lock().unwrap_or_else(PoisonError::into_inner);
            if token.current != restore_token {
                token.current = restore_token;
                token.unsaved = true;
            }
        }
        wake();
        let _ = sender.send(picture);
    });
    receiver
}

/// The capture's picture once it started; `None` if Stop came first.
pub fn wait_for_picture(
    captured: &mpsc::Receiver<Captured>,
    quit: &AtomicBool,
) -> Result<Option<Box<dyn Picture + Send>>, Ended> {
    loop {
        match captured.recv_timeout(Duration::from_millis(20)) {
            Ok(picture) => return picture.map(Some),
            Err(RecvTimeoutError::Timeout) if quit.load(Ordering::Relaxed) => return Ok(None),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Ended::Restart(
                    "The screen capture stopped unexpectedly; restarting it.".into(),
                ));
            }
        }
    }
}
