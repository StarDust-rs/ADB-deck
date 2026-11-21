//! One mirroring session: starts the scrcpy server on the phone, accepts its
//! connections, decodes the video and carries input back.
//!
//! Connection set-up is the one ADBPlus and scrcpy use. The server jar is
//! pushed to the device and started with `app_process`; it connects *to us*
//! through `adb reverse localabstract:scrcpy_<scid> tcp:<port>`, first the
//! video socket, then (if enabled) the control socket.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::decode::{Frame, VideoDecoder};
use super::proto::{self, Control, DeviceMessage, PacketHeader, ServerOptions, VideoHeader};
use crate::adb::DeviceAdb;
use crate::error::{Error, Result};
use crate::jobs::Waker;
use crate::process::{spawn_lines, LineStream};
use crate::retry::{self, Backoff};

/// The server jar, version 3.1 of scrcpy (Apache-2.0), see `assets/scrcpy/NOTICE.md`.
pub const SERVER_JAR: &[u8] = include_bytes!("../../assets/scrcpy/scrcpy-server-v3.1.jar");
const DEVICE_DIR: &str = "/data/local/tmp/adbdeck";
const DEVICE_JAR: &str = "/data/local/tmp/adbdeck/scrcpy-server.jar";
const ACCEPT_VIDEO: Duration = Duration::from_secs(20);
const ACCEPT_CONTROL: Duration = Duration::from_secs(8);
const HANDSHAKE: Duration = Duration::from_secs(10);
/// Pictures waiting for the decoder before late delta frames are dropped.
const DECODE_QUEUE: usize = 24;

/// What a running session reports to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The stream is up.
    Started { device_name: String, width: u32, height: u32 },
    /// Text the device put on its clipboard.
    Clipboard(String),
    /// The session is over; `Some` carries why when it was not the user's doing.
    Ended(Option<String>),
}

