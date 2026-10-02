# Changelog

What changed in each release. Each release's GitHub page shows its section from this file.

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

[0.1.0]: https://github.com/nikolaia/heylisten/releases/tag/v0.1.0
