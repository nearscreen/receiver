//! Command-line entry point: listen, decode, show.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use log::{error, info, warn};
use winit::event_loop::EventLoopProxy;

use nearscreen_receiver::autostart;
use nearscreen_receiver::config::Config;
use nearscreen_receiver::consent::{Answer, Ask, Consent};
use nearscreen_receiver::decode::{self, Decoder, Nv12Frame};
use nearscreen_receiver::net::{
    hostname, local_addresses, Advertisement, AllowAll, Codec, Interfaces, Server, ServerEvent,
    ServerOptions, SessionHandle, StreamConfig, DEFAULT_PORT,
};
use nearscreen_receiver::ui::{self, FrameSlot, UiEvent, WindowConfig};

/// How often the running statistics are printed.
const REPORT_EVERY: Duration = Duration::from_secs(2);

/// Never pester the phone for keyframes more often than this.
const KEYFRAME_EVERY: Duration = Duration::from_secs(1);

#[derive(Parser, Debug, Clone)]
#[command(
    name = "nearscreen-receiver",
    version,
    about = "Shows an iPhone screen streamed over your own network"
)]
struct Cli {
    /// Port to listen on (default: the settings file, else 9913)
    #[arg(long)]
    port: Option<u16>,

    /// Name shown on the phone (default: this computer's name)
    #[arg(long)]
    name: Option<String>,

    /// Frame rate to ask the phone for
    #[arg(long, default_value_t = 30.0)]
    fps: f64,

    /// Bitrate to ask the phone for, bits per second
    #[arg(long, default_value_t = 6_000_000)]
    bitrate: i64,

    /// Codec to ask the phone for: h264 or hevc
    #[arg(long, default_value = "h264")]
    codec: String,

    /// Do not announce the receiver over mDNS; the phone then needs the address
    #[arg(long)]
    no_mdns: bool,

    /// Announce on one interface only: an IP address, or "loopback"
    #[arg(long, value_name = "IP|loopback")]
    mdns_interface: Option<String>,

    /// Run without a window — for checking the network side on its own
    #[arg(long)]
    headless: bool,

    /// Write the received Annex-B stream to this file (playable with ffplay)
    #[arg(long, value_name = "FILE")]
    dump: Option<PathBuf>,

    /// Write the first decoded picture to this file, as a PPM image
    #[arg(long, value_name = "FILE")]
    save_frame: Option<PathBuf>,

    /// Log every record, not just the interesting ones
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let config = Config::load();
    let codec = choose_codec(&cli)?;
    let options = ServerOptions {
        port: cli.port.unwrap_or(config.port),
        name: cli.name.clone().unwrap_or_else(hostname),
        fps: cli.fps,
        bitrate: cli.bitrate,
        codec,
        ..ServerOptions::default()
    };
    let name = options.name.clone();
    let (events_tx, events_rx) = mpsc::channel();

    if cli.headless {
        // Nobody can be asked without a window, so nobody is.
        let server = Server::start(options, Arc::new(AllowAll), events_tx)?;
        let port = server.local_addr().port();
        announce(&cli, &config, &name, port, codec);
        info!("no window: every phone that connects is let straight in");
        let _advertisement = start_advertisement(&cli, &config, &name, port);
        return Pipeline::new(cli, None).run(&events_rx);
    }

    // The question goes to the window and the answer comes back, so that the
    // person's decision outlives the connection that prompted it.
    let (questions_tx, questions_rx) = mpsc::channel::<(String, String)>();
    let (answers_tx, answers_rx) = mpsc::channel::<(String, String, Answer)>();
    // A settings file that says "start at login" should mean it, even if it
    // was copied here from another computer.
    if config.start_at_login && !autostart::is_enabled() {
        if let Err(e) = autostart::set(true) {
            warn!("cannot add the receiver to the startup list: {e:#}");
        }
    }
    let settings = Arc::new(Mutex::new(config.clone()));
    let consent = Consent::new(settings.clone(), Box::new(AskTheWindow(questions_tx)));
    {
        let consent = consent.clone();
        thread::Builder::new()
            .name("nearscreen-consent".to_string())
            .spawn(move || {
                while let Ok((id, device, answer)) = answers_rx.recv() {
                    consent.record(&id, &device, answer);
                }
            })
            .context("cannot start the consent thread")?;
    }

    let server = Server::start(options, consent, events_tx)?;
    let port = server.local_addr().port();
    announce(&cli, &config, &name, port, codec);
    // Kept alive for as long as the receiver runs; dropping it withdraws the
    // announcement.
    let _advertisement = start_advertisement(&cli, &config, &name, port);

