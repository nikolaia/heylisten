//! The Recorder: captures a meeting's tracks to disk. It runs as its own background process
//! (see docs/adr/0003); this module also starts, stops and finds that process.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::aec::{Aec, DELAY_MAX, DELAY_MIN, estimate_delay};
use crate::audio::{self, To16k, WavWriter};
use crate::capture::{self, Mic, System};
use crate::config::{Config, meetings_dir};
use crate::live::{self, Chunker};
use crate::meeting::{Meeting, Track};

/// Captures both tracks of a meeting to WAV files while it's alive, and transcribes them live.
pub struct Recorder {
    mic: Option<Mic>,
    system: Option<System>,
    writers: Vec<JoinHandle<Result<()>>>,
    live: JoinHandle<Result<()>>,
}

impl Recorder {
    /// `debug` logs how much audio each track receives, every second.
    pub fn start(meeting: &Meeting, whisper_model: &Path, debug: bool) -> Result<Recorder> {
        let (live_tx, live) = live::spawn(whisper_model.to_path_buf(), meeting.dir())?;
        // The system track is the echo reference for the mic.
        let loopback = Arc::new(Mutex::new(Loopback::default()));
        let echo = match Aec::new() {
            Ok(aec) => {
                log("echo cancellation: on (DTLN-aec)");
                // In debug mode the mic is also kept from before cancellation, to tune it offline.
                let raw = if debug { WavWriter::create(&meeting.dir().join("mic-raw.wav")).ok() } else { None };
                Echo::Cancel(Box::new(Canceller {
                    aec,
                    raw,
                    loopback: loopback.clone(),
                    pending: Vec::new(),
                    next: 0,
                    delay: 0,
                    history: VecDeque::new(),
                    since_estimate: 0,
                    levels: Levels::new(debug),
                    debug,
                }))
            }
            Err(e) => {
                log(&format!("echo cancellation: off ({e:#})"));
                Echo::Off
            }
        };
        let (mic_tx, mic_writer) = track_writer(meeting, Track::Mic, live_tx.clone(), echo, debug)?;
        let (sys_tx, sys_writer) = track_writer(meeting, Track::System, live_tx, Echo::Reference(loopback), debug)?;
        let mic = Mic::start(Box::new(move |s, rate, at| drop(mic_tx.send((s.to_vec(), rate, at)))), debug)?;
        let system = System::start(Box::new(move |s, rate, at| drop(sys_tx.send((s.to_vec(), rate, at)))))?;
        // Always logged, so a bad recording can be traced to the device it came from.
        log(&format!("mic: {} ({} Hz, {} ch)", mic.device, mic.rate, mic.channels));
        log(&format!("system audio: tap of everything except heyListen, output device {}", system.device));
        Ok(Recorder { mic: Some(mic), system: Some(system), writers: vec![mic_writer, sys_writer], live })
    }

    /// Stops capture and waits until both files are complete and the live transcript has caught up.
    /// Returns whether the live transcript is complete.
    pub fn stop(mut self) -> Result<bool> {
        // Dropping the sources drops their senders, which ends the writer threads, which ends
        // the live transcriber once it has worked through its queue.
        drop(self.mic.take());
        drop(self.system.take());
        for w in self.writers.drain(..) {
            w.join().expect("writer thread panicked")?;
        }
        match self.live.join().expect("live transcriber panicked") {
            Ok(()) => Ok(true),
            Err(e) => {
                log(&format!("live transcript incomplete: {e:#}"));
                Ok(false)
            }
        }
    }
}

/// Samples, their rate, and when the first was captured (`capture::now_ns` clock).
type Audio = (Vec<f32>, u32, u64);

/// A source that's this late is filled with silence up to its capture time. Small enough to
/// keep the tracks aligned to the millisecond; big enough to ignore buffer timing jitter.
const MAX_GAP_NS: u64 = 10_000_000;

/// The system track's recent 16 kHz audio, by absolute sample index: what the mic may hear
/// back from the speakers.
#[derive(Default)]
struct Loopback {
    start: u64,
    samples: VecDeque<f32>,
}

impl Loopback {
    /// How much of the reference to keep for a lagging mic.
    const KEEP: usize = audio::SAMPLE_RATE as usize * 30;

    fn push(&mut self, samples: &[f32]) {
        self.samples.extend(samples);
        let excess = self.samples.len().saturating_sub(Self::KEEP);
        self.samples.drain(..excess);
        self.start += excess as u64;
    }

    fn end(&self) -> u64 {
        self.start + self.samples.len() as u64
    }

