//! Command-line entry point. Until the window exists this runs headless: it
//! listens, logs what a phone sends and can write the raw stream to a file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn};

use nearscreen_receiver::config::Config;
use nearscreen_receiver::net::{
    hostname, AllowAll, Codec, Server, ServerEvent, ServerOptions, DEFAULT_PORT,
};

/// How often the running statistics are printed.
const REPORT_EVERY: Duration = Duration::from_secs(2);

#[derive(Parser, Debug)]
#[command(
    name = "nearscreen-receiver",
    version,
    about = "Shows an iPhone screen streamed over your own network"
)]
struct Cli {
    /// Port to listen on (default: the settings file, else 9913)
    #[arg(long)]
    port: Option<u16>,

    /// Write the received Annex-B stream to this file (playable with ffplay)
    #[arg(long, value_name = "FILE")]
    dump: Option<PathBuf>,

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

    /// Log every record, not just the interesting ones
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let config = Config::load();
    let codec = Codec::parse(&cli.codec)
        .with_context(|| format!("unknown codec {:?} — use h264 or hevc", cli.codec))?;
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
    let server = Server::start(options, Arc::new(AllowAll), events_tx)?;
    let port = server.local_addr().port();
    info!("listening on port {port} as \"{name}\", asking for {codec}");
    if port != DEFAULT_PORT {
        info!("this is not the default port {DEFAULT_PORT} — the phone needs it in its settings");
    }

    let mut dump = match &cli.dump {
        Some(path) => {
            let file = File::create(path)
                .with_context(|| format!("cannot write to {}", path.display()))?;
            info!("writing the video stream to {}", path.display());
            Some(BufWriter::new(file))
        }
        None => None,
    };

    let mut stats = Stats::new();
    loop {
        match events_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => handle(event, &mut dump, &mut stats)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if stats.due() {
            stats.report();
            if let Some(dump) = dump.as_mut() {
                dump.flush().ok();
            }
        }
    }
    Ok(())
}

fn handle(event: ServerEvent, dump: &mut Option<BufWriter<File>>, stats: &mut Stats) -> Result<()> {
    match event {
        ServerEvent::SessionStarted { hello, handle, .. } => {
            info!(
                "streaming from {} ({})",
                hello.display_name(),
                hello.short_id()
            );
            // Start on a keyframe rather than waiting up to two seconds for one.
            if let Err(e) = handle.request_keyframe() {
                warn!("cannot ask for a keyframe: {e}");
            }
            stats.reset();
        }
        ServerEvent::Video { keyframe, data, .. } => {
            stats.count(data.len(), keyframe);
            if let Some(dump) = dump.as_mut() {
                dump.write_all(&data)
                    .context("cannot write the dump file")?;
            }
        }
        ServerEvent::SessionEnded { reason, .. } => {
            info!("waiting for a phone again ({reason})");
            if let Some(dump) = dump.as_mut() {
                dump.flush().ok();
            }
            stats.reset();
        }
        ServerEvent::StreamConfig { .. } | ServerEvent::Log { .. } => {} // Already logged.
        ServerEvent::Stats { json, .. } => info!("phone stats: {json}"),
        ServerEvent::Refused { peer, reason } => info!("[{peer}] turned away: {reason}"),
    }
    Ok(())
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

    fn report(&mut self) {
        let seconds = self.since.elapsed().as_secs_f64();
        if self.frames > 0 && seconds > 0.0 {
            info!(
                "{:.1} fps  {:.2} Mbit/s  {} keyframes",
                f64::from(self.frames) / seconds,
                self.bytes as f64 * 8.0 / seconds / 1e6,
                self.keyframes
            );
        }
        self.since = Instant::now();
        self.frames = 0;
        self.keyframes = 0;
        self.bytes = 0;
    }
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_millis()
        .init();
}