    let frames: FrameSlot = Arc::new(Mutex::new(None));
    let slot = frames.clone();
    let window = WindowConfig {
        name: name.clone(),
        addresses: preferred_first(local_addresses(), config.preferred_interface.as_deref()),
        port,
        settings,
    };
    ui::run(window, frames, move |proxy| {
        {
            let proxy = proxy.clone();
            let started = thread::Builder::new()
                .name("nearscreen-questions".to_string())
                .spawn(move || {
                    while let Ok((device, id)) = questions_rx.recv() {
                        let _ = proxy.send_event(UiEvent::Ask {
                            device,
                            id,
                            answer: answers_tx.clone(),
                        });
                    }
                });
            if let Err(e) = started {
                error!("cannot start the consent thread: {e}");
            }
        }

        let link = UiLink { slot, proxy };
        let started = thread::Builder::new()
            .name("nearscreen-decode".to_string())
            .spawn(move || {
                if let Err(e) = Pipeline::new(cli, Some(link)).run(&events_rx) {
                    error!("{e:#}");
                }
            });
        if let Err(e) = started {
            error!("cannot start the decoding thread: {e}");
        }
    })
}

/// Says where the receiver is listening, in the log.
fn announce(cli: &Cli, _config: &Config, name: &str, port: u16, codec: Codec) {
    info!("listening on port {port} as \"{name}\", asking for {codec}");
    if port != DEFAULT_PORT {
        info!("this is not the default port {DEFAULT_PORT} — the phone needs it in its settings");
    }
    if cli.no_mdns {
        info!("phones will need this computer's address: discovery is off");
    }
}

/// Carries the question from whichever thread is holding a phone at the door
/// to the window, which is on a thread of its own.
struct AskTheWindow(mpsc::Sender<(String, String)>);

impl Ask for AskTheWindow {
    fn ask(&self, device: &str, id: &str) {
        if self.0.send((device.to_string(), id.to_string())).is_err() {
            warn!("nobody is there to ask about {device}");
        }
    }
}

/// The codec we ask the phone for — never one this computer cannot decode.
fn choose_codec(cli: &Cli) -> Result<Codec> {
    let asked = Codec::parse(&cli.codec)
        .with_context(|| format!("unknown codec {:?} — use h264 or hevc", cli.codec))?;
    // Asking the system about its decoders puts this thread into a
    // multi-threaded COM apartment, and the window needs a single-threaded one
    // — so the question is asked on a thread that is thrown away afterwards.
    let probe = thread::spawn(move || decode::is_supported(asked));
    if probe.join().unwrap_or(false) {
        return Ok(asked);
    }
    if asked == Codec::Hevc {
        warn!(
            "this computer cannot decode HEVC (the HEVC Video Extensions are not installed); \
             asking the phone for H.264 instead"
        );
        return Ok(Codec::H264);
    }
    warn!("this computer cannot decode {asked}; the window will stay empty");
    Ok(asked)
}

/// Announces the receiver unless asked not to. A network that will not carry
/// the announcement is not fatal — the phone can still be pointed at us by
/// address — so this only warns.
fn start_advertisement(cli: &Cli, config: &Config, name: &str, port: u16) -> Option<Advertisement> {
    if cli.no_mdns {
        info!("not announcing on the network (--no-mdns)");
        return None;
    }
    let chosen = cli
        .mdns_interface
        .as_deref()
        .or(config.preferred_interface.as_deref());
    let interfaces = match chosen {
        Some(value) => match Interfaces::parse(value) {
            Ok(interfaces) => interfaces,
            Err(e) => {
                warn!("{e:#}; announcing on every interface instead");
                Interfaces::default()
            }
        },
        None => Interfaces::default(),
    };
    match Advertisement::start(name, port, &interfaces) {
        Ok(advertisement) => Some(advertisement),
        Err(e) => {
            warn!("the receiver works but phones cannot discover it: {e:#}");
            None
        }
    }
}

/// The way into the window from the decoding thread.
struct UiLink {
    slot: FrameSlot,
    proxy: EventLoopProxy<UiEvent>,
}

impl UiLink {
    fn show(&self, frame: Nv12Frame) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        let _ = self.proxy.send_event(UiEvent::Frame);
    }

    fn streaming(&self, device: String) {
        let _ = self.proxy.send_event(UiEvent::Streaming { device });
    }

    fn idle(&self) {
        let _ = self.proxy.send_event(UiEvent::Idle);
    }

    fn rate(&self, summary: String) {
        let _ = self.proxy.send_event(UiEvent::Rate { summary });
    }
}

