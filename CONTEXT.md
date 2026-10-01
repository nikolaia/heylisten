# heyListen

A local-only tool that listens to conversations (mostly Norwegian meetings in Teams, Slack Huddles, Google Meet and similar), and turns them into a transcript and a summary. Nothing leaves the machine.

## Language

### Meetings

**Meeting**:
One conversation and everything heyListen produces for it: its recording, transcript and summary. It can also be created from an existing audio file.
_Avoid_: Session, call, conversation

**Recording**:
The captured audio of a meeting, made up of tracks.
_Avoid_: Audio file, capture

**Track**:
One of a recording's audio streams: the *mic* track (Me, and anyone in the room with Me) or the *system* track (people heard through the computer).
_Avoid_: Channel, stream, source

### People

**Me**:
The person running heyListen. A meeting has a Me only when the mic track holds a single voice; in a room with several people, nobody can be told to be Me.
_Avoid_: User, host, local speaker

**Others**:
Everyone heard on the system track, before diarization tells them apart.
_Avoid_: Remote, participants, them

**Speaker**:
One distinct voice identified by diarization, on either track. Speakers on different tracks are always different people.
_Avoid_: Participant, attendee, voice

### Output

**Transcript**:
The timestamped, speaker-labelled text of everything said in a meeting. It grows live during the meeting; after the meeting, only its speaker labels change.
_Avoid_: Transcription (that's the process, not the result), text, captions

**Summary**:
A short Norwegian write-up of a transcript (overview, decisions, tasks, topics) produced by a local language model.
_Avoid_: Notes, minutes, recap

### Processes

**Recorder**:
The background process that owns an ongoing meeting: it captures the tracks and grows the live transcript.
_Avoid_: Daemon, server, worker

**Viewer**:
Anything that attaches to the recorder to show or control a meeting (the terminal view, status, a future tray app). It never owns the meeting.
_Avoid_: Client, UI, frontend
