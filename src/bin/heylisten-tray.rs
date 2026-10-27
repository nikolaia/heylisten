//! The tray / menu bar app: one more Viewer of the Recorder (see docs/adr/0003). It sets
//! heyListen up on first launch, starts and stops recordings, and lists recent notes.
//! Closing it never stops a recording.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use heylisten::config::{Config, SummaryEngine};
use heylisten::meeting::Meeting;
use heylisten::pipeline::{self, Event as PipelineEvent, Mode};
use heylisten::summarize::Progress as SummaryProgress;
use heylisten::recorder::{self, Status};
use heylisten::setup::{self, Needs, Progress};

const RECENT: usize = 10;

enum UserEvent {
    Menu(MenuEvent),
    /// From the background checker, every few seconds.
    Checked(Needs, Vec<PathBuf>),
    Started(Result<Meeting, String>),
    /// A step of whatever long-running job is going on.
    Busy(String),
    Done(Result<Option<PathBuf>, String>),
}

/// What the tray shows.
enum State {
    Idle,
    Starting,
    Recording(Meeting),
    /// The recorder stopped without being asked (a crash); its audio is saved. Stop finishes
    /// the meeting, and a new recording waits until then.
    Died(Meeting),
    /// Making the note after stop, or downloading models. `String` is the current step.
    Busy(String),
}

/// Menu items whose text or presence changes.
struct Items {
    status: MenuItem,
    download: MenuItem,
    install_ollama: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    /// Stop with only the transcript: no speakers, names or summary.
    stop_quick: MenuItem,
    recent: Submenu,
    summary: Submenu,
    notes_dir: MenuItem,
    debug: CheckMenuItem,
    report: MenuItem,
    quit: MenuItem,
}

struct Tray {
    icon: TrayIcon,
    menu: Menu,
    items: Items,
    state: State,
    /// While recording: seconds of audio the live transcript hasn't got to yet.
    behind_s: f32,
    needs: Option<Needs>,
    error: Option<String>,
    recent: Vec<PathBuf>,
    recent_ids: HashMap<MenuId, PathBuf>,
    /// Summary model choices: None is the built-in engine, Some is an Ollama model.
    summary_ids: HashMap<MenuId, Option<String>>,
    /// What the Summary model submenu was last built from.
    summary_shown: Option<(SummaryEngine, String, Vec<String>)>,
    shown_icon: Look,
    /// The breathing animations, and which frame is showing.
    busy: Vec<Icon>,
    recording: Vec<Icon>,
    frame: usize,
    started: Instant,
    /// Whether the setup items are in the menu right now.
    shown_download: bool,
    shown_install: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Idle,
    Recording,
    Busy,
}

/// The tray's own log, for what would otherwise vanish with the menu bar icon: startup errors,
/// failed actions, crashes. Errors only, never meeting content.
fn log(message: &str) {
    use std::io::Write as _;
    let path = heylisten::config::data_dir().join("tray.log");
    // Kept small: start over past 1 MB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {message}", Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

fn main() -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("crashed: {info}"));
        default_hook(info);
    }));
    run().inspect_err(|e| log(&format!("couldn't start: {e:#}")))
}

