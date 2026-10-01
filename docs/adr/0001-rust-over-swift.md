# Rust over Swift, even though macOS is the primary platform

heyListen targets macOS first (and a Mac tray app later), but the CLI must also run on Linux. Swift on Linux lacks AVFoundation, CoreML and FluidAudio, so its advantages only exist on one platform. We chose a Rust core with platform-specific capture backends behind a trait. We accept two costs: macOS system-audio capture (Core Audio process taps) has to go through `objc2` bindings, with a small Swift helper as the fallback, and diarization uses cross-platform ONNX models instead of FluidAudio on the Neural Engine.