/// What the decoder is currently set up for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Format {
    codec: Codec,
    width: u32,
    height: u32,
}

/// Everything that happens to a stream between the socket and the screen.
struct Pipeline {
    cli: Cli,
    ui: Option<UiLink>,
    decoder: Option<Box<dyn Decoder>>,
    format: Option<Format>,
    phone: Option<SessionHandle>,
    asked_for_keyframe: Option<Instant>,
    dump: Option<BufWriter<File>>,
    frame_saved: bool,
    stats: Stats,
}

impl Pipeline {
    fn new(cli: Cli, ui: Option<UiLink>) -> Self {
        Self {
            cli,
            ui,
            decoder: None,
            format: None,
            phone: None,
            asked_for_keyframe: None,
            dump: None,
            frame_saved: false,
            stats: Stats::new(),
        }
    }

    /// Runs until the server is gone. The decoder is built here and never
    /// leaves this thread — that is what the system decoders expect.
    fn run(mut self, events: &Receiver<ServerEvent>) -> Result<()> {
        if let Some(path) = self.cli.dump.clone() {
            let file = File::create(&path)
                .with_context(|| format!("cannot write to {}", path.display()))?;
            info!("writing the video stream to {}", path.display());
            self.dump = Some(BufWriter::new(file));
        }

        loop {
            match events.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => self.handle(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.stats.due() {
                let codec = self.format.as_ref().map(|format| format.codec);
                if let (Some(summary), Some(ui)) = (self.stats.report(codec), self.ui.as_ref()) {
                    ui.rate(summary);
                }
                if let Some(dump) = self.dump.as_mut() {
                    let _ = dump.flush();
                }
            }
        }
        Ok(())
    }

    fn handle(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::SessionStarted { hello, handle, .. } => {
                let device = format!("{} ({})", hello.display_name(), hello.short_id());
                info!("streaming from {device}");
                self.format = Codec::parse(&hello.codec).map(|codec| Format {
                    codec,
                    width: hello.w,
                    height: hello.h,
                });
                self.decoder = None;
                self.phone = Some(handle);
                self.stats.reset();
                if let Some(ui) = &self.ui {
                    ui.streaming(device);
                }
            }
            ServerEvent::StreamConfig { config, .. } => self.reconfigure(&config),
            ServerEvent::Video {
                keyframe,
                data,
                pts_us,
            } => {
                self.stats.count(data.len(), keyframe);
                if let Some(dump) = self.dump.as_mut() {
                    if let Err(e) = dump.write_all(&data) {
                        warn!("cannot write the dump file: {e}");
                    }
                }
                self.on_video(&data, keyframe, pts_us);
            }
            ServerEvent::SessionEnded { reason, .. } => {
                info!("waiting for a phone again ({reason})");
                self.decoder = None;
                self.phone = None;
                self.stats.reset();
                if let Some(dump) = self.dump.as_mut() {
                    let _ = dump.flush();
                }
                if let Some(ui) = &self.ui {
                    ui.idle();
                }
            }
            ServerEvent::Stats { json, .. } => info!("phone stats: {json}"),
            ServerEvent::Refused { peer, reason } => info!("[{peer}] turned away: {reason}"),
            ServerEvent::Log { .. } => {} // Already in the log.
        }
    }

    /// The phone told us what its encoder actually produces.
    fn reconfigure(&mut self, config: &StreamConfig) {
        let Some(codec) = Codec::parse(&config.codec) else {
            warn!("the phone reports an unknown codec {:?}", config.codec);
            return;
        };
        let format = Format {
            codec,
            width: config.w,
            height: config.h,
        };
        if self.format.as_ref() == Some(&format) {
            return;
        }
        info!(
            "the phone is now sending {codec} {}x{}",
            format.width, format.height
        );
        self.format = Some(format);
        // The next keyframe starts a decoder for the new shape.
        self.decoder = None;
        self.ask_for_keyframe();
    }