fn run() -> Result<()> {
    let mut config = Config::load()?;
    // `start` launches the recorder through the CLI binary, which sits next to this one.
    let cli = std::env::current_exe()?.with_file_name("heylisten");
    if !cli.is_file() {
        anyhow::bail!("can't find the heylisten CLI next to the tray app ({})", cli.display());
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        // Menu bar only, no Dock icon.
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));
    spawn_checker(proxy.clone());

    let items = Items {
        status: MenuItem::new("Checking setup…", false, None),
        download: MenuItem::new("Download models…", true, None),
        install_ollama: MenuItem::new("Install Ollama (for summaries)…", true, None),
        start: MenuItem::new("Start recording", false, None),
        stop: MenuItem::new("Stop recording", false, None),
        stop_quick: MenuItem::new("Stop without summary", false, None),
        recent: Submenu::new("Recent notes", false),
        summary: Submenu::new("Summary model", true),
        notes_dir: MenuItem::new("Set notes location…", true, None),
        debug: CheckMenuItem::new("Debug mode (keep recordings 7 days)", true, config.debug, None),
        report: MenuItem::new("Report a problem…", true, None),
        quit: MenuItem::new("Quit heyListen", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &items.start,
        &items.stop,
        &items.stop_quick,
        &PredefinedMenuItem::separator(),
        &items.recent,
        &items.summary,
        &items.notes_dir,
        &PredefinedMenuItem::separator(),
        &items.debug,
        &items.report,
        &items.quit,
    ])?;

    let mut items = Some(items);
    let mut tray: Option<Tray> = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(1));
        // Redraw on the timer and on our own events only. Redrawing on every event would loop:
        // changing menu text makes AppKit redraw, which is an event too.
        let wake = matches!(event, Event::NewEvents(_) | Event::UserEvent(_));
        match event {
            // The tray icon must be created once the event loop runs (macOS).
            Event::NewEvents(StartCause::Init) => {
                let icon = TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_tooltip("heyListen")
                    .with_icon_templated(idle_icon())
                    .build()
                    .expect("can't create the tray icon");
                tray = Some(Tray {
                    icon,
                    menu: menu.clone(),
                    items: items.take().unwrap(),
                    state: State::Idle,
                    behind_s: 0.0,
                    needs: None,
                    error: None,
                    recent: Vec::new(),
                    recent_ids: HashMap::new(),
                    summary_ids: HashMap::new(),
                    summary_shown: None,
                    shown_icon: Look::Idle,
                    busy: breathing(BUSY_BREATH, FRAME, ([255, 205, 60], 0.55), ([245, 135, 25], 1.0)),
                    recording: breathing(RECORDING_BREATH, SLOW_FRAME, ([230, 50, 50], 0.85), ([230, 50, 50], 1.0)),
                    frame: 0,
                    started: Instant::now(),
                    shown_download: false,
                    shown_install: false,
                });
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                let Some(t) = tray.as_mut() else { return };
                let id = &e.id;
                if id == t.items.start.id() {
                    t.state = State::Starting;
                    t.error = None;
                    spawn_start(cli.clone(), proxy.clone());
                } else if id == t.items.stop.id() || id == t.items.stop_quick.id() {
                    let mode = if id == t.items.stop.id() { Mode::Full } else { Mode::TranscriptOnly };
                    t.state = State::Busy("Stopping…".into());
                    spawn_stop(config.clone(), mode, proxy.clone());
                } else if id == t.items.download.id() {
                    t.state = State::Busy("Starting download…".into());
                    t.error = None;
                    spawn_setup(config.clone(), proxy.clone());
                } else if id == t.items.install_ollama.id() {
                    open(Path::new(setup::OLLAMA_DOWNLOAD));
                } else if id == t.items.notes_dir.id() {
                    let picked = rfd::FileDialog::new()
                        .set_title("Where should heyListen put meeting notes?")
                        .set_directory(&config.notes_dir)
                        .pick_folder();
                    if let Some(dir) = picked
                        && let Err(e) = config.set_notes_dir(&dir)
                    {
                        t.error = Some(format!("Couldn't save the notes location: {e:#}"));
                    }
                } else if id == t.items.debug.id() {
                    // The checkbox has already toggled itself.
                    if let Err(e) = config.set_debug(t.items.debug.is_checked()) {
                        t.error = Some(format!("Couldn't save debug mode: {e:#}"));
                    }
                } else if id == t.items.report.id() {
                    report_problem(&config);
                } else if id == t.items.quit.id() {
                    *control_flow = ControlFlow::Exit;
                } else if let Some(note) = t.recent_ids.get(id) {
                    open(note);
                } else if let Some(choice) = t.summary_ids.get(id).cloned() {
                    let engine = if choice.is_some() { SummaryEngine::Ollama } else { SummaryEngine::Builtin };
                    match config.set_summary(engine, choice.as_deref()) {
                        Ok(()) => t.needs = Some(Needs::check(&config)),
                        Err(e) => t.error = Some(format!("Couldn't save the summary model: {e:#}")),
                    }
                }
            }
            // Opening heyListen while it runs. If its icon is hidden (behind the camera notch, in a
            // full menu bar), this is the only way to reach it.
            Event::Reopen { .. } => {
                if let Some(t) = tray.as_mut()
                    && let Some(mode) = reopened(t)
                {
                    t.state = State::Busy("Stopping…".into());
                    spawn_stop(config.clone(), mode, proxy.clone());
                }
            }
            Event::UserEvent(UserEvent::Checked(needs, recent)) => {
                if let Some(t) = tray.as_mut() {
                    t.needs = Some(needs);
                    t.set_recent(recent);
                }
            }
            Event::UserEvent(UserEvent::Started(result)) => {
                let Some(t) = tray.as_mut() else { return };
                match result {
                    Ok(meeting) => t.state = State::Recording(meeting),
                    Err(e) => {
                        log(&format!("start failed: {e}"));
                        t.state = State::Idle;
                        t.error = Some(e);
                    }
                }
            }
            Event::UserEvent(UserEvent::Busy(step)) => {
                if let Some(t) = tray.as_mut() {
                    t.state = State::Busy(step);
                }
            }
            Event::UserEvent(UserEvent::Done(result)) => {
                let Some(t) = tray.as_mut() else { return };
                t.state = State::Idle;
                match result {
                    Ok(Some(note)) => {
                        let older = t.recent.iter().filter(|n| **n != note).cloned();
                        let recent = std::iter::once(note.clone()).chain(older).collect();
                        t.set_recent(recent);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log(&e);
                        t.error = Some(e);
                    }
                }
            }
            _ => {}
        }
        if let Some(t) = tray.as_mut()
            && wake
        {
            t.refresh(&config);
            let frame = match t.shown_icon {
                Look::Idle => None,
                Look::Busy => Some(FRAME),
                Look::Recording => Some(SLOW_FRAME),
            };
            if let Some(frame) = frame {
                // Wake often enough to animate.
                *control_flow = ControlFlow::WaitUntil(Instant::now() + frame);
            }
        }
    })
}