    /// Samples `[from, from + n)`, with silence for anything not (or no longer) here.
    fn take(&self, from: u64, n: usize) -> Vec<f32> {
        (from..from + n as u64)
            .map(|i| if i >= self.start { self.samples.get((i - self.start) as usize).copied().unwrap_or(0.0) } else { 0.0 })
            .collect()
    }
}

/// What a track does with its finished audio besides writing it.
enum Echo {
    /// The system track: it's the reference the mic is cleaned against.
    Reference(Arc<Mutex<Loopback>>),
    /// The mic: cancel the reference out of it first.
    Cancel(Box<Canceller>),
    Off,
}

/// Cleans the mic against the reference. `pending` is mic audio from sample `next` on, waiting
/// for the reference to catch up; `delay` is how late the reference runs, measured from the
/// audio itself every couple of seconds.
struct Canceller {
    aec: Aec,
    /// Debug mode: the mic before cancellation, sample-aligned with system.wav.
    raw: Option<WavWriter>,
    loopback: Arc<Mutex<Loopback>>,
    pending: Vec<f32>,
    next: u64,
    delay: i64,
    /// The last few seconds of mic audio before cancellation, to measure `delay` on.
    history: VecDeque<f32>,
    since_estimate: usize,
    levels: Levels,
    debug: bool,
}

impl Canceller {
    const HISTORY: usize = audio::SAMPLE_RATE as usize * 5;
    const ESTIMATE_EVERY: usize = audio::SAMPLE_RATE as usize * 2;

    fn put(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let start = self.reference_start();
        let (ready, reference) = {
            let loopback = self.loopback.lock().unwrap();
            let available = loopback.end().saturating_sub(start) as usize;
            // Wait for the reference, but never longer than ECHO_WAIT beyond its delay.
            let wait = ECHO_WAIT + self.delay.max(0) as usize;
            let ready = available.min(self.pending.len()).max(self.pending.len().saturating_sub(wait));
            (ready, loopback.take(start, ready))
        };
        let clean = self.process(ready, &reference)?;
        self.since_estimate += ready;
        if self.since_estimate >= Self::ESTIMATE_EVERY && self.history.len() == Self::HISTORY {
            self.since_estimate = 0;
            self.estimate_delay();
        }
        Ok(clean)
    }

    fn finish(&mut self) -> Result<Vec<f32>> {
        let reference = self.loopback.lock().unwrap().take(self.reference_start(), self.pending.len());
        let mut clean = self.process(self.pending.len(), &reference)?;
        self.aec.finish(&mut clean)?;
        if let Some(raw) = self.raw.take() {
            raw.finish()?;
        }
        Ok(clean)
    }

    fn reference_start(&self) -> u64 {
        (self.next as i64 + self.delay).max(0) as u64
    }

    fn process(&mut self, n: usize, reference: &[f32]) -> Result<Vec<f32>> {
        let mut clean = Vec::new();
        self.aec.process(&self.pending[..n], reference, &mut clean)?;
        self.levels.add(&self.pending[..n], reference, &clean);
        if let Some(raw) = &mut self.raw {
            raw.write(&self.pending[..n])?;
        }
        self.history.extend(&self.pending[..n]);
        let excess = self.history.len().saturating_sub(Self::HISTORY);
        self.history.drain(..excess);
        self.pending.drain(..n);
        self.next += n as u64;
        Ok(clean)
    }

    /// Measures how late the reference is, on the mic's [−5 s, −1 s) window (whose reference,
    /// even 800 ms late, is already in).
    fn estimate_delay(&mut self) {
        let window_len = Self::HISTORY - audio::SAMPLE_RATE as usize;
        let window: Vec<f32> = self.history.iter().take(window_len).copied().collect();
        let window_start = self.next - Self::HISTORY as u64;
        let reference = self.loopback.lock().unwrap().take(
            window_start.saturating_sub(DELAY_MIN as u64),
            window_len + DELAY_MIN + DELAY_MAX,
        );
        if window_start < DELAY_MIN as u64 {
            return;
        }
        // `found` is how late the reference runs, unshifted. DTLN-aec needs the reference to lead
        // its echo, as it does physically: aligned exactly, it cancels almost nothing (measured:
        // 8 dB, against 38 dB with a 15–45 ms lead).
        if let Some(found) = estimate_delay(&window, &reference) {
            let target = found + REFERENCE_LEAD;
            if (target - self.delay).abs() > REFERENCE_SLACK {
                if self.debug {
                    log(&format!(
                        "echo cancellation: speakers' audio {} ms late; reference shifted {} ms to lead the echo by {} ms",
                        found * 1000 / audio::SAMPLE_RATE as i64,
                        target * 1000 / audio::SAMPLE_RATE as i64,
                        REFERENCE_LEAD * 1000 / audio::SAMPLE_RATE as i64
                    ));
                }
                self.delay = target;
            }
        }
    }
}