    fn on_video(&mut self, access_unit: &[u8], keyframe: bool, pts_us: u64) {
        if self.decoder.is_none() {
            if !keyframe {
                // A decoder that starts mid-picture produces nothing but
                // errors; wait for a keyframe and hurry it along.
                self.ask_for_keyframe();
                return;
            }
            let Some(format) = self.format.clone() else {
                return;
            };
            match decode::new_decoder(format.codec, format.width, format.height) {
                Ok(decoder) => self.decoder = Some(decoder),
                Err(e) => {
                    warn!("cannot decode this stream: {e:#}");
                    return;
                }
            }
        }

        let decoded = match self.decoder.as_mut() {
            Some(decoder) => decoder.decode(access_unit, pts_us),
            None => return,
        };
        match decoded {
            Ok(Some(frame)) => {
                self.save_first_frame(&frame);
                if let Some(ui) = &self.ui {
                    ui.show(frame);
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!("the decoder gave up on this frame: {e:#}");
                self.decoder = None;
                self.ask_for_keyframe();
            }
        }
    }

    /// Asks the phone for a keyframe, but not more than once a second — it has
    /// to re-encode a whole picture for each one.
    fn ask_for_keyframe(&mut self) {
        let Some(phone) = &self.phone else {
            return;
        };
        let recent = self
            .asked_for_keyframe
            .is_some_and(|when| when.elapsed() < KEYFRAME_EVERY);
        if recent {
            return;
        }
        self.asked_for_keyframe = Some(Instant::now());
        if let Err(e) = phone.request_keyframe() {
            warn!("cannot ask the phone for a keyframe: {e}");
        }
    }

    fn save_first_frame(&mut self, frame: &Nv12Frame) {
        let Some(path) = self.cli.save_frame.clone() else {
            return;
        };
        if self.frame_saved {
            return;
        }
        self.frame_saved = true;
        match save_ppm(frame, &path) {
            Ok(()) => info!(
                "wrote a {}x{} picture to {}",
                frame.width,
                frame.height,
                path.display()
            ),
            Err(e) => warn!("cannot write {}: {e:#}", path.display()),
        }
    }
}

/// Writes a picture as a plain PPM — no image library, and every viewer and
/// converter reads it.
fn save_ppm(frame: &Nv12Frame, path: &Path) -> Result<()> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let mut pixels = vec![0u32; width * height];
    frame.blit_fit(&mut pixels, frame.width, frame.height, 0);

    let mut out = Vec::with_capacity(width * height * 3 + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for pixel in &pixels {
        out.push((pixel >> 16) as u8);
        out.push((pixel >> 8) as u8);
        out.push(*pixel as u8);
    }
    fs::write(path, out).with_context(|| format!("cannot write {}", path.display()))
}

/// Frames and bytes since the last report.
struct Stats {
    since: Instant,
    frames: u32,
    keyframes: u32,
    bytes: u64,
}

impl Stats {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            keyframes: 0,
            bytes: 0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn count(&mut self, bytes: usize, keyframe: bool) {
        self.frames += 1;
        self.bytes += bytes as u64;
        if keyframe {
            self.keyframes += 1;
        }
    }

    fn due(&self) -> bool {
        self.since.elapsed() >= REPORT_EVERY
    }

    /// Logs how the stream is doing and returns the same in the words the
    /// overlay uses.
    fn report(&mut self, codec: Option<Codec>) -> Option<String> {
        let seconds = self.since.elapsed().as_secs_f64();
        let summary = if self.frames > 0 && seconds > 0.0 {
            let fps = f64::from(self.frames) / seconds;
            let mbits = self.bytes as f64 * 8.0 / seconds / 1e6;
            info!(
                "{fps:.1} fps  {mbits:.2} Mbit/s  {} keyframes",
                self.keyframes
            );
            Some(match codec {
                Some(codec) => format!("{fps:.0} fps · {mbits:.1} Mbit/s · {}", spoken(codec)),
                None => format!("{fps:.0} fps · {mbits:.1} Mbit/s"),
            })
        } else {
            None
        };
        self.since = Instant::now();
        self.frames = 0;
        self.keyframes = 0;
        self.bytes = 0;
        summary
    }
}

/// Puts the address from the settings file first, when it is one this computer
/// actually has — the person who set it knows which network the phone is on.
fn preferred_first(mut addresses: Vec<IpAddr>, preferred: Option<&str>) -> Vec<IpAddr> {
    let Some(preferred) = preferred.and_then(|value| value.parse::<IpAddr>().ok()) else {
        return addresses;
    };
    if let Some(index) = addresses.iter().position(|address| *address == preferred) {
        addresses.swap(0, index);
    }
    addresses
}

/// The codec, spelled the way people write it.
fn spoken(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
    }
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_millis()
        .init();
}
