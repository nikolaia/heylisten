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
use heylisten::pipeline::{self, Event as PipelineEvent};
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
    recent: Submenu,
    summary: Submenu,
    notes_dir: MenuItem,
    debug: CheckMenuItem,
    quit: MenuItem,
}

struct Tray {
    icon: TrayIcon,
    menu: Menu,
    items: Items,
    state: State,
    needs: Option<Needs>,
    error: Option<String>,
    recent: Vec<PathBuf>,
    recent_ids: HashMap<MenuId, PathBuf>,
    /// Summary model choices: None is the built-in engine, Some is an Ollama model.
    summary_ids: HashMap<MenuId, Option<String>>,
    /// What the Summary model submenu was last built from.
    summary_shown: Option<(SummaryEngine, String, Vec<String>)>,
    shown_icon: Look,
    /// The orange dot's pulse, and which frame is showing.
    pulse: Vec<Icon>,
    frame: usize,
    started: Instant,
    /// The text next to the icon (elapsed time while recording).
    title: Option<String>,
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

fn main() -> Result<()> {
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
        recent: Submenu::new("Recent notes", false),
        summary: Submenu::new("Summary model", true),
        notes_dir: MenuItem::new("Set notes location…", true, None),
        debug: CheckMenuItem::new("Debug mode (keep recordings)", true, config.debug, None),
        quit: MenuItem::new("Quit heyListen", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &items.start,
        &items.stop,
        &PredefinedMenuItem::separator(),
        &items.recent,
        &items.summary,
        &items.notes_dir,
        &PredefinedMenuItem::separator(),
        &items.debug,
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
                    needs: None,
                    error: None,
                    recent: Vec::new(),
                    recent_ids: HashMap::new(),
                    summary_ids: HashMap::new(),
                    summary_shown: None,
                    shown_icon: Look::Idle,
                    pulse: pulse_frames(),
                    frame: 0,
                    started: Instant::now(),
                    title: None,
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
                } else if id == t.items.stop.id() {
                    t.state = State::Busy("Stopping…".into());
                    spawn_stop(config.clone(), proxy.clone());
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
                    Err(e) => t.error = Some(e),
                }
            }
            _ => {}
        }
        if let Some(t) = tray.as_mut()
            && wake
        {
            t.refresh(&config);
            if t.shown_icon == Look::Busy {
                // Wake often enough to animate the pulse.
                *control_flow = ControlFlow::WaitUntil(Instant::now() + PULSE_FRAME);
            }
        }
    })
}