/// How far the reference should run ahead of its echo in the mic, in samples (30 ms).
const REFERENCE_LEAD: i64 = audio::SAMPLE_RATE as i64 * 30 / 1000;
/// Leads between about 15 and 45 ms cancel equally well; only shift when further off.
const REFERENCE_SLACK: i64 = audio::SAMPLE_RATE as i64 * 15 / 1000;

/// In debug mode: the mic's level before and after echo cancellation, and the reference's,
/// logged every 5 s, to see what the cancellation removes.
struct Levels {
    on: bool,
    sums: [f64; 3],
    n: usize,
}

impl Levels {
    fn new(on: bool) -> Levels {
        Levels { on, sums: [0.0; 3], n: 0 }
    }

    fn add(&mut self, mic: &[f32], reference: &[f32], clean: &[f32]) {
        if !self.on {
            return;
        }
        for (sum, s) in self.sums.iter_mut().zip([mic, reference, clean]) {
            *sum += s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
        }
        self.n += mic.len();
        if self.n >= audio::SAMPLE_RATE as usize * 5 {
            let db = |sum: f64| 10.0 * (sum / self.n as f64).max(1e-12).log10();
            log(&format!(
                "echo cancellation over 5 s (dBFS RMS): mic {:.0} → {:.0} after, speakers {:.0}",
                db(self.sums[0]),
                db(self.sums[2]),
                db(self.sums[1])
            ));
            self.sums = [0.0; 3];
            self.n = 0;
        }
    }
}

/// The mic waits at most this long for the reference, then goes on with silence as reference
/// (nothing played, or the system tap is late).
const ECHO_WAIT: usize = audio::SAMPLE_RATE as usize / 2;

/// Where a track's 16 kHz audio goes: echo handling, then the WAV file and the live transcript.
struct Output {
    wav: WavWriter,
    chunker: Chunker,
    live: mpsc::Sender<live::Chunk>,
    echo: Echo,
}

impl Output {
    fn put(&mut self, samples: &[f32]) -> Result<()> {
        let clean = match &mut self.echo {
            Echo::Reference(loopback) => {
                loopback.lock().unwrap().push(samples);
                return self.write(samples);
            }
            Echo::Off => return self.write(samples),
            Echo::Cancel(c) => c.put(samples)?,
        };
        self.write(&clean)
    }

    /// Flushes everything still held back, with silence as the reference beyond what there is.
    fn finish(mut self) -> Result<()> {
        if let Echo::Cancel(c) = &mut self.echo {
            let clean = c.finish()?;
            self.write(&clean)?;
        }
        let mut chunks = Vec::new();
        self.chunker.finish(&mut chunks);
        for chunk in chunks {
            let _ = self.live.send(chunk);
        }
        self.wav.finish()?;
        Ok(())
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        self.wav.write(samples)?;
        let mut chunks = Vec::new();
        self.chunker.push(samples, &mut chunks);
        // A dead transcriber just means no live transcript.
        for chunk in chunks {
            let _ = self.live.send(chunk);
        }
        Ok(())
    }
}

