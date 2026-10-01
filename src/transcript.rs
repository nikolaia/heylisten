use serde::{Deserialize, Serialize};

/// Who said something. See CONTEXT.md: Me, Others, Speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Who {
    Me,
    Others,
    Speaker(u32),
}

impl Who {
    /// The Norwegian label shown in notes.
    pub fn label(self) -> String {
        match self {
            Who::Me => "Meg".into(),
            Who::Others => "Andre".into(),
            Who::Speaker(n) => format!("Taler {n}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub who: Who,
    pub text: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
}

/// Consecutive segments from the same person, joined.
pub struct Paragraph {
    pub start_ms: u64,
    pub who: Who,
    pub text: String,
}

impl Transcript {
    /// Merges per-track segments into one transcript ordered by start time, dropping echo.
    pub fn merge(tracks: Vec<Vec<Segment>>) -> Transcript {
        let mut segments: Vec<Segment> = tracks.into_iter().flatten().collect();
        segments.sort_by_key(|s| s.start_ms);
        let echoes: Vec<bool> = segments.iter().map(|s| is_echo(s, &segments)).collect();
        let mut echoes = echoes.into_iter();
        segments.retain(|_| !echoes.next().unwrap());
        Transcript { segments }
    }

    pub fn paragraphs(&self) -> Vec<Paragraph> {
        let mut out: Vec<Paragraph> = Vec::new();
        for s in &self.segments {
            match out.last_mut() {
                Some(p) if p.who == s.who => {
                    p.text.push(' ');
                    p.text.push_str(&s.text);
                }
                _ => out.push(Paragraph { start_ms: s.start_ms, who: s.who, text: s.text.clone() }),
            }
        }
        out
    }

    /// Distinct people in order of first appearance.
    pub fn people(&self) -> Vec<Who> {
        let mut out = Vec::new();
        for s in &self.segments {
            if !out.contains(&s.who) {
                out.push(s.who);
            }
        }
        out
    }
}

/// Without headphones the mic hears the others through the speakers. A mic segment that
/// mostly repeats what the system track had at about the same time is that echo.
fn is_echo(segment: &Segment, all: &[Segment]) -> bool {
    if segment.who != Who::Me {
        return false;
    }
    let mine = words(&segment.text);
    if mine.len() < 3 {
        return false;
    }
    all.iter()
        .filter(|o| o.who != Who::Me)
        .filter(|o| o.start_ms <= segment.end_ms + 2_000 && segment.start_ms <= o.end_ms + 2_000)
        .any(|o| {
            let theirs = words(&o.text);
            let shared = mine.iter().filter(|w| theirs.contains(w)).count();
            shared as f32 / mine.len() as f32 >= 0.6
        })
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, who: Who, text: &str) -> Segment {
        Segment { start_ms, end_ms: start_ms + 1000, who, text: text.into() }
    }

    #[test]
    fn drops_mic_echo_of_others() {
        let t = Transcript::merge(vec![
            vec![
                Segment { start_ms: 10_400, end_ms: 13_000, who: Who::Me, text: "Vi lanserer appen i november.".into() },
                Segment { start_ms: 20_000, end_ms: 22_000, who: Who::Me, text: "Det høres bra ut, Kari.".into() },
            ],
            vec![Segment { start_ms: 10_000, end_ms: 12_500, who: Who::Others, text: "Vi lanserer appen i november".into() }],
        ]);
        let texts: Vec<&str> = t.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Vi lanserer appen i november", "Det høres bra ut, Kari."]);
    }

    #[test]
    fn merges_tracks_and_groups_paragraphs() {
        let t = Transcript::merge(vec![
            vec![seg(0, Who::Me, "Hei."), seg(1000, Who::Me, "Hører dere meg?"), seg(5000, Who::Me, "Bra.")],
            vec![seg(3000, Who::Others, "Ja.")],
        ]);
        let p = t.paragraphs();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].text, "Hei. Hører dere meg?");
        assert_eq!(p[1].who, Who::Others);
        assert_eq!(p[2].start_ms, 5000);
        assert_eq!(t.people(), vec![Who::Me, Who::Others]);
    }
}
