use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

use heylisten::config::{self, Config};
use heylisten::doctor::{self, Status};
use heylisten::live;
use heylisten::transcript::Who;
use heylisten::meeting::Meeting;
use heylisten::pipeline::{self, Event};
use heylisten::recorder::{self, Status as RecorderStatus};
use heylisten::setup;

/// Hey, listen! Local-only meeting transcripts and summaries.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start recording a meeting in the background
    Start {
        /// Meeting title (default: "Møte HH:MM")
        title: Option<String>,
    },
    /// Stop recording, then write the transcript, summary and note
    Stop,
    /// Show whether a meeting is being recorded
    Status,
    /// Follow the live transcript of the meeting being recorded
    Watch,
    /// Transcribe a meeting again, or an audio file (wav, m4a, mp3…), and write its note
    Process {
        /// Meeting id, or path to an audio file
        target: String,
    },
    /// Download the speech model and pull the summary model through Ollama (about 8 GB)
    Setup,
    /// Check models, Ollama and folders, with fixes for anything missing
    Doctor,
    /// The recorder process itself (started by `start`)
    #[command(name = "__record", hide = true)]
    Record { meeting: String },
    /// Runs a command as a child of this process (the macOS app bundle's launcher)
    #[command(name = "__launch", hide = true)]
    Launch {
        exe: std::path::PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Launch { exe, args } = &cli.command {
        return match std::process::Command::new(exe).args(args).status() {
            Ok(status) if status.success() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        };
    }
    let result = Config::load().and_then(|config| match cli.command {
        Command::Start { title } => start(title),
        Command::Watch => watch(),
        Command::Stop => stop(&config),
        Command::Status => status(),
        Command::Record { meeting } => recorder::run(&config, &meeting).map(|_| ExitCode::SUCCESS),
        Command::Launch { .. } => unreachable!(),
        Command::Process { target } => process(&config, &target),
        Command::Setup => setup(&config),
        Command::Doctor => doctor(&config),
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", red("error:"));
            ExitCode::FAILURE
        }
    }
}