impl Tray {
    /// Follows recordings started or stopped elsewhere (the CLI), then redraws.
    fn refresh(&mut self, config: &Config) {
        if matches!(self.state, State::Idle | State::Recording(_) | State::Died(_)) {
            let status = recorder::status();
            if let Ok(Status::Died { meeting }) = &status
                && !matches!(&self.state, State::Died(m) if m.id == meeting.id)
            {
                log(&format!("recorder for {} stopped unexpectedly; see recorder.log in its meeting folder", meeting.id));
            }
            self.state = match status {
                Ok(Status::Recording { meeting, behind_s, .. }) => {
                    self.behind_s = behind_s;
                    State::Recording(meeting)
                }
                Ok(Status::Died { meeting }) => State::Died(meeting),
                _ => State::Idle,
            };
        }
        let idle = matches!(self.state, State::Idle);
        let needs_speech = self.needs.as_ref().is_none_or(|n| n.whisper_model);

        // Only in the menu, never next to the icon: a wider menu bar item is the first that
        // macOS hides behind the camera notch when the menu bar is full.
        let text = match &self.state {
            State::Idle => match (&self.error, &self.needs) {
                (Some(e), _) => e.clone(),
                (None, None) => "Checking setup…".into(),
                (None, Some(n)) if n.whisper_model => "Setup needed: download the models to start".into(),
                (None, Some(n)) if n.ollama_missing => "Ready (summaries need Ollama running)".into(),
                (None, Some(n)) if n.engine || n.summary_model => "Ready (download the Norwegian summary model for summaries)".into(),
                (None, Some(_)) => "Ready".into(),
            },
            State::Starting => "Starting…".into(),
            State::Recording(m) if self.behind_s >= recorder::BEHIND_WARNING_S => {
                format!("Recording '{}' · {} · {}", m.title, elapsed(m), recorder::behind_warning(self.behind_s))
            }
            State::Recording(m) => format!("Recording '{}' · {}", m.title, elapsed(m)),
            State::Died(m) => format!("Recording '{}' stopped unexpectedly. The audio so far is saved", m.title),
            State::Busy(step) => step.clone(),
        };
        set_text(&self.items.status, &text);
        set_enabled(&self.items.start, idle && !needs_speech);
        let stoppable = matches!(self.state, State::Recording(_) | State::Died(_));
        set_enabled(&self.items.stop, stoppable);
        set_enabled(&self.items.stop_quick, stoppable);
        let (stop, quick) = match &self.state {
            State::Died(m) => (format!("Finish '{}' (make its note)", m.title), "Finish without summary"),
            _ => ("Stop recording".into(), "Stop without summary"),
        };
        set_text(&self.items.stop, &stop);
        set_text(&self.items.stop_quick, quick);

        // Setup items appear only while something is missing.
        if let Some(n) = &self.needs {
            let download = idle && n.downloadable();
            if download {
                set_text(&self.items.download, &format!("Download models (~{:.1} GB)…", n.download_bytes() as f64 / 1e9));
            }
            self.shown_download = show(&self.menu, &self.items.download, 1, download, self.shown_download);
            let install = n.ollama_missing;
            let at = 1 + self.shown_download as usize;
            self.shown_install = show(&self.menu, &self.items.install_ollama, at, install, self.shown_install);
        }
        self.set_summary_choices(config);
        let dir = config.notes_dir.file_name().map_or_else(|| config.notes_dir.display().to_string(), |n| n.to_string_lossy().into());
        set_text(&self.items.notes_dir, &format!("Set notes location… ({dir})"));

        let look = match self.state {
            State::Recording(_) => Look::Recording,
            State::Starting | State::Busy(_) => Look::Busy,
            State::Idle | State::Died(_) => Look::Idle,
        };
        let frames = match look {
            Look::Idle => None,
            Look::Busy => Some((&self.busy, FRAME)),
            Look::Recording => Some((&self.recording, SLOW_FRAME)),
        };
        match frames {
            Some((frames, every)) => {
                // The frame follows the clock, however often the event loop happens to run.
                let frame = (self.started.elapsed().as_millis() / every.as_millis()) as usize % frames.len();
                if frame != self.frame || look != self.shown_icon {
                    let _ = self.icon.set_icon(Some(frames[frame].clone()));
                    self.frame = frame;
                }
            }
            None if look != self.shown_icon => {
                let _ = self.icon.set_icon_templated(Some(idle_icon()));
            }
            None => {}
        }
        self.shown_icon = look;
    }

