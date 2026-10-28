<div align="center">

# 🧚 heyListen

**Hey, listen!** Norwegian meeting notes that never leave your Mac.

Record a Teams, Slack, Meet or in-person meeting from your menu bar and watch the transcript appear as people talk.
When you stop, a Markdown note lands on your Desktop with a summary, decisions, tasks and who said what.

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust)
![macOS](https://img.shields.io/badge/macOS-14.4%2B-black?logo=apple)
![Local only](https://img.shields.io/badge/meeting%20data-stays%20local-brightgreen)
![License: MIT](https://img.shields.io/badge/license-MIT-blue)

**[⬇️ Download for Mac (Apple Silicon)](https://github.com/nikolaia/heylisten/releases/latest/download/heyListen-macos-arm64.zip)**

</div>

---

```markdown
---
title: "Kundemøte"
date: 2026-10-01
start: "09:30"
duration_min: 47
speakers: [Meg, Kari, Taler 2]
speaker_names:
  - name: "Kari"
    label: "Taler 1"
    evidence: "Kari, kan du starte med testingen"
type: meeting
tags: [møte]
---

## Sammendrag
Møtet handlet om å øke markedsføringsbudsjettet for neste år …

## Markedsføringsbudsjett
- Kari foreslo å øke budsjettet med 20 % for å nå flere kunder i Bergen.
- Taler 2 mente økningen bør vente til tallene for tredje kvartal er klare.

## Oppgaver
- [ ] Forberede tall for ledermøtet på fredag (ansvarlig: Taler 2)

## Transkripsjon
**Taler 1** [00:00:02]
Hei og velkommen, skal vi starte med budsjettet?
```

## ✨ Features

- 🔒 **Your meetings stay on your Mac.** Recording, transcription and summaries all run locally. The only downloads are the models, once, when you ask for them.
- 🎙️ **Mic and system audio** recorded as two separate tracks. System audio is captured directly by macOS, so there's no BlackHole or virtual driver.
- ⚡ **Live transcript** with [NB-Whisper](https://huggingface.co/NbAiLab/nb-whisper-large), the National Library of Norway's Whisper. By the time you stop, the transcript is already done.
- 🎚️ **Hears quiet voices, ignores noise.** A voice detector ([Silero VAD](https://github.com/snakers4/silero-vad)) decides what's speech by how it sounds, not how loud it is: a quiet mic still works, and music, typing and silence never reach the transcriber.
- 🗣️ **Tells speakers apart**, both remote people and several people sharing one mic in a meeting room.
- 🔁 **Echo cancellation** ([DTLN-aec](https://github.com/breizhn/DTLN-aec)): what your speakers play is subtracted from your mic, so the others' voices and music don't become your lines, even without headphones.
- 📝 **Norwegian summary** from [Borealis](https://huggingface.co/NbAiLab/borealis-12b-gguf), run by heyListen itself through llama.cpp, with no extra install. If you use [Ollama](https://ollama.com), you can pick any of its models instead. You get a short summary, a section per topic with what was said, why and by whom, decisions, `- [ ]` tasks and open questions. Its length follows the meeting, and the prompt is a file you can edit.
- 📄 **Plain Markdown notes** with YAML frontmatter, so they work as-is in any notes app that reads Markdown.
- 💾 **Crash-safe.** Audio streams to disk as it's recorded, so a crash or a closed terminal keeps everything recorded so far.

## 🚀 Getting started

1. **Install it** with [Homebrew](https://brew.sh), which also gives you updates and the `heylisten` command:
   ```bash
   brew install --cask nikolaia/tap/heylisten     # update later: brew upgrade --cask heylisten
   ```
   Or **[download heyListen](https://github.com/nikolaia/heylisten/releases/latest/download/heyListen-macos-arm64.zip)**, unzip it, and move **heyListen.app** to *Applications*.
2. **Open it from Applications.** The app isn't notarized by Apple yet, so macOS blocks it the first time. Go to System Settings → Privacy & Security, scroll down and click **Open Anyway**. Do this once for each new version, before using the `heylisten` command: until then macOS blocks that too.
3. Click the **ring ◯ in your menu bar** and choose **Download models (8.4 GB)…**. This fetches the Norwegian speech model (1.1 GB), the summary engine (12 MB) and the Norwegian summary model (7.3 GB). Progress is shown in the menu. If Ollama already has Borealis, heyListen uses that copy and skips the 7.3 GB.
4. Choose **Start recording**. On the first recording, macOS asks for permission to use the **microphone** and to **record system audio**. Allow both.

| In the menu bar | |
|---|---|
| ◯ ring | Idle |
| 🔴 red dot | Recording; the menu shows for how long |
| 🟠 orange dot | Making the note, or downloading models |

The menu only offers what you can do right now: **Start recording** when idle, **Stop and make note** or **Stop with transcript only** while recording, and progress while the note is made. Below that, always:

| | |
|---|---|
| **Recent notes** | Your last 10 notes; click to open |
| **Settings** | The summary model (built-in Borealis, or any model in your Ollama), where notes go (default: Desktop), and whether recordings are kept for 7 days |
| **About heyListen** | The version, with links to GitHub and its release notes |
| **Report a problem…** | Drafts a GitHub issue with the logs, after asking |

To have heyListen start with your Mac, add it under System Settings → General → Login Items.

## ⌨️ Command line

The app bundles a `heylisten` CLI that does the same things, which is handy for hotkeys (Raycast, Alfred) or for importing files:

```bash
alias heylisten=/Applications/heyListen.app/Contents/MacOS/heylisten   # not needed with Homebrew

heylisten start [title]        # record in the background and show the live transcript (no title: the summary names it)
                               # (Ctrl+C closes the view; recording continues)
heylisten watch                # re-attach the live view
heylisten status               # recording or idle
heylisten stop                 # stop and write the note
heylisten stop --transcript-only   # stop with just the transcript: no speakers, names or summary
heylisten process memo.m4a     # transcribe an audio file (m4a, mp3, wav, flac, ogg)
heylisten process <meeting-id> # make a meeting's note again, from what was transcribed live plus the rest of its audio
heylisten process <meeting-id> --force   # transcribe all of it again from the audio
heylisten setup                # download the models and the summary engine
heylisten doctor               # check everything, with copy-paste fixes
heylisten report               # draft a GitHub issue with the logs (names removed); asks first
```

The tray app and the CLI share one recorder, so you can start a recording in one and stop it in the other.

### Configuration

`~/.config/heylisten/config.toml` is created on first run. Every setting is optional:

```toml
notes_dir = "~/Desktop"                             # where notes go (the tray can set this)
# whisper_model = "~/models/nb-whisper-large-q5_0.bin"  # default: heyListen's models folder
summary_engine = "builtin"                          # or "ollama" (the tray can set this)
ollama_model = "hf.co/NbAiLab/borealis-12b-gguf"    # used with summary_engine = "ollama"
ollama_url = "http://localhost:11434"               # keep it local
keep_audio = false                                  # keep audio after the note (deleted after 7 days)
debug = true                                        # keep audio 7 days + detailed recorder.log (default while tuning)
```

The summary prompt is `~/.config/heylisten/summary-prompt.md`. Edit it to change what the summary looks like.

## 🔍 How it works

```mermaid
flowchart LR
    mic[🎙️ Mic] --> rec
    sys[🔊 System audio<br/>Core Audio tap] --> rec
    rec[Recorder<br/>background process] --> wav[(mic.wav<br/>system.wav)]
    rec --> live[Live transcript<br/>voice detection + NB-Whisper]
    live --> view[Menu bar / heylisten watch]
    wav -- stop --> dia[Speakers<br/>pyannote + CAM++]
    live -- stop --> dia
    dia --> sum[Summary<br/>Borealis via llama.cpp or Ollama]
    sum --> note[📄 Markdown note]
```

- **Two tracks.** The mic is you (`Meg`) and system audio is everyone else (`Andre`), so the live view can tell them apart from the start.
- **Echo.** Without headphones the mic hears the others through the speakers. The recorder cancels the system track out of the mic, sample-aligned, before anything is written or transcribed (about 2 % of one CPU core). Mic lines that still repeat what the system track said are dropped from the note too.
- **Live.** A voice detector finds the speech on each track, which is cut into chunks of a few seconds and transcribed as you talk. Silence, music and noise are left out, and quiet speech is brought up to a normal level before transcribing. Live transcription runs at low priority so it never disturbs the call; if it falls more than a minute behind, the menu and `heylisten status` say so. At stop it catches up at full priority, with progress shown, and then only speaker labelling and the summary are left.
- **Forgotten recordings.** If no one has spoken for 2 minutes, a notification asks whether the meeting is over, with a button to stop. After 15 minutes without speech, heyListen stops by itself and makes the note.
- **Awake.** While recording and making a note, heyListen tells macOS it's doing work you started, so it isn't slowed down by App Nap and the Mac doesn't go to sleep in the middle of a meeting you're only listening to.
- **Speakers.** After stop, voices on both tracks are told apart and numbered `Taler 1`, `Taler 2`… in order of first appearance. If the mic hears only one voice, it stays `Meg`. In a meeting room where several people share a mic, every voice becomes a `Taler`, because heyListen can't know which one is you.
- **Names.** When the meeting makes it clear who someone is, `Taler 2` becomes `Kari`, and `Meg` becomes your name. That means they introduce themselves ("dette er Kari"), or answer right after being addressed or handed over to ("Kari, kan du…?", "Over til deg, Kari"), or are thanked right after speaking ("Takk, Kari"). Code finds these and works out who is who; Borealis only confirms which words are names. Anything ambiguous stays `Taler N` or `Meg`. Names Whisper mishears, often unfamiliar or foreign ones, can't be found. Each name is listed in the note's frontmatter with the label it replaced and the sentence it came from, so a wrong name is easy to fix by hand or by asking an LLM to replace it throughout. Names are found per meeting only, and nothing about anyone's voice is kept.
- **Language.** Always Norwegian. NB-Whisper translates English speech into Norwegian rather than transcribing it.
- **Summary.** By default heyListen starts llama.cpp's `llama-server` on localhost just for the summary, then stops it, so the 7 GB model is only in memory while it's needed.
- **If the summary fails** (for example the model isn't downloaded, or Ollama is down), the note is written without a summary, the audio is kept, and `heylisten process <id>` retries later.
- **If recording stops unexpectedly** (a crash, a Force Quit), the audio so far is saved and the menu offers to finish the meeting. Finishing keeps what was transcribed live and transcribes only the rest.

Meetings (audio while recording, transcripts, metadata) are kept in `~/Library/Application Support/heylisten/meetings/`. The models are in `~/Library/Application Support/heylisten/models/` and the summary engine in `…/heylisten/engine/`. The vocabulary is in [CONTEXT.md](CONTEXT.md) and the design decisions in [docs/adr](docs/adr).

## 🔒 Privacy & security

**Your meeting data stays on your Mac.** Audio, transcripts and summaries are written only to your disk. The built-in summary engine listens only on `127.0.0.1`, and only for the length of a summary. It also requires a random key that's new each time, so other programs can't use it meanwhile. With the Ollama engine, transcripts go to `ollama_url`, which is `localhost` unless you change it. If you point it anywhere else, heyListen warns you in red every time.

**Downloads only happen when you ask**, by clicking *Download models* or running `heylisten setup`, and each one is verified:

| What | From | Verified by |
|---|---|---|
| NB-Whisper speech model (1.1 GB) | Hugging Face | SHA-256 pinned in the source |
| Borealis summary model (7.3 GB) | Hugging Face | SHA-256 pinned in the source |
| llama.cpp summary engine (12 MB) | llama.cpp's GitHub releases | SHA-256 pinned in the source |
| Speaker-recognition and voice-detection models | GitHub (at build time) | SHA-256 pinned in `build.rs` |
| Ollama models (Ollama engine only) | Your Ollama | Ollama's own digest checks |

Downloads use HTTPS only, redirects included. A file that doesn't match its checksum is deleted, never used, so someone intercepting the connection (or a compromised mirror) can only make the download fail. Interrupted downloads resume, and the checksum covers the whole file, resumed part included. See [ADR 0004](docs/adr/0004-models-downloaded-on-request.md) and [ADR 0005](docs/adr/0005-built-in-summary-engine.md).

**Releases.** The app is built by [GitHub Actions](.github/workflows/release.yml) straight from this repository. Every action is pinned to a commit SHA, dependencies are locked (`Cargo.lock`) and checked with `cargo audit`, and each release includes its SHA-256 and a signed build-provenance attestation. To check a download:

```bash
shasum -a 256 -c SHA256SUMS                                    # matches the release's checksum
gh attestation verify heyListen-macos-arm64.zip --repo nikolaia/heylisten   # built by this repo's workflow
```

**What heyListen doesn't protect against:** other software running as your user. It can read your meeting folders and notes just like any other files you own. The app isn't notarized by Apple yet (see below), so your trust rests on the checks above.

## 🍎 macOS permissions

- **"Apple could not verify heyListen":** the app is ad-hoc signed, not notarized. Click *Open Anyway* under System Settings → Privacy & Security, or run `xattr -dr com.apple.quarantine /Applications/heyListen.app`.
- **Microphone and system audio:** the recorder runs from a small helper app that heyListen keeps in its data folder, so the permissions belong to heyListen and not to whichever terminal or hotkey app started it. Approve once; updates don't ask again.
- **The very first recording** after you click Allow may have an empty system-audio track. Later ones are fine.
- **To check:** System Settings → Privacy & Security → *Microphone*, and *Screen & System Audio Recording* → *System Audio Recording Only*. Both should list heyListen.

## 🛠️ Building from source

```bash
git clone https://github.com/nikolaia/heylisten && cd heylisten
nix develop -c cargo build --release --features tray   # or use rustup + cmake
nix develop -c scripts/bundle-macos.sh                  # → target/heyListen.app
nix develop -c scripts/install-dev.sh                   # build, install in /Applications and restart
nix develop -c scripts/check.sh                         # what CI checks: tests, clippy, cargo audit
git config core.hooksPath .githooks                     # run check.sh before every push
```

The build downloads sherpa-onnx and small speaker-recognition and voice-detection models, which get embedded in the binary. Pushing a `v*` tag runs [the release workflow](.github/workflows/release.yml), which builds the app and attaches the zip to a GitHub Release.

**Debugging:** *Keep recordings for 7 days (debug)* is on by default while heyListen is being tuned. Turn it off under **Settings** in the menu, or with `debug = false` in the config. It keeps each meeting's audio so `heylisten process <id>` can redo the note, and logs the devices, audio errors, how much audio each track receives, when the mic stops delivering audio, how far behind live transcription is (every minute), and the speaker turns to `recorder.log` in the meeting folder. **Kept audio is always deleted after 7 days**, whatever the settings; transcripts and notes stay.

**Reporting a problem:** *Report a problem…* in the menu (or `heylisten report`) drafts a GitHub issue with your versions, a few settings and the last lines of the logs, with names, meeting titles and your home folder removed. It asks first, and only opens the draft in your browser: nothing is sent unless you review it and submit it there.

**Long meetings:** `scripts/scale-test.sh` runs the whole pipeline on synthetic 2-, 4- and 8-minute meetings (a few minutes in total) and predicts what an hour will take. On a MacBook Pro M3 Pro:

| For a 60-minute meeting | |
|---|---|
| Live transcription | keeps up at 4× real time (two tracks need 2×) |
| After stop: speakers | ~4 min |
| After stop: summary | ~30–60 s (one pass; meetings over ~2.5 h are summarized in parts) |
| Peak memory | ~10 GB while summarizing (mostly the 7 GB summary model), ~1.6 GB while recording |

## 🗺️ Status

| | |
|---|---|
| Menu bar app with first-run setup, recent notes, notes location | ✅ macOS |
| Mic + system audio, live transcript, echo cancellation | ✅ macOS |
| Speakers, including several people on one mic | ✅ |
| Norwegian summary, built in (llama.cpp) or via Ollama | ✅ (built-in engine: Apple Silicon only so far) |
| Linux (PipeWire recording, tray via AppIndicator) | 🚧 not implemented or tested yet |
| Notarized app | 💭 needs an Apple Developer ID |

## 🙏 Credits

- [NB-Whisper](https://huggingface.co/NbAiLab/nb-whisper-large) and [Borealis](https://huggingface.co/NbAiLab/borealis-12b-gguf) from the National Library of Norway.
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) via [whisper-rs](https://crates.io/crates/whisper-rs).
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) via [sherpa-rs](https://github.com/thewh1teagle/sherpa-rs), with [pyannote segmentation 3.0](https://huggingface.co/pyannote/segmentation-3.0) (MIT), [3D-Speaker CAM++](https://github.com/modelscope/3D-Speaker) (Apache-2.0) and [Silero VAD](https://github.com/snakers4/silero-vad) (MIT).
- Echo cancellation: [DTLN-aec](https://github.com/breizhn/DTLN-aec) by Nils L. Westhausen and Bernd T. Meyer (MIT), with processing adapted from [Anarlog](https://github.com/fastrepl/anarlog)'s `crates/aec` by Fastrepl (MIT), run with [tract](https://github.com/sonos/tract). Full notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
- [llama.cpp](https://github.com/ggml-org/llama.cpp), [tray-icon](https://github.com/tauri-apps/tray-icon), [rfd](https://github.com/PolyMeilex/rfd) and [Ollama](https://ollama.com).
- Ideas from [vaqlo](https://github.com/ivshestakov/vaqlo.app).

## Contributing

Bug reports and ideas are welcome; pull requests aren't accepted. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE). Third-party notices: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
