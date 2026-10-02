# Teams mode: record only what's needed during Teams calls

## Context

You were in a Teams meeting, muted, and took a phone call. heyListen recorded the mic throughout, so the call ended up in the transcript and the summary. heyListen's ideal is to record only what it must, so during Teams calls it switches into **Teams mode** automatically:

1. **System track: only Teams.** Spotify, notifications and a phone call through the Mac are left out.
2. **Mic track: nothing while you're muted.** Your mic audio is held briefly, until Teams' log confirms your mute state for that moment. Audio from muted stretches becomes silence before anything is written to disk or transcribed. Words right after you unmute aren't lost.
3. **Afterwards:** muted stretches are recorded as metadata, shown as a placeholder in the note, and kept out of the summary.

Constraints:
- **No IT involvement** and no extra permissions. Your org has Teams' local API disabled (Teams logs `third_party_devices_manager_ is not available`, and the setting is hidden), so it can't be the mechanism.
- **Start and stop stay manual.**
- **No manual "pause mic" fallback.**
- **Teams mode switches on automatically** during Teams calls.
- **When the mute state is unknown, record the mic** and flag it (your choice).

## Measured on your Mac (2026-10-02)

**Teams' audio**
- Teams keeps the mic claimed while you're muted, so mic use can't reveal muting.
- Call audio (mic and speaker) runs in **`com.microsoft.teams2.modulehost`**. Its `kAudioProcessPropertyIsRunningInput` and `IsRunningOutput` are both on during a call, which makes a reliable "in a Teams call" signal.

**Teams' log**
- Location: `~/Library/Group Containers/UBF8T346G9.com.microsoft.teams/Library/Application Support/Logs/MSTeams_*.log`. Readable without a permission prompt, rotates at about 2 MB.
- **Timestamps are UTC**, despite being labelled `+02:00`. `06:45:48…+02:00` was 08:45:48 local; the WebView2 lines inside the log carry the real local time.
- **Written in 4 KB blocks:** the newest line was 5–25 s old when its block landed (measured 07:03 UTC). That's why a fixed back-buffer won't work, and the gate below waits for the log instead.

**Events found in the log**

| Event | Log line | Notes |
|---|---|---|
| Join | `Window: (… name=<id>; …) Close prompt: ClosePromptInfo{prompt=meeting; source=Web}` | a new window ID per join, so a dropout and rejoin is a new join |
| Mute change | `AirPodsModuleProxy: AirPodsModule::SetMuteState: OOP call` | no direction; one per toggle |
| Leave | `Window: (… name=<id>; …) Close prompt: std::nullopt` | the **same window ID** as its join; no `SetMuteState` on leaving |
| Ignore | windows tagged `CallMonitor` | the small call popup when Teams isn't focused; opens and closes on its own |

Today's sequence, local time:
- 08:04:33 join (muted), 08:07:50 rejoin after audio problems (muted). Each logged one `SetMuteState`.
- 08:45:48 and 08:45:52: two test toggles, one line each.
- 09:05:39: leave, window `37b73e7b…`, the same as the 08:07:50 join.

**Still unknown, to test first** (solo "Meet now": join *unmuted*, mute, unmute, leave):
- Does joining **unmuted** log a `SetMuteState`? If not, "count changes since the join, odd = muted" holds. If it does, the log can't tell direction, and only the Teams-only audio part ships.

**Watch out:** a Teams update is already announced in your log (`upcomingVersion 26246.1709.5146.8945`). The log format is undocumented and can change, so detection must recognise its own failure.

## Design

### 1. `src/teams.rs` (new)
- **`in_call()`:** whether `com.microsoft.teams2.modulehost` has input and output running. Uses the Core Audio process properties, like the existing `get()` helper in `src/capture/system_macos.rs`; it lives in the macOS capture module, with a `false` stub elsewhere.
- **`LogWatcher`:**
  - **On start:** reads the newest `MSTeams_*.log` (and the previous file, if needed) back to the latest join without a matching leave, to get the current state.
  - **Then tails it** every 250 ms, following rotation to new files.
  - **Parses** joins, leaves and `SetMuteState` into a **`MuteTimeline`**: transitions in meeting-relative ms (via `Meeting::start`), plus a **watermark**, the newest log timestamp seen, meaning "the timeline is complete up to here". The count resets on each join.
  - **Timestamp sanity check:** read as UTC, the newest line must be within minutes of now. Otherwise the timeline is marked **unrecognised**.
  - **Pure parsing functions**, tested on real excerpts from today.
- Shared as `Arc<Mutex<MuteTimeline>>`. Every transition and state change is written to `recorder.log` via `recorder::log` and to `meeting.dir()/teams.jsonl`.

### 2. Mic privacy gate (`src/recorder.rs`, `track_writer`, mic track only)
- **Where:** between the resampler's 16 kHz output (`out`) and `wav.write` / `feed`. A `MicGate` holds samples with their absolute sample index.
- **Release rule:** samples captured before the timeline's watermark are released, silenced where the timeline says muted. A chunk that spans a transition is split at the exact sample.
- **Hold cap: 60 s** (about 4 MB). Past the cap, or when the timeline is unrecognised, the oldest audio is released **as recorded**, and the stretch is logged as unconfirmed.
- **Outside a Teams call**, the gate passes everything straight through (no delay).
- **Output stays the same number of samples.** The existing gap filling (`due`/`gap`), the chunker, the live transcriber and `audio::has_speech` work unchanged, and silence is skipped as now.
- **The live view** shows your own lines with the log's delay (typically 5–30 s). The others' lines stay instant.
- **On stop:** wait until the watermark passes the stop time, at most 30 s (the tray/CLI shows "Waiting for Teams' log…"), then flush using the same rule.