    /// Rebuilds the Summary model submenu: the built-in Borealis, plus whatever Ollama has.
    fn set_summary_choices(&mut self, config: &Config) {
        let models = self.needs.as_ref().map(|n| n.ollama_models.clone()).unwrap_or_default();
        let wanted = (config.summary_engine, config.ollama_model.clone(), models);
        if self.summary_shown.as_ref() == Some(&wanted) {
            return;
        }
        while self.items.summary.remove_at(0).is_some() {}
        self.summary_ids.clear();
        let builtin = config.summary_engine == SummaryEngine::Builtin;
        let item = CheckMenuItem::new("Built-in: Borealis (Norwegian)", true, builtin, None);
        let _ = self.items.summary.append(&item);
        self.summary_ids.insert(item.id().clone(), None);
        if !wanted.2.is_empty() {
            let _ = self.items.summary.append(&PredefinedMenuItem::separator());
        }
        for model in &wanted.2 {
            let checked = !builtin && ollama_matches(model, &config.ollama_model);
            let item = CheckMenuItem::new(format!("Ollama: {model}"), true, checked, None);
            let _ = self.items.summary.append(&item);
            self.summary_ids.insert(item.id().clone(), Some(model.clone()));
        }
        self.summary_shown = Some(wanted);
    }

    /// Rebuilds the Recent notes submenu if the list changed.
    fn set_recent(&mut self, recent: Vec<PathBuf>) {
        let recent: Vec<PathBuf> = recent.into_iter().take(RECENT).collect();
        if recent == self.recent {
            return;
        }
        while self.items.recent.remove_at(0).is_some() {}
        self.recent_ids.clear();
        for note in &recent {
            let label = note.file_stem().map_or_else(|| note.display().to_string(), |s| s.to_string_lossy().into());
            let item = MenuItem::new(label, true, None);
            let _ = self.items.recent.append(&item);
            self.recent_ids.insert(item.id().clone(), note.clone());
        }
        self.items.recent.set_enabled(!recent.is_empty());
        self.recent = recent;
    }
}

/// Changing a menu item makes AppKit redraw it, so only change what actually changed.
fn set_text(item: &MenuItem, text: &str) {
    if item.text() != text {
        item.set_text(text);
    }
}

fn set_enabled(item: &MenuItem, enabled: bool) {
    if item.is_enabled() != enabled {
        item.set_enabled(enabled);
    }
}

/// Adds or removes a menu item. Returns whether it's shown now.
fn show(menu: &Menu, item: &MenuItem, at: usize, wanted: bool, shown: bool) -> bool {
    if wanted && !shown {
        let _ = menu.insert(item, at);
    } else if !wanted && shown {
        let _ = menu.remove(item);
    }
    wanted
}

/// Ollama lists `name:latest` for models pulled without a tag.
fn ollama_matches(listed: &str, configured: &str) -> bool {
    listed == configured || listed.strip_suffix(":latest") == Some(configured)
}

