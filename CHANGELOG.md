# Changelog

What changed in each release. Each release's GitHub page shows its section from this file.

## [Unreleased]

### Added
- `heylisten process <meeting-id> --force` transcribes all of a meeting again from its audio. Without `--force`, it continues from what the live transcript already has.

### Fixed
- **Telling speakers apart is faster**, about 40 % for a long meeting: it only looks at the stretches where someone speaks (a track is often mostly silence: your mic while you listen, either side of a 1:1), and does both tracks at once. On a 44-minute meeting, from 3 minutes to under 2, with the same speakers found.
- **Finishing a recording that stopped unexpectedly is quick.** If the recorder crashed or was force-quit, finishing the meeting used to transcribe all of it again from the audio, which for a 44-minute meeting on an M1 Pro looked like a hang, also with **Stop with transcript only**. It now keeps what the live transcript already has and transcribes only the rest: in that meeting, the last 3 minutes.
- **Stop works after the mic has disappeared**, as when AirPods are put back in their case before stopping. The mic goes quiet without an error, and stop could hang closing it or waiting for its audio; now it leaves a quiet mic alone and doesn't wait for audio that won't come.

## [0.1.3] - 2026-10-27

Fuller summaries, progress while the note is made, names for everyone, and a tidier menu.

### Added
- **Fuller summaries.** The summary now has a section per topic with what was actually said, why, and by whom (numbers, names, reasons and alternatives as they were said), then decisions, tasks and open questions. Its length follows the meeting: more topics for a long meeting, and no padding for a short one. Long meetings summarized in parts keep every name, number, decision and task when the parts are merged. An unedited summary prompt is updated; an edited one is left alone.
- **A tidier menu.** It only offers what you can do right now (no greyed-out Start and Stop), says what's going on in a short headline with the details under it, and gathers the summary model, notes folder and keeping recordings under **Settings**.
- **About heyListen** in the menu: the version (as the `heylisten` command reports it), the notes folder, and links to GitHub and that version's release notes.
- **Progress while making the note.** After stop, the menu and `heylisten stop` show how much is left of catching up on the live transcript ("about 2 min of audio left"), then how far the summary is: reading the meeting (percent), then words written. In a terminal the CLI draws a progress bar; piped or logged, it prints only the steps.
- **A warning when live transcription falls behind** by more than a minute during a recording, in the menu and in `heylisten status`, mentioning Low Power Mode when it's on. `recorder.log` notes the backlog every minute, and when the mic stops delivering audio.

### Fixed
- **Stopping no longer seems to hang on a slower or busy Mac.** macOS could slow heyListen down with App Nap while it ran in the background, and catching up on the live transcript ran at low priority with nothing shown. heyListen now tells macOS that recording and making a note are work you started (which also keeps the Mac from sleeping during a meeting), catches up at full priority, and shows its progress.
- **Handovers name people.** "Over til deg, Sindre" or "Kan du ta resten, Kari?" now names whoever speaks next; before, only a name at the start of a sentence counted ("Kari, kan du…"). A remark like "Rosa, kanskje." doesn't: an address has to be a question or say "du", "deg" or "dere".
- **You get your name too.** When the meeting makes it clear ("Kan du ta over, Torbjørn?"), `Meg` becomes your name in the note and the summary, listed in the frontmatter with its evidence like everyone else's.

### Known limitations
Still as in 0.1.0: not notarized by Apple (click *Open Anyway* under System Settings → Privacy & Security after each install or upgrade), Apple Silicon and macOS only, speaker separation still being tuned, debug mode on by default (audio kept 7 days), and no Teams mode yet.

## [0.1.2] - 2026-10-23

Quiet mics, and Homebrew.