fn start(title: Option<String>) -> Result<ExitCode> {
    let title = title.unwrap_or_else(|| chrono::Local::now().format("Møte %H:%M").to_string());
    let meeting = recorder::spawn(&std::env::current_exe()?, title)?;
    println!("Recording '{}' ({}). Stop with: heylisten stop", meeting.title, meeting.id);
    if std::io::stdout().is_terminal() {
        println!("Live transcript below. Ctrl+C closes this view; recording continues.\n");
        return watch();
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints the live transcript as it grows, until the recording stops.
fn watch() -> Result<ExitCode> {
    let RecorderStatus::Recording { meeting, .. } = recorder::status()? else {
        println!("Not recording");
        return Ok(ExitCode::SUCCESS);
    };
    let mut pos = 0;
    loop {
        let (segments, next) = live::read_from(&meeting.dir(), pos)?;
        pos = next;
        for s in segments {
            let secs = s.start_ms / 1000;
            let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
            let who = s.who.label();
            let who = if s.who == Who::Me { paint(&who, "36") } else { paint(&who, "33") };
            println!("[{time}] {who}: {}", s.text);
        }
        if !matches!(recorder::status()?, RecorderStatus::Recording { .. }) {
            println!("\nRecording stopped.");
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn stop(config: &Config) -> Result<ExitCode> {
    eprintln!("Stopping (finishing the live transcript)…");
    let meeting = recorder::stop()?;
    eprintln!("Stopped '{}' ({})", meeting.title, meeting.id);
    let id = meeting.id.clone();
    pipeline::process(&meeting, config, true, Arc::new(move |e| print_event(&id, e)))?;
    Ok(ExitCode::SUCCESS)
}

fn status() -> Result<ExitCode> {
    match recorder::status()? {
        RecorderStatus::Idle => println!("Idle"),
        RecorderStatus::Recording { meeting, .. } => {
            let secs = (chrono::Local::now() - meeting.start).num_seconds();
            let elapsed = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
            println!("Recording '{}' for {elapsed} ({})", meeting.title, meeting.id);
        }
        RecorderStatus::Died { meeting } => {
            println!("{} '{}' ({})", red("Recorder died while recording"), meeting.title, meeting.id);
            println!("The audio so far is saved. Finish it with: heylisten stop");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn process(config: &Config, target: &str) -> Result<ExitCode> {
    let meeting = if Path::new(target).is_file() {
        eprintln!("Importing {target}…");
        Meeting::import(Path::new(target))?
    } else {
        Meeting::load(target)?
    };
    eprintln!("Meeting {} ({})", meeting.id, meeting.title);
    let id = meeting.id.clone();
    pipeline::process(&meeting, config, false, Arc::new(move |e| print_event(&id, e)))?;
    Ok(ExitCode::SUCCESS)
}

fn print_event(meeting_id: &str, event: Event) {
    match event {
        Event::LoadingModel => eprintln!("Loading whisper model…"),
        Event::Diarizing(track) => eprintln!("Telling speakers apart on the {} track…", track.name()),
        Event::DiarizationFailed(track, why) => eprintln!("Couldn't tell speakers apart on the {} track ({why})", track.name()),
        Event::Transcribing { track, percent } => {
            eprint!("\rTranscribing {} track… {percent}%", track.name());
            if percent >= 100 {
                eprintln!();
            }
            let _ = std::io::stderr().flush();
        }
        Event::SkippedSilentTrack(track) => {
            eprintln!("Skipping {} track: no speech", track.name());
            if track == heylisten::meeting::Track::System {
                eprintln!("  If others were talking, allow heyListen under System Settings → Privacy & Security → Screen & System Audio Recording");
            }
        }
        Event::SendingOffMachine(url) => {
            eprintln!("{}", red(&format!("WARNING: sending the transcript to {url}, which is not this machine")))
        }
        Event::Summarizing { part: 1, parts: 1 } => eprintln!("Summarizing…"),
        Event::Summarizing { part, parts } => eprintln!("Summarizing part {part}/{parts}…"),
        Event::SummaryFailed(why) => {
            eprintln!("{} {why}", red("No summary:"));
            eprintln!("Audio kept. When Ollama is up (`heylisten doctor`), retry with: heylisten process {meeting_id}");
        }
        Event::NoteWritten(path) => println!("Note: {}", path.display()),
        Event::AudioDeleted => eprintln!("Deleted audio (keep_audio = false)"),
    }
}

fn setup(config: &Config) -> Result<ExitCode> {
    let needs = setup::Needs::check(config);
    if !needs.anything() {
        println!("Everything is set up. Try: heylisten start");
        return Ok(ExitCode::SUCCESS);
    }
    if needs.download_bytes() > 0 {
        eprintln!("Downloading about {:.1} GB. Nothing else is sent or received.", needs.download_bytes() as f64 / 1e9);
    }
    let mut last = String::new();
    setup::run(config, |p| {
        let line = match p {
            setup::Progress::SpeechModel { done, total } => format!("Speech model (NB-Whisper)… {}", percent(done, total)),
            setup::Progress::Engine { done, total } => format!("Summary engine (llama.cpp)… {}", percent(done, total)),
            setup::Progress::SummaryModel { done, total, .. } if total > 0 => format!("Summary model (Borealis)… {}", percent(done, total)),
            setup::Progress::SummaryModel { status, .. } => format!("Summary model (Ollama: {status})…"),
        };
        if line != last {
            eprint!("\r\x1b[2K{line}");
            let _ = std::io::stderr().flush();
            last = line;
        }
    })?;
    eprintln!("\nDone. Try: heylisten start");
    Ok(ExitCode::SUCCESS)
}

fn percent(done: u64, total: u64) -> String {
    let size = if total >= 1_000_000_000 { format!("{:.1} GB", total as f64 / 1e9) } else { format!("{} MB", total / 1_000_000) };
    format!("{}% of {size}", done * 100 / total.max(1))
}

fn doctor(config: &Config) -> Result<ExitCode> {
    println!("Config: {}\n", config::config_path().display());
    let checks = doctor::run(config);
    for c in &checks {
        let mark = match c.status {
            Status::Pass => green("✓"),
            Status::Warn => red("!"),
            Status::Fail => red("✗"),
        };
        println!("{mark} {}", c.what);
        if let Some(fix) = &c.fix {
            println!("    {fix}");
        }
    }
    let failed = checks.iter().any(|c| c.status == Status::Fail);
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn red(s: &str) -> String {
    paint(s, "1;31")
}

fn green(s: &str) -> String {
    paint(s, "32")
}

fn paint(s: &str, code: &str) -> String {
    if std::io::stdout().is_terminal() && std::io::stderr().is_terminal() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}