fn elapsed(m: &Meeting) -> String {
    let secs = (Local::now() - m.start).num_seconds().max(0);
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Shows what heyListen is doing when it's opened while already running, with a way to stop a
/// recording. Returns how to stop, if the person chose to.
fn reopened(t: &Tray) -> Option<Mode> {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult};
    const HIDDEN: &str = "If you can't see heyListen's ring in the menu bar, macOS is probably hiding it behind \
        the camera notch because the menu bar is full. Quitting a few other menu bar apps makes room.";
    let (what, stop) = match &t.state {
        State::Recording(m) => (format!("heyListen is recording '{}' ({}).", m.title, elapsed(m)), Some("Stop recording")),
        State::Died(m) => (format!("Recording '{}' stopped unexpectedly. The audio so far is saved.", m.title), Some("Finish it")),
        State::Busy(step) => (format!("heyListen is busy: {step}"), None),
        State::Starting => ("heyListen is starting a recording.".into(), None),
        State::Idle => ("heyListen is running.".into(), None),
    };
    let dialog = MessageDialog::new().set_title("heyListen").set_description(format!("{what}\n\n{HIDDEN}"));
    match stop {
        Some(stop) => {
            let answer = dialog.set_buttons(MessageButtons::OkCancelCustom(stop.into(), "Not now".into())).show();
            (answer == MessageDialogResult::Custom(stop.into())).then_some(Mode::Full)
        }
        None => {
            dialog.set_buttons(MessageButtons::Ok).show();
            None
        }
    }
}

/// Asks first, then opens a draft GitHub issue in the browser. Nothing is sent from here: the
/// person reviews the draft on GitHub and decides whether to submit it.
fn report_problem(config: &Config) {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult};
    let open = "Open draft";
    let answer = MessageDialog::new()
        .set_title("Report a problem")
        .set_description(format!(
            "heyListen will open a draft GitHub issue in your browser with {}.\n\nNothing is sent: you review the \
             draft, remove anything you don't want public, and decide whether to submit it.",
            heylisten::report::CONTENTS
        ))
        .set_buttons(MessageButtons::OkCancelCustom(open.into(), "Cancel".into()))
        .show();
    if answer == MessageDialogResult::Custom(open.into())
        && let Err(e) = heylisten::report::open(&heylisten::report::draft(config))
    {
        log(&format!("couldn't open the problem report: {e}"));
    }
}

/// Every few seconds: what's missing, and the recent notes (also those made from the CLI).
/// Reads the config each time, so changes from the menu or the CLI show up.
fn spawn_checker(proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let mut last_prune: Option<Instant> = None;
        loop {
            // Kept audio is deleted after 7 days, even if heyListen is never used to record.
            if last_prune.is_none_or(|t| t.elapsed() > Duration::from_secs(3600)) {
                Meeting::prune_old_audio();
                last_prune = Some(Instant::now());
            }
            let Ok(config) = Config::load() else {
                thread::sleep(Duration::from_secs(5));
                continue;
            };
            let event = UserEvent::Checked(Needs::check(&config), Meeting::recent_notes(RECENT));
            if proxy.send_event(event).is_err() {
                return;
            }
            thread::sleep(Duration::from_secs(5));
        }
    });
}

fn spawn_start(cli: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let title = heylisten::note::default_title(Local::now());
        let result = recorder::spawn(&cli, title).map_err(|e| format!("{e:#}"));
        let _ = proxy.send_event(UserEvent::Started(result));
    });
}

fn spawn_stop(config: Config, mode: Mode, proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let result = (|| -> Result<PathBuf> {
            let _awake = heylisten::awake::Activity::begin("Making a meeting note");
            let progress = proxy.clone();
            let meeting = recorder::stop(move |left| {
                let _ = progress.send_event(UserEvent::Busy(format!("Transcribing the rest: {}…", recorder::describe_left(left))));
            })
            .context("couldn't stop")?;
            let p = proxy.clone();
            let on_event = Arc::new(move |e: PipelineEvent| {
                let step = match e {
                    PipelineEvent::LoadingModel | PipelineEvent::Transcribing { .. } => "Transcribing…".into(),
                    PipelineEvent::Diarizing(_) => "Telling speakers apart…".into(),
                    PipelineEvent::FindingNames => "Finding names…".into(),
                    PipelineEvent::Summarizing { .. } => "Summarizing…".into(),
                    PipelineEvent::SummaryProgress(SummaryProgress::Reading(percent)) => format!("Summarizing: reading the meeting {percent}%…"),
                    PipelineEvent::SummaryProgress(SummaryProgress::Writing(words)) => format!("Summarizing: {words} words written…"),
                    _ => return,
                };
                let _ = p.send_event(UserEvent::Busy(step));
            });
            pipeline::process(&meeting, &config, true, mode, on_event)
                .with_context(|| format!("couldn't make the note for '{}' (try again: heylisten process {})", meeting.title, meeting.id))
        })();
        let _ = proxy.send_event(UserEvent::Done(result.map(Some).map_err(|e| format!("{e:#}"))));
    });
}