/// A thread that resamples one track to 16 kHz and streams it into its WAV file.
fn track_writer(
    meeting: &Meeting,
    track: Track,
    live: mpsc::Sender<live::Chunk>,
    echo: Echo,
    debug: bool,
) -> Result<(mpsc::Sender<Audio>, JoinHandle<Result<()>>)> {
    let (tx, rx) = mpsc::channel::<Audio>();
    let mut output = Output { wav: WavWriter::create(&meeting.track_path(track))?, chunker: Chunker::new(track), live, echo };
    // The meeting's start on the capture clock.
    let since_start = (Local::now() - meeting.start).num_nanoseconds().unwrap_or(0).max(0) as u64;
    let start_ns = capture::now_ns().saturating_sub(since_start);
    let handle = thread::spawn(move || {
        let mut resampler: Option<(To16k, u32)> = None;
        let mut out = Vec::new();
        let mut last_flush = Instant::now();
        let t0 = Instant::now();
        let (mut got, mut last_log) = (0usize, Instant::now());
        // When the next sample is due, on the capture clock.
        let mut position_ns = start_ns;
        for (samples, rate, captured) in rx {
            got += samples.len();
            if debug && last_log.elapsed() > Duration::from_secs(1) {
                log(&format!("{:?}: {:5.1}s in, received {:6.2}s of audio at {rate} Hz", track, t0.elapsed().as_secs_f32(), got as f32 / rate as f32));
                last_log = Instant::now();
            }
            // A device can change sample rate mid-meeting (Bluetooth headsets switching mode,
            // for example): finish the old resampler and start a new one at the new rate.
            if let Some((rs, old)) = &mut resampler
                && *old != rate
            {
                log(&format!("{:?}: sample rate changed from {old} Hz to {rate} Hz", track));
                out.clear();
                rs.finish(&mut out)?;
                output.put(&out)?;
                resampler = Some((To16k::new(rate)?, rate));
            }
            let rs = match &mut resampler {
                Some((rs, _)) => rs,
                None => &mut resampler.insert((To16k::new(rate)?, rate)).0,
            };
            out.clear();

            // Place the audio by when it was captured, not when it arrived. A source can start
            // late, and macOS's system-audio tap delivers nothing while nothing plays: fill those
            // gaps with silence at the device's rate, so both tracks stay lined up to the
            // millisecond, which echo cancellation depends on.
            let gap_ns = captured.saturating_sub(position_ns);
            if gap_ns > MAX_GAP_NS {
                if debug && gap_ns > 300_000_000 {
                    log(&format!("{:?}: {:.1} s without audio, filled with silence", track, gap_ns as f64 / 1e9));
                }
                let silence = (gap_ns as u128 * rate as u128 / 1_000_000_000) as usize;
                rs.push(&vec![0.0; silence], &mut out)?;
                position_ns = captured;
            }
            rs.push(&samples, &mut out)?;
            position_ns += samples.len() as u64 * 1_000_000_000 / rate as u64;
            output.put(&out)?;
            if last_flush.elapsed() > Duration::from_secs(1) {
                output.wav.flush()?;
                last_flush = Instant::now();
            }
        }
        if let Some((rs, _)) = &mut resampler {
            out.clear();
            rs.finish(&mut out)?;
            output.put(&out)?;
        }
        output.finish()
    });
    Ok((tx, handle))
}

/// One timestamped line in the meeting's recorder.log (the recorder's stderr).
pub fn log(line: &str) {
    eprintln!("[{}] {line}", Local::now().format("%H:%M:%S"));
}

// ---- The background process ----

#[derive(Serialize, Deserialize)]
struct State {
    pid: i32,
    meeting: String,
}

pub enum Status {
    Idle,
    Recording { meeting: Meeting, pid: i32 },
    /// The recorder process is gone without being stopped; its audio is still on disk.
    Died { meeting: Meeting },
}

