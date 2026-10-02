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

use crate::aec::Aec;
use crate::audio::{self, To16k, WavWriter};
use crate::capture::{Mic, System};
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
                Echo::Cancel { aec: Box::new(aec), loopback: loopback.clone(), pending: Vec::new(), next: 0, levels: Levels::new(debug) }
            }
            Err(e) => {
                log(&format!("echo cancellation: off ({e:#})"));
                Echo::Off
            }
        };
        let (mic_tx, mic_writer) = track_writer(meeting, Track::Mic, live_tx.clone(), echo, debug)?;
        let (sys_tx, sys_writer) = track_writer(meeting, Track::System, live_tx, Echo::Reference(loopback), debug)?;
        let mic = Mic::start(Box::new(move |s, rate| drop(mic_tx.send((s.to_vec(), rate)))), debug)?;
        let system = System::start(Box::new(move |s, rate| drop(sys_tx.send((s.to_vec(), rate)))))?;
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

type Audio = (Vec<f32>, u32);

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
    /// The mic: cancel the reference out of it first. `pending` is mic audio from sample `next`
    /// on, waiting for the reference to catch up.
    Cancel { aec: Box<Aec>, loopback: Arc<Mutex<Loopback>>, pending: Vec<f32>, next: u64, levels: Levels },
    Off,
}

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
            Echo::Cancel { aec, loopback, pending, next, levels } => {
                pending.extend_from_slice(samples);
                let (ready, reference) = {
                    let loopback = loopback.lock().unwrap();
                    let available = loopback.end().saturating_sub(*next) as usize;
                    let ready = available.min(pending.len()).max(pending.len().saturating_sub(ECHO_WAIT));
                    (ready, loopback.take(*next, ready))
                };
                let mut clean = Vec::new();
                aec.process(&pending[..ready], &reference, &mut clean)?;
                levels.add(&pending[..ready], &reference, &clean);
                pending.drain(..ready);
                *next += ready as u64;
                clean
            }
        };
        self.write(&clean)
    }

    /// Flushes everything still held back, with silence as the reference beyond what there is.
    fn finish(mut self) -> Result<()> {
        if let Echo::Cancel { aec, loopback, pending, next, .. } = &mut self.echo {
            let reference = loopback.lock().unwrap().take(*next, pending.len());
            let mut clean = Vec::new();
            aec.process(pending, &reference, &mut clean)?;
            aec.finish(&mut clean)?;
            pending.clear();
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
    let start = meeting.start;
    let handle = thread::spawn(move || {
        let mut resampler: Option<(To16k, u32)> = None;
        let mut out = Vec::new();
        let mut last_flush = Instant::now();
        let t0 = Instant::now();
        let (mut got, mut last_log) = (0usize, Instant::now());
        // 16 kHz samples written so far, silence included.
        let mut written: u64 = 0;
        for (samples, rate) in rx {
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
                written += out.len() as u64;
                resampler = Some((To16k::new(rate)?, rate));
            }
            let rs = match &mut resampler {
                Some((rs, _)) => rs,
                None => &mut resampler.insert((To16k::new(rate)?, rate)).0,
            };
            out.clear();
            rs.push(&samples, &mut out)?;

            // Keep the track in step with the clock. A source can start late, and macOS's
            // system-audio tap delivers nothing while nothing is playing: fill those gaps with
            // silence, so both tracks (and the timestamps) stay lined up with real time.
            let due = (Local::now() - start).num_milliseconds().max(0) as u64 * audio::SAMPLE_RATE as u64 / 1000;
            let gap = due.saturating_sub(written + out.len() as u64);
            if gap > audio::SAMPLE_RATE as u64 * 3 / 10 {
                if debug {
                    log(&format!("{:?}: {:.1} s without audio, filled with silence", track, gap as f32 / audio::SAMPLE_RATE as f32));
                }
                output.put(&vec![0.0; gap as usize])?;
                written += gap;
            }
            output.put(&out)?;
            written += out.len() as u64;
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