/// Downloads the speech model, then the summary model if Ollama is running.
fn spawn_setup(config: Config, proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let mut last = String::new();
        let mut report = |p: Progress| {
            let step = match p {
                Progress::SpeechModel { done, total } => format!("Downloading speech model… {}", percent(done, total)),
                Progress::Engine { done, total } => format!("Downloading summary engine… {}", percent(done, total)),
                Progress::SummaryModel { done, total, .. } if total > 0 => {
                    format!("Downloading Norwegian summary model… {}", percent(done, total))
                }
                Progress::SummaryModel { status, .. } => format!("Summary model: {status}…"),
            };
            if step != last {
                let _ = proxy.send_event(UserEvent::Busy(step.clone()));
                last = step;
            }
        };
        let result = setup::download_speech_model(&config, &mut report).and_then(|()| {
            if Needs::check(&config).ollama_missing { Ok(()) } else { setup::get_summary_model(&config, &mut report) }
        });
        let _ = proxy.send_event(UserEvent::Done(result.map(|()| None).map_err(|e| format!("Setup failed: {e:#}"))));
    });
}

fn percent(done: u64, total: u64) -> String {
    let size = if total >= 1_000_000_000 { format!("{:.1} GB", total as f64 / 1e9) } else { format!("{} MB", total / 1_000_000) };
    format!("{}% of {size}", done * 100 / total.max(1))
}

fn open(target: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener).arg(target).spawn();
}

const SIZE: u32 = 44; // 22 pt menu bar icons, drawn at 2x
/// The busy pulse runs at 24 frames a second; the faint recording pulse, which runs for the
/// whole meeting, at 12 to keep it cheap. Every frame is drawn up front.
const FRAME: Duration = Duration::from_millis(1000 / 24);
const SLOW_FRAME: Duration = Duration::from_millis(1000 / 12);
const BUSY_BREATH: Duration = Duration::from_millis(1600);
const RECORDING_BREATH: Duration = Duration::from_millis(2400);
const DOT_RADIUS: f32 = 0.6;

/// A ring: black on transparent, used as a template so macOS tints it for light and dark menu bars.
fn idle_icon() -> Icon {
    draw(|d| {
        let coverage = edge(d - 0.55).min(edge(0.78 - d));
        ([0, 0, 0], coverage)
    })
}

/// A dot that eases from one colour and opacity to another and back, once per `breath`.
fn breathing(breath: Duration, every: Duration, from: ([u8; 3], f32), to: ([u8; 3], f32)) -> Vec<Icon> {
    let frames = (breath.as_millis() / every.as_millis()).max(1) as usize;
    (0..frames)
        .map(|i| {
            // 0 → 1 → 0, eased at both ends.
            let t = 0.5 - 0.5 * (i as f32 / frames as f32 * std::f32::consts::TAU).cos();
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            let rgb = [0, 1, 2].map(|c| lerp(from.0[c] as f32, to.0[c] as f32).round() as u8);
            let alpha = lerp(from.1, to.1);
            draw(move |d| (rgb, alpha * edge(DOT_RADIUS - d)))
        })
        .collect()
}

/// Anti-aliasing: how much of a pixel lies inside an edge, given its distance inside it
/// (in units of the icon's radius).
fn edge(inside: f32) -> f32 {
    (inside * SIZE as f32 / 2.0 + 0.5).clamp(0.0, 1.0)
}

/// Draws a square icon from a function of the distance from the centre (0 at the centre, 1 at
/// the edge) to a colour and opacity (0 to 1).
fn draw(paint: impl Fn(f32) -> ([u8; 3], f32)) -> Icon {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = SIZE as f32 / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt() / c;
            let ([r, g, b], a) = paint(d);
            rgba.extend([r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon")
}