impl Tray {
    /// Follows recordings started or stopped elsewhere (the CLI), then redraws.
    fn refresh(&mut self, config: &Config) {
        if matches!(self.state, State::Idle | State::Recording(_)) {
            self.state = match recorder::status() {
                Ok(Status::Recording { meeting, .. }) => State::Recording(meeting),
                Ok(Status::Died { meeting }) => {
                    self.error = Some(format!("Recorder died: finish '{}' with heylisten stop", meeting.title));
                    State::Idle
                }
                _ => State::Idle,
            };
        }
        let idle = matches!(self.state, State::Idle);
        let needs_speech = self.needs.as_ref().is_none_or(|n| n.whisper_model);

        let (text, title) = match &self.state {
            State::Idle => {
                let text = match (&self.error, &self.needs) {
                    (Some(e), _) => e.clone(),
                    (None, None) => "Checking setup…".into(),
                    (None, Some(n)) if n.whisper_model => "Setup needed: download the models to start".into(),
                    (None, Some(n)) if n.ollama_missing => "Ready (summaries need Ollama running)".into(),
                    (None, Some(n)) if n.engine || n.summary_model => "Ready (download the Norwegian summary model for summaries)".into(),
                    (None, Some(_)) => "Ready".into(),
                };
                (text, None)
            }
            State::Starting => ("Starting…".into(), None),
            State::Recording(m) => {
                let secs = (Local::now() - m.start).num_seconds().max(0);
                let elapsed = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
                (format!("Recording '{}' · {elapsed}", m.title), Some(elapsed))
            }
            State::Busy(step) => (step.clone(), None),
        };
        set_text(&self.items.status, &text);
        if title != self.title {
            self.icon.set_title(title.as_deref());
            self.title = title;
        }
        set_enabled(&self.items.start, idle && !needs_speech);
        set_enabled(&self.items.stop, matches!(self.state, State::Recording(_)));

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
            State::Idle => Look::Idle,
        };
        if look == Look::Busy {
            // The frame follows the clock, however often the event loop happens to run.
            let frame = (self.started.elapsed().as_millis() / PULSE_FRAME.as_millis()) as usize % self.pulse.len();
            if frame != self.frame || self.shown_icon != Look::Busy {
                let _ = self.icon.set_icon(Some(self.pulse[frame].clone()));
                self.frame = frame;
            }
        } else if look != self.shown_icon {
            let _ = match look {
                Look::Recording => self.icon.set_icon(Some(dot_icon([230, 50, 50]))),
                Look::Idle => self.icon.set_icon_templated(Some(idle_icon())),
                Look::Busy => unreachable!(),
            };
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

/// Every few seconds: what's missing, and the recent notes (also those made from the CLI).
/// Reads the config each time, so changes from the menu or the CLI show up.
fn spawn_checker(proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        loop {
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
        let title = Local::now().format("Møte %H:%M").to_string();
        let result = recorder::spawn(&cli, title).map_err(|e| format!("{e:#}"));
        let _ = proxy.send_event(UserEvent::Started(result));
    });
}

fn spawn_stop(config: Config, proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let result = (|| -> Result<PathBuf> {
            let meeting = recorder::stop().context("couldn't stop")?;
            let p = proxy.clone();
            let on_event = Arc::new(move |e: PipelineEvent| {
                let step = match e {
                    PipelineEvent::LoadingModel | PipelineEvent::Transcribing { .. } => "Transcribing…",
                    PipelineEvent::Diarizing(_) => "Telling speakers apart…",
                    PipelineEvent::Summarizing { .. } => "Summarizing…",
                    _ => return,
                };
                let _ = p.send_event(UserEvent::Busy(step.into()));
            });
            pipeline::process(&meeting, &config, true, on_event)
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

/// A ring: black on transparent, used as a template so macOS tints it for light and dark menu bars.
fn idle_icon() -> Icon {
    draw(|d| (d > 0.55 && d < 0.78).then_some(([0, 0, 0], 255)))
}

/// A filled dot in a colour.
fn dot_icon(rgb: [u8; 3]) -> Icon {
    draw(|d| (d < 0.62).then_some((rgb, 255)))
}

/// One breath of the orange "working" dot takes PULSE_FRAMES × PULSE_FRAME = 1.8 s.
const PULSE_FRAME: Duration = Duration::from_millis(100);
const PULSE_FRAMES: usize = 18;

/// The orange "working" dot, breathing: it grows a little and fades, then comes back.
fn pulse_frames() -> Vec<Icon> {
    const FRAMES: usize = PULSE_FRAMES;
    (0..FRAMES)
        .map(|i| {
            // 1 at the start of the cycle, 0 halfway.
            let t = 0.5 + 0.5 * (i as f32 / FRAMES as f32 * std::f32::consts::TAU).cos();
            let radius = 0.50 + 0.14 * t;
            let alpha = (110.0 + 145.0 * t) as u8;
            draw(move |d| (d < radius).then_some(([240, 160, 30], alpha)))
        })
        .collect()
}

/// Draws a square icon from a function of the distance from the centre (0 at the centre, 1 at
/// the edge) to a colour and opacity.
fn draw(paint: impl Fn(f32) -> Option<([u8; 3], u8)>) -> Icon {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = SIZE as f32 / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt() / c;
            match paint(d) {
                Some(([r, g, b], a)) => rgba.extend([r, g, b, a]),
                None => rgba.extend([0, 0, 0, 0]),
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon")
}
