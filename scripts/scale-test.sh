#!/usr/bin/env bash
# Checks that long meetings will work without recording one: runs the full pipeline on
# synthetic meetings of a few lengths (default 2, 4 and 8 minutes), then prints how each step
# scales and what that predicts for 60 minutes. If the cost per minute stays flat as the
# length doubles, an hour behaves the same. Also measures CPU time and memory (heyListen plus
# its summary engine together). Takes a few minutes. macOS only (uses `say`).
#
#   scripts/scale-test.sh [minutes...]
#
# Notes are written to your notes folder as "Skalatest N min (test)".
set -euo pipefail
cd "$(dirname "$0")/.."
heylisten=${HEYLISTEN:-target/release/heylisten}
minutes=("${@:-2 4 8}")
minutes=(${minutes[@]})
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# One ~70 s round: two voices taking turns, then a long Norwegian monologue.
round() {
    local i=0
    while IFS='|' read -r voice text; do
        i=$((i + 1))
        say -v "$voice" -o "$work/t$i.aiff" "$text"
        ffmpeg -loglevel error -y -i "$work/t$i.aiff" -ar 16000 -ac 1 -c:a pcm_s16le "$work/t$i.wav"
        ffmpeg -loglevel error -y -f lavfi -i anullsrc=r=16000:cl=mono -t 0.8 -c:a pcm_s16le "$work/g$i.wav"
        printf "file 't%s.wav'\nfile 'g%s.wav'\n" "$i" "$i" >> "$work/round.txt"
    done <<'EOF'
Nora|Hei, og velkommen. Skal vi starte med budsjettet?
Daniel|Yes, I think the budget for marketing needs to increase next year.
Nora|Hvor mye mer trenger dere, omtrent?
Daniel|Around twenty percent more, mainly for the launch campaign in November.
Nora|Ok, da tar vi det med til ledermøtet på fredag.
Nora|Neste sak er serverrommet. Vi vurderer å kjøpe en ny strømforsyning som kan håndtere strømbrudd i opptil en halvtime, og Ola skal undersøke prisene før neste uke. Testingen av appen er ikke ferdig ennå, så Kari tar ansvar for å fullføre den innen fredag. Til slutt: husk at julebordet er den tolvte desember.
EOF
    ffmpeg -loglevel error -y -f concat -safe 0 -i "$work/round.txt" -c copy "$work/round.wav"
}

echo "Generating test speech…"
round
round_secs=$(python3 -c "import wave; w = wave.open('$work/round.wav'); print(w.getnframes() / 16000)")

results="$work/results.tsv"
for m in "${minutes[@]}"; do
    n=$(python3 -c "import math; print(max(1, round($m * 60 / $round_secs)))")
    : > "$work/list.txt"
    for _ in $(seq "$n"); do echo "file 'round.wav'" >> "$work/list.txt"; done
    file="$work/Skalatest $m min (test).wav"
    ffmpeg -loglevel error -y -f concat -safe 0 -i "$work/list.txt" -c copy "$file"
    echo "Running $m min…"
    # Sample the combined memory of heyListen and llama-server twice a second.
    ( peak=0; while sleep 0.5; do
        kb=$(ps -Ao rss=,comm= | awk '/heylisten$|llama-server$/ { s += $1 } END { print s + 0 }')
        [ "$kb" -gt "$peak" ] && peak=$kb && echo "$peak" > "$work/peak.txt"
      done ) &
    sampler=$!
    /usr/bin/time -l "$heylisten" process "$file" > "$work/log.txt" 2>&1
    kill "$sampler" 2>/dev/null || true
    python3 - "$work/log.txt" "$file" "$results" "$work/peak.txt" <<'PY'
import re, sys, wave
log, wav, out = open(sys.argv[1]).read(), sys.argv[2], sys.argv[3]
took = {k: float(v) for k, v in re.findall(r"(\w+) took ([\d.]+) s", log)}
real, user, sys_ = map(float, re.search(r"([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys", log).groups())
peak = int(open(sys.argv[4]).read()) / 1024
note = re.search(r"Note: (.*\.md)", log)
words = 0
if note:
    text = open(note.group(1)).read().split("## Transkripsjon", 1)[-1]
    words = len([w for w in text.split() if "**" not in w and not w.startswith("[")])
w = wave.open(wav); minutes = w.getnframes() / 16000 / 60
with open(out, "a") as f:
    f.write(f"{minutes:.2f}\t{took.get('transcription', 0)}\t{took.get('speakers', 0)}\t{took.get('summary', 0)}\t{peak:.0f}\t{words}\t{real}\t{user + sys_}\n")
PY
done

python3 - "$results" <<'PY'
import sys
rows = [list(map(float, l.split("\t"))) for l in open(sys.argv[1])]
print()
print(f"{'length':>8} {'transcribe':>11} {'speakers':>9} {'summary':>8} {'total':>8} {'CPU time':>9} {'avg CPU':>8} {'peak mem':>9} {'words':>6}")
for m, t, s, su, mem, w, real, cpu in rows:
    print(f"{m:7.1f}m {t:9.1f} s {s:7.1f} s {su:6.1f} s {real:6.1f} s {cpu:7.1f} s {cpu / real * 100:6.0f} % {mem:7.0f} MB {w:6.0f}")
print("  (avg CPU: 100 % = one core busy. Whisper and the summary mostly run on the GPU.)")
print()
print("Per minute of meeting (flat = scales linearly):")
for m, t, s, su, mem, w, real, cpu in rows:
    print(f"{m:7.1f}m  transcribe {t / m:5.1f} s/min   speakers {s / m:5.2f} s/min   CPU {cpu / m:5.1f} s/min   words {w / m:5.0f}/min")

m, t, s, su, mem, w, real, cpu = rows[-1]
first = rows[0]
f = 60 / m
# Memory: the models are a fixed cost. What grows is the audio in memory and the summary's
# context, which is sized to the transcript but capped at 32k tokens (about a 2.5-hour meeting).
grow = (mem - first[4]) / (m - first[0]) if m > first[0] else 0
mem60 = mem + grow * (60 - m)
speed = m * 60 / t if t else 0
words60 = w * f
tokens60 = words60 * 1.6  # Norwegian: roughly 1.6 tokens a word
budget = 32768 - 2048 - 400
print()
print("Predicted for a 60-minute meeting:")
print(f"  transcription: {t * f / 60:4.1f} min if redone with `process` (live keeps up: {speed:.0f}x real time, needs 2x for two tracks)")
print(f"  speakers:      {s * f / 60:4.1f} min" + ("   (growing faster than the meeting!)" if len(rows) > 1 and s / m > 1.5 * rows[0][2] / rows[0][0] + 0.05 else ""))
print(f"  summary:       ~{tokens60 / 1000:.0f}k tokens, {'one pass' if tokens60 < budget else f'split into {int(tokens60 // budget) + 1} parts'} (context {budget // 1000}k)")
print(f"  peak memory:   at most ~{mem60 / 1000:.1f} GB (grows {grow:.0f} MB per minute; an upper bound, since the summary context is capped)")
print(f"  CPU time:      ~{cpu * f / 60:.1f} min after stop, if everything is redone (live transcription spreads most of it over the meeting)")
PY