### Fixed
- **Quiet mics are transcribed.** Speech was found by volume, so a quiet mic (a MacBook's built-in one at a distance, or an interface with low gain) was taken for silence and dropped. A voice detector (Silero VAD) now decides what's speech by how it sounds, and quiet speech is brought up to a normal level before transcribing. Music, typing and silence no longer reach the transcriber, which also makes transcription faster (about 14 s per minute of meeting instead of 19; live keeps up at 4× real time).
- **No made-up meeting when nothing was said.** Starting and stopping a recording right away gave the summary model an empty transcript, and it invented a whole meeting. Now a recording where nobody spoke gets no note, and one with under 30 words gets the transcript without a summary. The summary prompt no longer has an example title for the model to copy; an unedited prompt is updated.

### Added
- **Homebrew:** `brew install --cask nikolaia/tap/heylisten`, and `brew upgrade --cask heylisten` for updates. The [tap](https://github.com/nikolaia/homebrew-tap) follows releases on its own, and only moves to a release whose zip matches its checksum and was built by this repo's release workflow.

### Known limitations
Still as in 0.1.0: not notarized by Apple (click *Open Anyway* under System Settings → Privacy & Security after each install or upgrade), Apple Silicon and macOS only, speaker separation still being tuned, debug mode on by default (audio kept 7 days), and no Teams mode yet.

## [0.1.1] - 2026-10-23

Fixes from the first round of testing by colleagues, and meeting titles.

### Added
- **Meeting titles:** the summary model names each meeting ("Planlegging av høstlanseringen"), and the note is called that instead of "Møte 08:46". A title you give when starting is kept. An unedited summary prompt from an earlier version is updated to ask for the title; an edited one is left alone.
- **Report a problem…** in the menu (`heylisten report`): drafts a GitHub issue with your versions, a few settings and the last lines of the logs, with names, meeting titles and your home folder removed. It asks first and only opens the draft in your browser; nothing is sent unless you submit it.
- **Stop without summary** in the menu (`heylisten stop --transcript-only`): just the transcript, in seconds.

### Fixed
- **The menu bar icon no longer vanishes when a recording starts.** On a MacBook with a camera notch and a full menu bar, the timer next to the icon made it wider, and macOS hid it behind the notch. The timer is now only in the menu. Opening heyListen while it runs now shows what it's doing, with a way to stop the recording, in case the icon is hidden anyway.
- **A recording that was force-quit can be finished.** Its audio files were left unfinished (an empty file, or a header that hadn't caught up), and finishing the meeting failed on them. heyListen now reads whatever audio was written. A meeting where nobody spoke says so, instead of claiming its audio was deleted.
- **A recording that stopped unexpectedly no longer leaves the menu stuck.** The menu says what happened and offers to finish the meeting (Start waits until then), instead of disabling Stop. The menu bar app also logs its own errors and crashes to `tray.log` in heyListen's data folder.
- **Others sounding like a duck with AirPods.** In call mode, AirPods run at 24 kHz while the system audio tap reported 48 kHz, so other participants were recorded at double speed and transcribed as "kvakk kvakk". heyListen now uses the device's real rate and checks it against how fast audio arrives.
- **Long monologues no longer slow down telling speakers apart.** Each speaker's voice is measured from at most 60 s of audio, as intended; a long turn used to go in whole.
- **Security:** rustls 0.23.46, rustls-webpki 0.103.16 and webpki-roots 1.0.11, for advisories in the libraries behind heyListen's HTTPS downloads (RUSTSEC-2026-0346 to -0349, -0381, -0382).

### Known limitations
Still as in 0.1.0: not notarized by Apple (click *Open Anyway* under System Settings → Privacy & Security on first launch), Apple Silicon and macOS only, speaker separation still being tuned, debug mode on by default (audio kept 7 days), and no Teams mode yet.

## [0.1.0] - 2026-10-02

The first release. heyListen records, transcribes and summarizes Norwegian meetings on your Mac, and nothing leaves the machine.

### What's in it
- **Menu bar app** with first-run setup: downloads the models (~8.4 GB), shows your recent notes, and lets you choose the summary model and where notes go.
- **Records your mic and the Mac's sound** (Teams, Slack, Meet or anything else), with no virtual audio driver. Audio streams to disk, so a crash keeps what's recorded so far.
- **Live transcription** with NB-Whisper large from the National Library of Norway, written to the note as the meeting goes.
- **Echo cancellation** on the mic, so meetings on laptop speakers aren't transcribed twice.
- **Tells speakers apart** on both tracks, including several people in a room around one mic. Each word goes to the speaker who said it.
- **Finds names** when the meeting makes them clear (introductions, being addressed, being thanked). Each one is listed in the frontmatter with its evidence, so a wrong name is easy to fix.
- **Summary, decisions, tasks and topics** with Borealis, run locally by a built-in llama.cpp, or by your own Ollama.
- **Plain Markdown notes** with YAML frontmatter.
- **The `heylisten` CLI** for everything the app does: `start`, `stop`, `status`, `watch`, `process`, `setup`, `doctor`.
- **Security:** downloads use HTTPS only and are checked against pinned SHA-256s. Releases are built by GitHub Actions with pinned actions, `cargo audit`, checksums and a signed build-provenance attestation.
- **Privacy:** kept audio is deleted after 7 days, whatever the settings.

### Known limitations
- **Not notarized by Apple.** macOS blocks the first launch: click *Open Anyway* under System Settings → Privacy & Security.
- **Apple Silicon and macOS only.** Linux is planned.
- **Speaker separation is still being tuned.** It can split one person into two speakers, or put a few lines on the wrong person, especially in quick exchanges or when someone changes their voice. Notes show `Taler N`; fix them by search and replace.
- **Live transcription keeps up at about 3× real time** on an M3 Pro, with two tracks needing 2×. It hasn't been tested on slower Macs or during very heavy video calls.
- **Debug mode is on by default** while heyListen is being tuned: it keeps each meeting's audio for 7 days, so a note can be redone. Turn it off in the menu.
- **Teams mode** (record only Teams, silence the mic while you're muted) isn't in this release.

[Unreleased]: https://github.com/nikolaia/heylisten/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.3
[0.1.2]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.2
[0.1.1]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.1
[0.1.0]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.0