fn state_path() -> PathBuf {
    meetings_dir().with_file_name("recorder.json")
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn status() -> Result<Status> {
    let Ok(text) = fs::read_to_string(state_path()) else { return Ok(Status::Idle) };
    let state: State = serde_json::from_str(&text)?;
    let meeting = Meeting::load(&state.meeting)?;
    Ok(if alive(state.pid) { Status::Recording { meeting, pid: state.pid } } else { Status::Died { meeting } })
}

/// Starts a recorder process (`exe __record <id>`) for a new meeting.
pub fn spawn(exe: &Path, title: String) -> Result<Meeting> {
    match status()? {
        Status::Recording { meeting, .. } => bail!("already recording '{}' ({}). Stop it first: heylisten stop", meeting.title, meeting.id),
        Status::Died { meeting } => bail!("the recorder for '{}' died. Finish that meeting first: heylisten stop", meeting.title),
        Status::Idle => {}
    }
    Meeting::prune_old_audio();
    let meeting = Meeting::create(title, Local::now())?;
    let log = meeting.dir().join("recorder.log");
    launch(exe, &meeting.id, &log)?;

    // The recorder writes the state file itself once it runs. Then give it a moment to fail
    // on permissions or devices, and surface why.
    let fail = |why: &str| -> Result<Meeting> {
        let log = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_file(state_path());
        let _ = fs::remove_dir_all(meeting.dir());
        bail!("recorder failed to start: {}", if log.trim().is_empty() { why } else { log.trim() })
    };
    let started = Instant::now();
    loop {
        match status() {
            Ok(Status::Recording { meeting: m, .. }) if m.id == meeting.id && started.elapsed() > Duration::from_millis(1500) => break,
            Ok(Status::Died { meeting: m }) if m.id == meeting.id => return fail("it exited"),
            _ if started.elapsed() > Duration::from_secs(10) => return fail("it didn't start within 10 s"),
            _ => thread::sleep(Duration::from_millis(100)),
        }
    }
    Ok(meeting)
}

/// macOS: through a tiny app bundle, so the microphone and system audio permissions belong to
/// heyListen itself rather than to whatever terminal or hotkey app ran `start`.
///
/// The bundle holds a launcher (a copy of this binary, run as `__launch`) that starts the real
/// binary as its child. macOS gives a child the permissions of the app that launched it, and the
/// launcher is only replaced when `LAUNCHER_VERSION` changes, so rebuilding heyListen doesn't
/// change the bundle's signature and the permissions stick.
#[cfg(target_os = "macos")]
fn launch(exe: &Path, meeting_id: &str, log: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    const LAUNCHER_VERSION: &str = "1";

    let app = meetings_dir().with_file_name("heyListen.app");
    let bin = app.join("Contents/MacOS/heylisten");
    let version = app.join("Contents/launcher-version");
    if fs::read_to_string(&version).ok().as_deref() != Some(LAUNCHER_VERSION) {
        fs::create_dir_all(bin.parent().unwrap())?;
        fs::write(app.join("Contents/Info.plist"), include_str!("Info.plist"))?;
        fs::copy(exe, &bin)?;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
        // A copy of a downloaded heyListen inherits its quarantine flag, and Gatekeeper would then
        // block this launcher too. The user already approved heyListen itself.
        let _ = Command::new("xattr").args(["-dr", "com.apple.quarantine"]).arg(&app).status();
        let signed = Command::new("codesign").args(["--force", "--sign", "-"]).arg(&app).stderr(Stdio::null()).status()?;
        if !signed.success() {
            bail!("couldn't sign {}", app.display());
        }
        fs::write(&version, LAUNCHER_VERSION)?;
    }
    let mut open = Command::new("open");
    open.args(["-n", "-g", "-a"]).arg(&app);
    // `open` doesn't pass on the environment; the recorder needs to find the same config.
    for var in ["XDG_CONFIG_HOME", "HEYLISTEN_DEBUG"] {
        if let Some(value) = std::env::var_os(var) {
            open.arg("--env").arg(format!("{var}={}", value.to_string_lossy()));
        }
    }
    let opened = open
        .arg("--stderr")
        .arg(log)
        .args(["--args", "__launch"])
        .arg(exe)
        .args(["__record", meeting_id])
        .status()?;
    if !opened.success() {
        bail!("couldn't launch {}", app.display());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn launch(exe: &Path, meeting_id: &str, log: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    unsafe {
        Command::new(exe)
            .args(["__record", meeting_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(log)?)
            // Its own session, so closing the terminal doesn't take it down.
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()?;
    }
    Ok(())
}

/// Stops the recorder process and waits for it to finish writing. Returns the meeting.
pub fn stop() -> Result<Meeting> {
    let meeting = match status()? {
        Status::Idle => bail!("not recording"),
        Status::Died { meeting } => meeting,
        Status::Recording { pid, .. } => {
            // No time limit: the recorder finishes the live transcript before it exits.
            unsafe { libc::kill(pid, libc::SIGINT) };
            while alive(pid) {
                thread::sleep(Duration::from_millis(100));
            }
            let text = fs::read_to_string(state_path())?;
            Meeting::load(&serde_json::from_str::<State>(&text)?.meeting)?
        }
    };
    fs::remove_file(state_path())?;
    Ok(meeting)
}

static STOP: AtomicBool = AtomicBool::new(false);
static RECORDING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    // Still starting (e.g. blocked on a permission prompt): nothing to save, just exit.
    if !RECORDING.load(Ordering::SeqCst) {
        unsafe { libc::_exit(1) };
    }
    STOP.store(true, Ordering::SeqCst);
}

/// The body of the recorder process: record until SIGINT or SIGTERM.
pub fn run(config: &Config, meeting_id: &str) -> Result<()> {
    let mut meeting = Meeting::load(meeting_id)?;
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let state = State { pid: std::process::id() as i32, meeting: meeting.id.clone() };
    fs::write(state_path(), serde_json::to_string(&state)?)?;
    let recorder = Recorder::start(&meeting, &config.whisper_model, config.debug).context("can't start recording")?;
    RECORDING.store(true, Ordering::SeqCst);
    while !STOP.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }
    meeting.end = Local::now();
    meeting.live_complete = recorder.stop()?;
    meeting.save()
}
