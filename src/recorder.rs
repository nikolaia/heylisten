//! The Recorder: captures a meeting's tracks to disk. It runs as its own background process
//! (see docs/adr/0003); this module also starts, stops and finds that process.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use serde::{Deserialize, Serialize};

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
    pub fn start(meeting: &Meeting, whisper_model: &Path) -> Result<Recorder> {
        let (live_tx, live) = live::spawn(whisper_model.to_path_buf(), meeting.dir())?;
        let (mic_tx, mic_writer) = track_writer(meeting, Track::Mic, live_tx.clone())?;
        let (sys_tx, sys_writer) = track_writer(meeting, Track::System, live_tx)?;
        let mic = Mic::start(Box::new(move |s, rate| drop(mic_tx.send((s.to_vec(), rate)))))?;
        let system = System::start(Box::new(move |s, rate| drop(sys_tx.send((s.to_vec(), rate)))))?;
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
                eprintln!("live transcript incomplete: {e:#}");
                Ok(false)
            }
        }
    }
}

type Audio = (Vec<f32>, u32);

/// A thread that resamples one track to 16 kHz and streams it into its WAV file.
fn track_writer(meeting: &Meeting, track: Track, live: mpsc::Sender<live::Chunk>) -> Result<(mpsc::Sender<Audio>, JoinHandle<Result<()>>)> {
    let (tx, rx) = mpsc::channel::<Audio>();
    let mut wav = WavWriter::create(&meeting.track_path(track))?;
    let start = meeting.start;
    let handle = thread::spawn(move || {
        let mut resampler: Option<To16k> = None;
        let mut chunker = Chunker::new(track);
        let mut chunks = Vec::new();
        let mut out = Vec::new();
        let mut last_flush = Instant::now();
        // Sends out finished chunks. A dead transcriber just means no live transcript.
        let mut feed = |samples: &[f32], chunker: &mut Chunker, last: bool| {
            chunker.push(samples, &mut chunks);
            if last {
                chunker.finish(&mut chunks);
            }
            for chunk in chunks.drain(..) {
                let _ = live.send(chunk);
            }
        };
        let t0 = Instant::now();
        let (mut got, mut last_log) = (0usize, Instant::now());
        for (samples, rate) in rx {
            got += samples.len();
            if std::env::var_os("HEYLISTEN_DEBUG").is_some() && last_log.elapsed() > Duration::from_secs(1) {
                eprintln!("[{:?}] {:5.1}s received {:6.2}s of audio at {rate} Hz", track, t0.elapsed().as_secs_f32(), got as f32 / rate as f32);
                last_log = Instant::now();
            }
            let rs = match &mut resampler {
                Some(rs) => rs,
                None => {
                    // Line the tracks up: pad with silence for the time before this one started.
                    let late = (Local::now() - start).num_milliseconds().max(0) as usize;
                    let padding = vec![0.0; late * audio::SAMPLE_RATE as usize / 1000];
                    wav.write(&padding)?;
                    feed(&padding, &mut chunker, false);
                    resampler.insert(To16k::new(rate)?)
                }
            };
            out.clear();
            rs.push(&samples, &mut out)?;
            wav.write(&out)?;
            feed(&out, &mut chunker, false);
            if last_flush.elapsed() > Duration::from_secs(1) {
                wav.flush()?;
                last_flush = Instant::now();
            }
        }
        if let Some(rs) = &mut resampler {
            out.clear();
            rs.finish(&mut out)?;
            wav.write(&out)?;
        }
        feed(&out, &mut chunker, true);
        wav.finish()?;
        Ok(())
    });
    Ok((tx, handle))
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
    let recorder = Recorder::start(&meeting, &config.whisper_model).context("can't start recording")?;
    RECORDING.store(true, Ordering::SeqCst);
    while !STOP.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }
    meeting.end = Local::now();
    meeting.live_complete = recorder.stop()?;
    meeting.save()
}