### 3. Teams-only system audio (`src/capture/system_macos.rs`, `src/recorder.rs`)
- **`System::start(sink, scope)`**, with `Scope::Everything` (today) or `Scope::Apps(["com.microsoft.teams2.modulehost"])`, via `CATapDescription::setBundleIDs`, or process objects from `kAudioHardwarePropertyProcessObjectList` plus `kAudioProcessPropertyBundleID`.
- **The recorder's `run()` loop** checks `in_call()` every second. On a change, it swaps the `System` capture (a new tap with a clone of the same `sys_tx`), and switches the gate and the log watcher on or off. The gap filling covers the swap.
- Stubbed to `Everything` on other platforms, like `capture/mod.rs`'s Linux stub.

### 4. After stop: metadata and the note
- **`Meeting`:** `teams: Option<TeamsInfo { muted: Vec<Interval>, unconfirmed: Vec<Interval>, source }>`, filled from `teams.jsonl`.
- **`Segment`:** `#[serde(default)] muted: bool`. The pipeline (`src/pipeline.rs`) marks `Me` segments that overlap muted intervals, before diarization, `names::find` and `Writer::summarize`. That's a safety net in case anything slipped through.
- **`Transcript::text()` and `summarize::split()` skip muted segments.** `note::render` shows `*(dempet 00:12:03–00:14:40)*` in place of each muted stretch.
- **Frontmatter**, next to `speaker_names`:
  ```yaml
  # Teams mode: Teams audio only; your mic was silenced while muted in Teams.
  teams:
    muted: ["00:12:03–00:14:40"]
    unconfirmed: []   # stretches where the mute state couldn't be confirmed, recorded as is
  ```

### 5. Visible state and docs
- **Status:** the recorder's `State` (`recorder.json`) gets `teams_mode: bool` and `muted: Option<bool>`. `heylisten status` and the tray status line show "· Teams mode" / "· muted".
- **`CONTEXT.md`:** **Teams mode** and **Muted** terms.
- **ADR `0006-teams-mode.md`:** why the log and not the API (the IT constraint); why gating waits for the log (it's written in 4 KB blocks); and the unknown-state choice.
- **README:** a Teams mode section.

## Further ideas (not in scope yet)
- **Trim** audio from before you joined or after you left the Teams call, using the join and leave markers.
- **Meeting title:** if the Teams log or window title has the meeting's subject, use it as the note title instead of "Møte HH:MM".
- **Participants:** if the log lists attendee names, offer them as candidates to `names::find`, but only confirmed by the existing transcript rules.
- **Dropouts:** a rejoin is a new join with its own window ID, which could be noted in the transcript (`*(falt ut 08:04–08:07)*`).
- **Teams local API** for orgs that allow it (pairing via `{"action":"pair",…}` once `canPair` is true, and `tokenRefresh`): an exact mute state, which could replace the log there.

## Files
- **new:** `src/teams.rs`, `docs/adr/0006-teams-mode.md`
- **changed:** `src/recorder.rs` (gate, `run()` loop, `State`), `src/capture/system_macos.rs` and `mod.rs` (`Scope`, `in_call`), `src/meeting.rs`, `src/transcript.rs`, `src/pipeline.rs`, `src/note.rs`, `src/summarize.rs`, `src/main.rs`, `src/bin/heylisten-tray.rs`, `src/lib.rs`, `CONTEXT.md`, `README.md`
- **reused:** `recorder::log`, `track_writer` gap filling, the `live::Chunker` pipeline, the `get()` Core Audio helper, `config::data_dir`, debug mode

## Verification
1. **First, a solo "Meet now", 2 minutes:** join **unmuted**, mute, unmute, leave. Read the log to settle the parity question. If an unmuted join also logs `SetMuteState`, the log can't tell direction: then only Teams-only audio ships, and mute gating waits for another source.
2. **Unit tests:**
   - log parsing on real excerpts (join-muted, rejoin, toggles, leave, `CallMonitor` windows ignored, rotation, UTC check, unrecognised format)
   - the gate: release only before the watermark, split at a transition, the cap releases as recorded, passthrough outside calls, same sample count out as in
   - segment marking and note placeholders
   - summary input without muted text
3. **End to end, solo Teams call, debug mode** (keeps audio):
   - talk; mute and keep talking (as if on the phone); unmute and talk at once; play Spotify meanwhile
   - check `mic.wav` is silent exactly in the muted stretch
   - check the first words after unmuting are present
   - check `system.wav` has no Spotify
   - check the note's placeholder, the frontmatter and the summary
   - check the `recorder.log` events
4. **Regression:** no Teams call means behaviour as today (no delay). `scripts/scale-test.sh` gives unchanged numbers.
5. **CPU and memory while recording in Teams mode:** sample the recorder's CPU and RSS with `ps`. Expect a negligible increase (log tail every 250 ms, ≤4 MB buffer).
6. **Never build or install while a recording is running.** Replacing the app's files under a live recorder can crash it.
