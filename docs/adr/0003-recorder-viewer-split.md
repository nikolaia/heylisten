# Recorder and viewers are separate processes

A meeting is owned by one background Recorder process. The terminal view, `status`, and the future tray app are Viewers that attach to it. Closing a viewer (or the terminal) never stops a recording. A single foreground process would be simpler, but it would lose audio when the terminal dies and would have to be restructured anyway for the tray app. Don't merge them.
