//! [`GpuSession`]: the supervisor that owns the GPU process.
//!
//! The session is not a backend. It owns no pixels and no rasterizer logic of
//! its own: it owns a *process*, decides whether to trust it, and when it cannot
//! it renders the same display list with [`SoftwareRasterizer`] in this process.
//! The caller sees a `Canvas` either way, and (because the software backend is
//! deterministic and the GPU process runs the same one) the same bytes either
//! way, which is what makes "the next frame equals the software baseline" a
//! statement a test can check.
//!
//! # What makes a failure
//!
//! Anything but a complete, correct frame within the deadline: the process
//! cannot be created under the policy or does not confirm it; it dies; it is
//! killed; it does not answer in time; it sends bytes that do not decode, a
//! message over the size ceiling, a header for the wrong request, a strip that
//! is not the next one the receiver's arithmetic allows, a strip too many, or
//! reports a failure of its own. Each is recorded as a [`Cause`] in the event
//! log, the process is killed and dropped, and the frame in hand is rendered in
//! software in the same call. Nothing the process said is used.
//!
//! # Restart policy
//!
//! A failed process is replaced on the **next** frame, not the same one: the
//! frame in hand is already being rendered in software and a respawn costs far
//! more than a frame. Replacement is bounded by [`GpuConfig::max_restarts`],
//! default [`DEFAULT_MAX_RESTARTS`], counted over the session's whole life and
//! never reset by a success. Once the bound is exceeded the session stays on
//! software for good and says so ([`GpuEvent::GaveUp`]).
//!
//! Eight is the bound `ContentProcess` already uses (ADR 033 (e) names it), and
//! it is the right size for the same reason: a process that has failed nine
//! times is not suffering a transient fault, and an unbounded loop of spawning
//! sandboxed processes is itself a denial of service. A failure to *start*
//! counts the same as a failure while running, so a missing binary or a kernel
//! that cannot confine the process also ends in software within nine attempts,
//! never in a retry loop.
//!
//! # Untouched state
//!
//! The session holds nothing the rest of the browser depends on. It has no
//! `ChannelId` in the broker's table, no frame, no tab, and does not take the
//! broker as an argument, so there is nothing a dead GPU process could leave
//! half-updated. The tests assert this against a real `Broker` rather than
//! rely on the absence.

use std::collections::VecDeque;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use std::sync::Arc;

use px_ipc::gpu::{
    GlyphChunk, GpuFailure, GpuRequest, GpuResponse, ItemChunk, MAX_DISPLAY_GLYPHS,
    MAX_DISPLAY_ITEMS, MAX_FONT_BYTES, MAX_FONTS, MAX_GLYPHS_PER_MESSAGE, MAX_ITEMS_PER_MESSAGE,
    MAX_TOTAL_FONT_BYTES,
};
use px_ipc::gpu_fonts::font_requests;
use px_ipc::{Channel, IpcError};
use px_paint::raster::MAX_CLIP_DEPTH;
use px_paint::{Canvas, DisplayList, Item};
use px_sandbox::{Rung, SandboxedChild};

use crate::fonts::FontSet;
use crate::rasterizer::{Rasterizer, RenderError, SoftwareRasterizer};
use crate::wire::{self, FrameAssembler};

/// Replacements allowed over a session's life; mirrors `ContentProcess`.
pub const DEFAULT_MAX_RESTARTS: u32 = 8;
/// How long one frame may take, request to last strip.
pub const DEFAULT_FRAME_DEADLINE: Duration = Duration::from_secs(10);
/// How long a new process may take to confine itself and say so.
pub const DEFAULT_STARTUP_DEADLINE: Duration = Duration::from_secs(10);

/// Messages the reader thread may hold unread. A strip is at most a quarter of a
/// megabyte, so this bounds what a flooding process can park in the broker.
const QUEUE_DEPTH: usize = 4;
/// Events kept; the oldest are dropped.
const EVENT_LOG_CAP: usize = 64;

/// How a session is set up.
#[derive(Clone, Debug)]
pub struct GpuConfig {
    /// The `px-gpu` binary, or `None` for software only (`--disable-gpu`):
    /// no process is ever spawned.
    pub executable: Option<PathBuf>,
    /// Per-frame deadline.
    pub frame_deadline: Duration,
    /// Per-startup deadline.
    pub startup_deadline: Duration,
    /// Replacements allowed over the session's life.
    pub max_restarts: u32,
}