pub struct Session {
    pub serial: String,
    frames: Arc<Mutex<Option<Frame>>>,
    visible: Arc<AtomicBool>,
    control: mpsc::UnboundedSender<Control>,
    events: mpsc::UnboundedReceiver<Event>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Session {
    pub fn start(dev: DeviceAdb, opts: ServerOptions, scratch: &Path, handle: &Handle, wake: Waker) -> Session {
        let serial = dev.serial().to_string();
        let frames = Arc::new(Mutex::new(None));
        let visible = Arc::new(AtomicBool::new(true));
        let (control, control_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (stop, stop_rx) = oneshot::channel();
        let ctx = Run {
            dev,
            opts,
            scratch: scratch.to_path_buf(),
            frames: frames.clone(),
            visible: visible.clone(),
            control: control.clone(),
            control_rx,
            events: events_tx.clone(),
            wake: wake.clone(),
        };
        let task = handle.spawn(async move {
            let ended = match run(ctx, stop_rx).await {
                Ok(()) => None,
                Err(e) => Some(e.to_string()),
            };
            let _ = events_tx.send(Event::Ended(ended));
            wake();
        });
        Session { serial, frames, visible, control, events, stop: Some(stop), task }
    }

    /// While the picture is not on screen the stream keeps decoding but no
    /// pictures are produced, so a hidden mirror costs little.
    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
    }

    /// The newest decoded picture, once; older ones are never shown.
    pub fn take_frame(&self) -> Option<Frame> {
        self.frames.lock().ok()?.take()
    }

    pub fn send(&self, message: Control) {
        let _ = self.control.send(message);
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Ends the session; the end is reported through [`Event::Ended`].
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Stops and waits briefly for the device side to be cleaned up (used at exit).
    pub fn finish(&mut self, rt: &tokio::runtime::Runtime) {
        self.stop();
        let task = &mut self.task;
        let _ = rt.block_on(tokio::time::timeout(Duration::from_secs(4), task));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Run {
    dev: DeviceAdb,
    opts: ServerOptions,
    scratch: std::path::PathBuf,
    frames: Arc<Mutex<Option<Frame>>>,
    visible: Arc<AtomicBool>,
    control: mpsc::UnboundedSender<Control>,
    control_rx: mpsc::UnboundedReceiver<Control>,
    events: mpsc::UnboundedSender<Event>,
    wake: Waker,
}

/// Keeps the last lines the server printed; they explain a failed start.
#[derive(Default)]
struct Tail(VecDeque<String>);

impl Tail {
    fn push(&mut self, line: String) {
        tracing::debug!(target: "adbdeck::mirror::server", "{line}");
        if self.0.len() == 6 {
            self.0.pop_front();
        }
        self.0.push_back(line);
    }

    fn drain(&mut self, server: &mut LineStream) {
        while let Ok(line) = server.lines.try_recv() {
            self.push(line);
        }
    }

    fn text(&self) -> String {
        self.0.iter().cloned().collect::<Vec<_>>().join(" / ")
    }
}

/// Removes the reverse tunnel and ends the server when the session is over,
/// whichever way that happens (including the task being aborted).
struct Cleanup {
    dev: DeviceAdb,
    socket: String,
    scid: u32,
    handle: Handle,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let (dev, socket, scid) = (self.dev.clone(), self.socket.clone(), self.scid);
        self.handle.spawn(async move {
            let _ = dev.remove_reverse(&format!("localabstract:{socket}")).await;
            // The server ends itself when its sockets close; this covers a stuck one. The
            // bracket keeps the pattern from matching the shell that carries it.
            let hex = format!("{scid:08x}");
            let (head, last) = hex.split_at(7);
            let _ = dev.shell(&format!("pkill -f 'Server {} scid={head}[{last}]'", proto::SERVER_VERSION)).await;
        });
    }
}

async fn run(mut ctx: Run, stop: oneshot::Receiver<()>) -> Result<()> {
    let socket = ctx.opts.socket_name();
    let handle = Handle::current();

    // 1. Put the server on the device.
    tokio::fs::create_dir_all(&ctx.scratch).await.map_err(|e| Error::io(format!("creating {}", ctx.scratch.display()), e))?;
    let local = ctx.scratch.join("scrcpy-server-v3.1.jar");
    tokio::fs::write(&local, SERVER_JAR).await.map_err(|e| Error::io(format!("writing {}", local.display()), e))?;
    ctx.dev.shell_ok(&format!("mkdir -p {DEVICE_DIR}")).await?;
    retry::retry(
        Backoff::QUICK,
        |n, e| tracing::warn!("copying the scrcpy server failed (try {n}): {e}"),
        || async {
            let out = ctx.dev.exec([std::ffi::OsString::from("push"), local.clone().into_os_string(), DEVICE_JAR.into()]).await?;
            if out.success() {
                Ok(())
            } else {
                Err(Error::Exit { program: "adb push".into(), code: out.code, message: out.failure_message() })
            }
        },
    )
    .await
    .map_err(|e| Error::Invalid(format!("could not copy the screen server to the device: {e}")))?;

    // 2. Listen, and make the device's abstract socket lead here.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| Error::io("listening for the device", e))?;
    let port = listener.local_addr().map_err(|e| Error::io("listening for the device", e))?.port();
    let _cleanup = Cleanup { dev: ctx.dev.clone(), socket: socket.clone(), scid: ctx.opts.scid, handle };
    ctx.dev.reverse(&format!("localabstract:{socket}"), &format!("tcp:{port}")).await?;

    // 3. Start the server.
    let command = format!("CLASSPATH={DEVICE_JAR} app_process / {} {}", proto::SERVER_CLASS, ctx.opts.args().join(" "));
    let mut server = spawn_lines(&ctx.dev.spec().arg("shell").arg(command), 64, true)?;
    let mut tail = Tail::default();

    // 4. The device connects: video first, then control.
    let video = accept(&listener, &mut server, &mut tail, ACCEPT_VIDEO, "video").await?;
    let control = if ctx.opts.control { Some(accept(&listener, &mut server, &mut tail, ACCEPT_CONTROL, "control").await?) } else { None };
    drop(listener);

    // 5. The first socket starts with the device name, then the codec header.
    let mut video = video;
    let (device_name, header) = tokio::time::timeout(HANDSHAKE, async {
        let mut name = [0u8; proto::DEVICE_NAME_LEN];
        video.read_exact(&mut name).await.map_err(|e| Error::io("reading the device name", e))?;
        let mut head = [0u8; VideoHeader::LEN];
        video.read_exact(&mut head).await.map_err(|e| Error::io("reading the video header", e))?;
        Ok::<_, Error>((proto::parse_device_name(&name), VideoHeader::parse(&head)?))
    })
    .await
    .map_err(|_| Error::Invalid(format!("the device connected but sent no video{}", describe(&tail))))??;
    let _ = ctx.events.send(Event::Started { device_name, width: header.width, height: header.height });
    (ctx.wake)();

    // 6. Run the stream until something ends it.
    let (packets_tx, packets_rx) = mpsc::channel::<Packet>(DECODE_QUEUE);
    let decoder = spawn_decoder(packets_rx, ctx.frames.clone(), ctx.visible.clone(), ctx.control.clone(), ctx.wake.clone())?;
    let mut video_task = tokio::spawn(read_video(video, packets_tx, ctx.control.clone()));
    let mut control_tasks = Vec::new();
    if let Some(stream) = control {
        let (mut read, mut write) = stream.into_split();
        let mut outgoing = std::mem::replace(&mut ctx.control_rx, mpsc::unbounded_channel().1);
        control_tasks.push(tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if write.write_all(&message.encode()).await.is_err() {
                    break;
                }
            }
        }));
        let (events, wake) = (ctx.events.clone(), ctx.wake.clone());
        control_tasks.push(tokio::spawn(async move {
            let mut pending: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            'read: loop {
                match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => pending.extend_from_slice(&chunk[..n]),
                }
                loop {
                    match proto::parse_device_message(&pending) {
                        Ok(Some((message, used))) => {
                            pending.drain(..used);
                            if let DeviceMessage::Clipboard(text) = message {
                                let _ = events.send(Event::Clipboard(text));
                                wake();
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break 'read,
                    }
                }
            }
        }));
    }

