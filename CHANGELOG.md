# Changelog

What changed in each release. Each release's GitHub page shows its section from this file.

## [Unreleased]

### Added
- **Meeting titles:** the summary model names each meeting ("Planlegging av høstlanseringen"), and the note is called that instead of "Møte 08:46". A title you give when starting is kept. An unedited summary prompt from an earlier version is updated to ask for the title; an edited one is left alone.
- **Report a problem…** in the menu (`heylisten report`): drafts a GitHub issue with your versions, a few settings and the last lines of the logs, with names, meeting titles and your home folder removed. It asks first and only opens the draft in your browser; nothing is sent unless you submit it.
- **Stop without summary** in the menu (`heylisten stop --transcript-only`): just the transcript, in seconds.

### Fixed
- **The menu bar icon no longer vanishes when a recording starts.** On a MacBook with a camera notch and a full menu bar, the timer next to the icon made it wider, and macOS hid it behind the notch. The timer is now only in the menu. Opening heyListen while it runs now shows what it's doing, with a way to stop the recording, in case the icon is hidden anyway.
- **A recording that was force-quit can be finished.** Its audio files were left unfinished (an empty file, or a header that hadn't caught up), and finishing the meeting failed on them. heyListen now reads whatever audio was written. A meeting where nobody spoke says so, instead of claiming its audio was deleted.
- **A recording that stopped unexpectedly no longer leaves the menu stuck.** The menu says what happened and offers to finish the meeting (Start waits until then), instead of disabling Stop. The menu bar app also logs its own errors and crashes to `tray.log` in heyListen's data folder.
- **Others sounding like a duck with AirPods.** In call mode, AirPods run at 24 kHz while the system audio tap reported 48 kHz, so other participants were recorded at double speed and transcribed as "kvakk kvakk". heyListen now uses the device's real rate and checks it against how fast audio arrives.
- **Security:** rustls 0.23.46, rustls-webpki 0.103.16 and webpki-roots 1.0.11, for advisories in the libraries behind heyListen's HTTPS downloads (RUSTSEC-2026-0346 to -0349, -0381, -0382).

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

[Unreleased]: https://github.com/nikolaia/heylisten/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.0
