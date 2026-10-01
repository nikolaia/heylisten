---
status: superseded by ADR-0004
---

# No network traffic except the configured Ollama endpoint

The whole point of heyListen is that audio and transcripts never leave the machine. The binary contains no networking code except calls to the Ollama API. It does not download models: v1 is for power users, and `doctor` prints the exact commands to fetch anything that's missing. `ollama_url` defaults to localhost. A non-loopback URL is allowed but produces a prominent red warning every time it's used, because it sends transcripts off the machine.