    let outcome = tokio::select! {
        _ = stop => Ok(()),
        result = &mut video_task => match result {
            Ok(Ok(())) | Err(_) => Err(Error::Invalid("the device closed the screen stream".into())),
            Ok(Err(e)) => Err(e),
        },
    };
    video_task.abort();
    for t in control_tasks {
        t.abort();
    }
    decoder.stop();
    tail.drain(&mut server);
    outcome.map_err(|e| match tail.text() {
        t if t.is_empty() => e,
        t => Error::Invalid(format!("{e}: {t}")),
    })
}

fn describe(tail: &Tail) -> String {
    match tail.text() {
        t if t.is_empty() => String::new(),
        t => format!(": {t}"),
    }
}

/// Waits for the device to connect, noticing a server that exits first.
async fn accept(listener: &TcpListener, server: &mut LineStream, tail: &mut Tail, limit: Duration, what: &str) -> Result<TcpStream> {
    let deadline = Instant::now() + limit;
    loop {
        tail.drain(server);
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|e| Error::io(format!("accepting the {what} connection"), e))?;
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if server.is_finished() {
            tail.drain(server);
            return Err(Error::Invalid(format!("the screen server stopped before it connected{}", describe(tail))));
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(format!(
                "the screen server did not connect its {what} stream within {} s{}",
                limit.as_secs(),
                describe(tail)
            )));
        }
    }
}

struct Packet {
    config: bool,
    data: Vec<u8>,
}

/// Reads packets until the stream ends. Delta frames are dropped when the decoder
/// is behind, and a new key frame is requested, so latency stays bounded.
async fn read_video(mut stream: TcpStream, packets: mpsc::Sender<Packet>, control: mpsc::UnboundedSender<Control>) -> Result<()> {
    let mut skipping = false;
    loop {
        let mut head = [0u8; PacketHeader::LEN];
        if stream.read_exact(&mut head).await.is_err() {
            return Ok(());
        }
        let header = PacketHeader::parse(&head)?;
        let mut data = vec![0u8; header.size];
        stream.read_exact(&mut data).await.map_err(|e| Error::io("reading video", e))?;
        if header.config || header.key_frame {
            skipping = false;
            if packets.send(Packet { config: header.config, data }).await.is_err() {
                return Ok(());
            }
        } else if !skipping {
            match packets.try_send(Packet { config: false, data }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    skipping = true;
                    let _ = control.send(Control::ResetVideo);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
            }
        }
    }
}

struct DecoderThread {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DecoderThread {
    /// The thread ends when the packet channel closes, which the reader's exit does.
    fn stop(mut self) {
        self.thread.take();
    }
}

fn spawn_decoder(
    mut packets: mpsc::Receiver<Packet>,
    frames: Arc<Mutex<Option<Frame>>>,
    visible: Arc<AtomicBool>,
    control: mpsc::UnboundedSender<Control>,
    wake: Waker,
) -> Result<DecoderThread> {
    let mut decoder = VideoDecoder::new()?;
    let thread = std::thread::Builder::new()
        .name("adbdeck-h264".into())
        .spawn(move || {
            let mut last_reset: Option<Instant> = None;
            while let Some(packet) = packets.blocking_recv() {
                match decoder.decode_picture(&packet.data, visible.load(Ordering::Relaxed)) {
                    Ok(Some(frame)) => {
                        if let Ok(mut slot) = frames.lock() {
                            *slot = Some(frame);
                        }
                        wake();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!("video packet skipped: {e}");
                        // A broken reference frame stays visible until the next key frame.
                        if !packet.config && last_reset.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                            last_reset = Some(Instant::now());
                            let _ = control.send(Control::ResetVideo);
                        }
                    }
                }
            }
        })
        .map_err(|e| Error::io("starting the video decoder thread", e))?;
    Ok(DecoderThread { thread: Some(thread) })
}