impl GpuConfig {
    /// Software only. `--disable-gpu` selects this and nothing is spawned.
    pub fn software_only() -> Self {
        Self {
            executable: None,
            frame_deadline: DEFAULT_FRAME_DEADLINE,
            startup_deadline: DEFAULT_STARTUP_DEADLINE,
            max_restarts: DEFAULT_MAX_RESTARTS,
        }
    }

    /// Use the GPU process at `executable`.
    pub fn process(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: Some(executable.into()),
            ..Self::software_only()
        }
    }
}

/// Which renderer produced a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The GPU process.
    Gpu,
    /// The in-process software rasterizer.
    Software,
}

/// Why a GPU process was given up on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The sandbox refused to create the process, or it could not be created.
    SpawnRefused(String),
    /// The process did not say it was ready in time.
    StartupTimedOut,
    /// The kernel does not confirm the process is confined.
    NotConfined(String),
    /// The channel closed: the process exited, crashed or was killed.
    Exited,
    /// No complete frame within the deadline.
    TimedOut,
    /// Bytes that are not a valid message.
    Malformed,
    /// A length prefix over the message ceiling.
    Oversized,
    /// A well-formed message that the exchange does not allow here.
    Protocol(&'static str),
    /// The process reported it could not produce the frame.
    Reported(GpuFailure),
    /// The request could not be written.
    WriteFailed,
}

/// What the session did, for the log and for tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuEvent {
    /// Software only by configuration; nothing will be spawned.
    Disabled,
    /// A process started, was confirmed confined by the kernel, and is in use.
    Started {
        /// The rungs the kernel confirmed (not the process's own claim).
        applied: Vec<Rung>,
        /// The Landlock ABI the process *reported*. Unverified.
        reported_landlock_abi: u32,
    },
    /// A process was lost or refused; the frame in hand went to software.
    Failed(Cause),
    /// The restart bound was exceeded; software for the rest of the session.
    GaveUp {
        /// Replacements that were made.
        restarts: u32,
    },
}

/// Why [`GpuSession::register_font`] refused a font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontRegisterError {
    /// Empty, or longer than [`MAX_FONT_BYTES`].
    BadLength,
    /// The id is already registered.
    Duplicate,
    /// More than [`MAX_FONTS`] fonts or [`MAX_TOTAL_FONT_BYTES`] bytes.
    OverCap,
}

/// A font the session will send to any process that needs it, and uses itself
/// for the software fallback.
struct RegisteredFont {
    id: u32,
    bytes: Arc<[u8]>,
}

struct GpuProcess {
    /// Fonts this process has been sent, by id. Fonts are per process: a
    /// replacement starts empty and is sent what its first frame needs.
    sent: Vec<u32>,
    child: SandboxedChild,
    inbound: Receiver<Result<GpuResponse, IpcError>>,
    outbound: SyncSender<Vec<u8>>,
    applied: Vec<Rung>,
    reported_landlock_abi: u32,
}

/// One frame to send: what [`GpuProcess::exchange`] needs besides the fonts.
struct FrameRequest<'a> {
    serial: u32,
    list: &'a DisplayList,
    width: u32,
    height: u32,
    deadline: Instant,
}

/// The distinct fonts a list's glyph runs name, in first-use order.
fn fonts_needed(list: &DisplayList) -> Vec<u32> {
    let mut needed = Vec::new();
    for item in &list.items {
        if let Item::GlyphRun { font, .. } = item
            && !needed.contains(&font.0)
        {
            needed.push(font.0);
        }
    }
    needed
}

fn from_ipc(error: IpcError) -> Cause {
    match error {
        IpcError::PeerClosed => Cause::Exited,
        IpcError::MessageTooLarge { .. } => Cause::Oversized,
        IpcError::Malformed | IpcError::Poisoned | IpcError::Io(_) => Cause::Malformed,
    }
}

impl GpuProcess {
    /// Create the process under the policy, wait for it to say it is ready, and
    /// ask the kernel whether it really is confined. Nothing derived from a
    /// page is sent before the last of those.
    fn start(config: &GpuConfig, executable: &std::path::Path) -> Result<Self, Cause> {
        let mut child = px_sandbox::spawn_with(executable, &px_sandbox::Policy::gpu())
            .map_err(|error| Cause::SpawnRefused(error.to_string()))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| Cause::SpawnRefused("no stdout pipe".to_owned()))?;
        let stdin = child
            .take_stdin()
            .ok_or_else(|| Cause::SpawnRefused("no stdin pipe".to_owned()))?;

