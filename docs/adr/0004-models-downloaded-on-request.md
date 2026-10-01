---
status: accepted, supersedes ADR-0002
---

# Models are downloaded on request; meeting data never leaves the machine

To make heyListen work for people who download the app rather than build it, `heylisten setup` (and the tray's "Download models" item) fetches the NB-Whisper model from a pinned Hugging Face URL over HTTPS only (redirects included), checks its SHA-256 (a mismatch deletes the file), and asks the local Ollama to pull the summary model. That is the only time heyListen opens an internet connection, and it only happens when the user asks for it; recording, transcription and summarizing never touch the network. Audio and transcripts still only ever go to the configured Ollama URL, which defaults to localhost, and a non-local URL still produces a red warning on every run.