        // Reader: the only thing that ever waits on the child's output, so the
        // session itself never blocks on it. Bounded, so a flood parks at most
        // QUEUE_DEPTH messages and then the thread blocks, and with it the
        // child's writes.
        let (request_tx, inbound) = sync_channel::<Result<GpuResponse, IpcError>>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut channel = Channel::new(BufReader::new(stdout), std::io::sink());
            loop {
                let message = channel.recv::<GpuResponse>();
                let fatal = message.is_err();
                if request_tx.send(message).is_err() || fatal {
                    return;
                }
            }
        });

        // Writer: owns the write half behind a bounded queue of frames that
        // were encoded, and so size-checked, by the caller.
        let (outbound, frame_rx) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut writer = std::io::BufWriter::new(stdin);
            while let Ok(frame) = frame_rx.recv() {
                if px_ipc::write_frame(&mut writer, &frame).is_err() {
                    return;
                }
            }
        });

        let mut process = Self {
            sent: Vec::new(),
            child,
            inbound,
            outbound,
            applied: Vec::new(),
            reported_landlock_abi: 0,
        };

        let deadline = Instant::now() + config.startup_deadline;
        let ready = match process.recv(deadline) {
            Err(Cause::TimedOut) => return Err(Cause::StartupTimedOut),
            other => other?,
        };
        let GpuResponse::Ready { landlock_abi } = ready else {
            return Err(Cause::Protocol("the first message was not Ready"));
        };
        process.reported_landlock_abi = landlock_abi;

        // The word of the process is not evidence. The kernel's account of it is.
        process.applied = process
            .child
            .verify_gpu_profile()
            .map_err(|error| Cause::NotConfined(error.to_string()))?;
        Ok(process)
    }

    fn recv(&self, deadline: Instant) -> Result<GpuResponse, Cause> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.inbound.recv_timeout(wait) {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(error)) => Err(from_ipc(error)),
            Err(RecvTimeoutError::Timeout) => Err(Cause::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(Cause::Exited),
        }
    }

    /// Queue one encoded frame, waiting no longer than the deadline. While the
    /// queue is full, look for an early answer: a process that has already said
    /// it failed will have stopped reading, and waiting for room would be
    /// waiting out the deadline for nothing.
    fn send(&self, request: &GpuRequest, deadline: Instant) -> Result<(), Cause> {
        let mut frame = px_ipc::encode_frame(request)
            .map_err(|_| Cause::Protocol("a request could not be framed"))?;
        loop {
            match self.outbound.try_send(frame) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    // The write half is gone because the process stopped
                    // reading, usually because it said why and exited. Give
                    // that explanation a moment to arrive: it is what tells a
                    // refused font from a crash.
                    let grace = Instant::now() + Duration::from_millis(500);
                    return match self.recv(grace.min(deadline)) {
                        Ok(GpuResponse::Failed { why }) => Err(Cause::Reported(why)),
                        _ => Err(Cause::WriteFailed),
                    };
                }
                Err(TrySendError::Full(back)) => {
                    frame = back;
                    match self.inbound.try_recv() {
                        Ok(Ok(GpuResponse::Failed { why })) => return Err(Cause::Reported(why)),
                        Ok(Ok(_)) => return Err(Cause::Protocol("unsolicited message")),
                        Ok(Err(error)) => return Err(from_ipc(error)),
                        Err(_) => {}
                    }
                    if Instant::now() >= deadline {
                        return Err(Cause::TimedOut);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// One whole exchange: send the list, receive and reassemble the frame.
    ///
    /// Fonts the list needs and this process lacks go first, between frames as
    /// far as the process is concerned. Each one that was completely sent is
    /// pushed to `fresh`, so that if the process answers that a font is
    /// malformed the session knows which fonts were the suspects.
    fn exchange(
        &mut self,
        registry: &[RegisteredFont],
        frame: FrameRequest<'_>,
        fresh: &mut Vec<u32>,
    ) -> Result<Canvas, Cause> {
        let FrameRequest {
            serial,
            list,
            width,
            height,
            deadline,
        } = frame;
        let item_count =
            u32::try_from(list.items.len()).map_err(|_| Cause::Protocol("too many items"))?;
        let glyph_count =
            u32::try_from(list.glyphs.len()).map_err(|_| Cause::Protocol("too many glyphs"))?;
        for id in fonts_needed(list) {
            if self.sent.contains(&id) {
                continue;
            }
            let font = registry
                .iter()
                .find(|font| font.id == id)
                .ok_or(Cause::Protocol("a run names an unregistered font"))?;
            let requests = font_requests(id, &font.bytes)
                .ok_or(Cause::Protocol("a font could not be chunked"))?;
            for request in requests {
                self.send(&request, deadline)?;
            }
            self.sent.push(id);
            fresh.push(id);
        }
        self.send(
            &GpuRequest::Begin {
                serial,
                width,
                height,
                item_count,
                glyph_count,
            },
            deadline,
        )?;
        for chunk in list.glyphs.chunks(MAX_GLYPHS_PER_MESSAGE) {
            let wire_glyphs: Vec<_> = chunk.iter().map(wire::glyph_to_wire).collect();
            let glyphs = GlyphChunk::new(wire_glyphs)
                .map_err(|_| Cause::Protocol("a chunk exceeded its bound"))?;
            self.send(&GpuRequest::Glyphs { glyphs }, deadline)?;
        }
        for chunk in list.items.chunks(MAX_ITEMS_PER_MESSAGE) {
            let wire_items: Vec<_> = chunk.iter().map(wire::item_to_wire).collect();
            let items = ItemChunk::new(wire_items)
                .map_err(|_| Cause::Protocol("a chunk exceeded its bound"))?;
            self.send(&GpuRequest::Items { items }, deadline)?;
        }
        self.send(&GpuRequest::Draw, deadline)?;
        self.receive_frame(serial, width, height, deadline)
    }

    fn receive_frame(
        &self,
        serial: u32,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> Result<Canvas, Cause> {
        match self.recv(deadline)? {
            GpuResponse::FrameHeader {
                serial: echoed,
                width: w,
                height: h,
            } => {
                if echoed != serial {
                    return Err(Cause::Protocol("the header answers a different request"));
                }
                if (w, h) != (width, height) {
                    return Err(Cause::Protocol("the header is for a different size"));
                }
            }
            GpuResponse::Failed { why } => return Err(Cause::Reported(why)),
            _ => return Err(Cause::Protocol("expected a frame header")),
        }

        let mut assembler = FrameAssembler::new(width, height)
            .map_err(|_| Cause::Protocol("the frame is not a size the protocol carries"))?;
        while assembler.expected().is_some() {
            match self.recv(deadline)? {
                GpuResponse::Strip {
                    first_row,
                    rows,
                    pixels,
                } => assembler
                    .push(first_row, rows, pixels.as_slice())
                    .map_err(|_| Cause::Protocol("a strip is not the next valid strip"))?,
                GpuResponse::Failed { why } => return Err(Cause::Reported(why)),
                _ => return Err(Cause::Protocol("expected a strip")),
            }
        }

        // The process was asked for one frame and has sent it. Anything already
        // waiting is something nobody asked for.
        if self.inbound.try_recv().is_ok() {
            return Err(Cause::Protocol("more was sent than was asked for"));
        }
        assembler
            .finish()
            .map_err(|_| Cause::Protocol("the frame is incomplete"))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for GpuProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The supervisor. See the module documentation.
pub struct GpuSession {
    config: GpuConfig,
    process: Option<GpuProcess>,
    software: SoftwareRasterizer,
    /// Fonts the broker registered; sent to a process on first use.
    registry: Vec<RegisteredFont>,
    registry_bytes: u64,
    /// The same fonts, parsed here, for the software fallback. A font that
    /// does not parse is simply absent: its runs draw nothing, in software and
    /// (once the process has refused it) everywhere.
    local: FontSet,
    /// Fonts a process refused as malformed. Never sent again; a list that
    /// names one is rendered in software.
    rejected: Vec<u32>,
    /// Failures of any kind, over the session's life.
    failures: u32,
    /// Processes created, over the session's life.
    spawned: u32,
    given_up: bool,
    serial: u32,
    last: Backend,
    events: VecDeque<GpuEvent>,
}

impl GpuSession {
    /// A session. Spawns nothing: the process is started by the first frame
    /// that wants it, or by [`Self::warm_up`].
    pub fn new(config: GpuConfig) -> Self {
        let mut session = Self {
            given_up: config.executable.is_none(),
            config,
            process: None,
            software: SoftwareRasterizer,
            registry: Vec::new(),
            registry_bytes: 0,
            local: FontSet::new(),
            rejected: Vec::new(),
            failures: 0,
            spawned: 0,
            serial: 0,
            last: Backend::Software,
            events: VecDeque::new(),
        };
        if session.given_up {
            session.record(GpuEvent::Disabled);
        }
        session
    }

    /// Register a font under `id`, the number display lists use in
    /// [`px_paint::Item::GlyphRun`]. The same caps as the wire's apply (the
    /// process would refuse more). The bytes are not validated here: a font
    /// that does not parse is exactly what the process boundary is for, and the
    /// session finds out from the process (see [`Cause::Reported`]) and stops
    /// sending it. The session parses its own copy for the software fallback.
    pub fn register_font(
        &mut self,
        id: u32,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<(), FontRegisterError> {
        let bytes: Arc<[u8]> = bytes.into();
        let len = u32::try_from(bytes.len()).map_err(|_| FontRegisterError::BadLength)?;
        if len == 0 || len > MAX_FONT_BYTES {
            return Err(FontRegisterError::BadLength);
        }
        if self.registry.iter().any(|font| font.id == id) {
            return Err(FontRegisterError::Duplicate);
        }
        if self.registry.len() >= MAX_FONTS
            || self.registry_bytes.saturating_add(u64::from(len)) > MAX_TOTAL_FONT_BYTES
        {
            return Err(FontRegisterError::OverCap);
        }
        self.registry_bytes += u64::from(len);
        // Best effort: a font that does not parse is not drawn.
        let _ = self.local.insert(id, Arc::clone(&bytes));
        self.registry.push(RegisteredFont { id, bytes });
        Ok(())
    }

    /// Fonts a process has refused as malformed, in the order refused.
    pub fn rejected_fonts(&self) -> &[u32] {
        &self.rejected
    }

    /// Start the process now rather than at the first frame. A no-op in
    /// software mode.
    pub fn warm_up(&mut self) {
        self.ensure_process();
    }

    /// Render `list` at `width` by `height`: in the GPU process if there is a
    /// confirmed, healthy one, and otherwise (or on any failure of it) in
    /// software, with the same result.
    ///
    /// The `Err` cases are the input's: a viewport or clip structure no backend
    /// can render. They are not GPU failures, do not count against the restart
    /// bound, and do not touch the process.
    pub fn render(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Canvas, RenderError> {
        if self.gpu_can_take(list, width, height) {
            self.ensure_process();
            let serial = self.next_serial();
            let deadline = Instant::now() + self.config.frame_deadline;
            let mut fresh = Vec::new();
            let outcome = self.process.as_mut().map(|process| {
                let frame = FrameRequest {
                    serial,
                    list,
                    width,
                    height,
                    deadline,
                };
                process.exchange(&self.registry, frame, &mut fresh)
            });
            match outcome {
                Some(Ok(canvas)) => {
                    self.last = Backend::Gpu;
                    return Ok(canvas);
                }
                Some(Err(cause)) => {
                    // The process said a font did not parse. The fonts it was
                    // sent for this frame are the suspects (at most those the
                    // frame needed and it lacked); none is sent again.
                    if cause == Cause::Reported(GpuFailure::BadFont) {
                        self.rejected.extend(fresh);
                    }
                    self.fail(cause);
                }
                None => {}
            }
        }
        self.last = Backend::Software;
        self.software.rasterize(list, width, height, &self.local)
    }

    /// Which renderer produced the most recent frame.
    pub fn last_backend(&self) -> Backend {
        self.last
    }

    /// Whether a confirmed GPU process is currently held.
    pub fn has_process(&self) -> bool {
        self.process.is_some()
    }

    /// Whether this session will only ever render in software: by
    /// configuration, or because the restart bound was exceeded.
    pub fn is_software_only(&self) -> bool {
        self.given_up
    }

    /// Replacement processes created so far: spawns after the first.
    pub fn restarts(&self) -> u32 {
        self.spawned.saturating_sub(1)
    }

    /// The process's id, if one is held. A number, not an authority.
    pub fn process_id(&self) -> Option<u32> {
        self.process.as_ref().map(|process| process.child.id())
    }

    /// The rungs the kernel confirmed for the held process.
    pub fn applied(&self) -> Option<&[Rung]> {
        self.process
            .as_ref()
            .map(|process| process.applied.as_slice())
    }

    /// The Landlock ABI the held process says it enforced. Unverified.
    pub fn reported_landlock_abi(&self) -> Option<u32> {
        self.process
            .as_ref()
            .map(|process| process.reported_landlock_abi)
    }

    /// What has happened, oldest first (bounded).
    pub fn events(&self) -> impl Iterator<Item = &GpuEvent> {
        self.events.iter()
    }

    /// Testing only: kill the held process without telling the session, as an
    /// external `SIGKILL` would. The next frame finds out.
    #[cfg(feature = "testing")]
    pub fn kill_process_for_test(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }

    /// Testing only: make the held process misbehave during its next frame.
    /// Starts one first if none is held. `false` if there is none to arm.
    #[cfg(feature = "testing")]
    pub fn arm_fault(&mut self, fault: px_ipc::gpu::GpuFault) -> bool {
        self.ensure_process();
        let deadline = Instant::now() + self.config.frame_deadline;
        self.process
            .as_ref()
            .is_some_and(|process| process.send(&GpuRequest::Arm { fault }, deadline).is_ok())
    }

    /// Testing only: make the held process attempt the forbidden operations
    /// and report what the kernel answered. This is evidence about its
    /// behaviour under the profile, never about its authority.
    #[cfg(feature = "testing")]
    pub fn probe(&mut self) -> Result<Vec<px_ipc::gpu::ProbeAttempt>, Cause> {
        self.ensure_process();
        let deadline = Instant::now() + self.config.frame_deadline;
        let Some(process) = self.process.as_ref() else {
            return Err(Cause::SpawnRefused("no process".to_owned()));
        };
        let result = process.send(&GpuRequest::Probe, deadline).and_then(|()| {
            match process.recv(deadline)? {
                GpuResponse::Probe { attempts } => Ok(attempts.into_vec()),
                _ => Err(Cause::Protocol("expected a probe report")),
            }
        });
        match result {
            Ok(attempts) => Ok(attempts),
            Err(cause) => {
                self.fail(cause.clone());
                Err(cause)
            }
        }
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Whether this input is one the GPU path can carry. Inputs it cannot
    /// carry, and inputs no backend can render, go straight to software: that
    /// is not a GPU failure.
    fn gpu_can_take(&self, list: &DisplayList, width: u32, height: u32) -> bool {
        if self.given_up || !wire::viewport_ok(width, height) {
            return false;
        }
        let Ok(count) = u32::try_from(list.items.len()) else {
            return false;
        };
        if count > MAX_DISPLAY_ITEMS || !list.clips_balanced() {
            return false;
        }
        // Glyph runs the wire can carry: a well-formed arena, and only fonts
        // that are registered and have not been refused. Anything else is
        // software's to draw (or refuse), with the same answer.
        let glyphs_fit = u32::try_from(list.glyphs.len()).is_ok_and(|n| n <= MAX_DISPLAY_GLYPHS);
        if !glyphs_fit || list.validate().is_err() {
            return false;
        }
        let fonts_ok = fonts_needed(list).iter().all(|id| {
            !self.rejected.contains(id) && self.registry.iter().any(|font| font.id == *id)
        });
        if !fonts_ok {
            return false;
        }
        let mut depth = 0usize;
        for item in &list.items {
            match item {
                Item::PushClip { .. } => depth += 1,
                Item::PopClip => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth > MAX_CLIP_DEPTH {
                return false;
            }
        }
        true
    }

    fn ensure_process(&mut self) {
        if self.process.is_some() || self.given_up {
            return;
        }
        let Some(executable) = self.config.executable.clone() else {
            return;
        };
        self.spawned = self.spawned.saturating_add(1);
        match GpuProcess::start(&self.config, &executable) {
            Ok(process) => {
                self.record(GpuEvent::Started {
                    applied: process.applied.clone(),
                    reported_landlock_abi: process.reported_landlock_abi,
                });
                self.process = Some(process);
            }
            Err(cause) => self.fail(cause),
        }
    }

    /// Drop the process, record why, and give up for good if the bound is spent.
    fn fail(&mut self, cause: Cause) {
        // Dropping kills and reaps it.
        self.process = None;
        self.record(GpuEvent::Failed(cause));
        self.failures = self.failures.saturating_add(1);
        if self.failures > self.config.max_restarts {
            self.given_up = true;
            let restarts = self.restarts();
            self.record(GpuEvent::GaveUp { restarts });
        }
    }

    fn record(&mut self, event: GpuEvent) {
        if self.events.len() >= EVENT_LOG_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}
