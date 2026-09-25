//! Synced lyrics: parsing LRC text into timed lines and words, and finding what is being
//! sung at a playhead. Word times come from enhanced LRC (`<mm:ss.xx>` before each word)
//! when the source has them, and are otherwise spread across the line by word length.

/// One word and when it starts, in seconds from the start of the track.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricWord {
    pub time_s: f32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub time_s: f32,
    pub text: String,
    pub words: Vec<LyricWord>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lyrics {
    /// Timed lines, sorted by time. Empty for plain (unsynced) lyrics.
    pub lines: Vec<LyricLine>,
    /// Where they came from, for the launcher ("LRCLIB").
    pub source: String,
}

/// What is being sung at a moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LyricAt {
    /// Index of the line being sung.
    pub line: usize,
    /// 0..1 through the line's time.
    pub line_progress: f32,
    /// Seconds since the line started.
    pub since_s: f32,
    /// Index of the word being sung within the line.
    pub word: usize,
    /// 0..1 through that word.
    pub word_progress: f32,
}

/// A word with its stamp, if the source gave one.
type RawWord = (Option<f32>, String);

/// A line with no successor is held this long.
const LAST_LINE_S: f32 = 5.0;
/// Words share at most this much of a long line's gap; the rest is the pause after it.
const MAX_WORD_S: f32 = 0.6;

/// `mm:ss.xx` (or `mm:ss`, `mm:ss:xx`) to seconds.
fn parse_stamp(s: &str) -> Option<f32> {
    let (m, rest) = s.split_once(':')?;
    let m: f32 = m.trim().parse().ok()?;
    let rest = rest.replacen(':', ".", 1);
    let sec: f32 = rest.trim().parse().ok()?;
    (m >= 0.0 && sec >= 0.0).then_some(m * 60.0 + sec)
}

/// Splits a line body with optional `<mm:ss.xx>` word stamps into words.
fn words_of(body: &str) -> (String, Vec<RawWord>) {
    let mut words = Vec::new();
    let mut plain = String::new();
    let mut stamp: Option<f32> = None;
    let mut rest = body;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<')
            && let Some((inside, tail)) = after.split_once('>')
            && let Some(t) = parse_stamp(inside)
        {
            stamp = Some(t);
            rest = tail;
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len()).max(1);
        let (chunk, tail) = rest.split_at(end);
        for (i, w) in chunk.split_whitespace().enumerate() {
            words.push((if i == 0 { stamp.take() } else { None }, w.to_string()));
            if !plain.is_empty() {
                plain.push(' ');
            }
            plain.push_str(w);
        }
        rest = tail;
    }
    (plain, words)
}

impl Lyrics {
    /// Parses LRC text: `[mm:ss.xx]` stamps (several per line allowed), optional enhanced
    /// word stamps; metadata tags and empty lines are dropped.
    pub fn parse_lrc(text: &str, source: &str) -> Self {
        let mut raw: Vec<(f32, String, Vec<RawWord>)> = Vec::new();
        for line in text.lines() {
            let mut rest = line.trim();
            let mut stamps = Vec::new();
            while let Some(after) = rest.strip_prefix('[') {
                let Some((inside, tail)) = after.split_once(']') else {
                    break;
                };
                let Some(t) = parse_stamp(inside) else {
                    stamps.clear(); // a metadata tag like [ar:...]
                    rest = "";
                    break;
                };
                stamps.push(t);
                rest = tail;
            }
            let (plain, words) = words_of(rest.trim());
            if plain.is_empty() {
                continue;
            }
            for t in stamps {
                raw.push((t, plain.clone(), words.clone()));
            }
        }
        raw.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut lines: Vec<LyricLine> = Vec::with_capacity(raw.len());
        for (i, (t, text, words)) in raw.iter().enumerate() {
            let next = raw.get(i + 1).map_or(t + LAST_LINE_S, |n| n.0);
            lines.push(LyricLine {
                time_s: *t,
                text: text.clone(),
                words: time_words(*t, next, words),
            });
        }
        Self {
            lines,
            source: source.to_string(),
        }
    }

    /// When line `i` ends: the next line's start, or a few seconds after the last.
    pub fn line_end(&self, i: usize) -> f32 {
        self.lines
            .get(i + 1)
            .map_or_else(|| self.lines.get(i).map_or(0.0, |l| l.time_s + LAST_LINE_S), |n| n.time_s)
    }

    /// The line and word being sung at `t_s`, or `None` before the first line and after the
    /// last one has been held.
    pub fn at(&self, t_s: f32) -> Option<LyricAt> {
        let i = self.lines.partition_point(|l| l.time_s <= t_s).checked_sub(1)?;
        let line = &self.lines[i];
        let end = self.line_end(i);
        if t_s >= end {
            return None;
        }
        let span = (end - line.time_s).max(1e-3);
        let w = line
            .words
            .partition_point(|w| w.time_s <= t_s)
            .saturating_sub(1);
        let w_start = line.words.get(w).map_or(line.time_s, |x| x.time_s);
        let w_end = line.words.get(w + 1).map_or(end.min(w_start + MAX_WORD_S), |x| x.time_s);
        Some(LyricAt {
            line: i,
            line_progress: ((t_s - line.time_s) / span).clamp(0.0, 1.0),
            since_s: t_s - line.time_s,
            word: w,
            word_progress: ((t_s - w_start) / (w_end - w_start).max(1e-3)).clamp(0.0, 1.0),
        })
    }
}

/// Word start times: given stamps are kept; the rest are spread by word length over the
/// line's gap (capped, so a long pause after a line is not sung through).
fn time_words(start: f32, end: f32, words: &[RawWord]) -> Vec<LyricWord> {
    let weight = |w: &str| w.chars().count() as f32 + 2.0;
    let total: f32 = words.iter().map(|(_, w)| weight(w)).sum::<f32>().max(1.0);
    let sung = (end - start).min(MAX_WORD_S * words.len() as f32).max(0.0);
    let mut t = start;
    words
        .iter()
        .map(|(stamp, w)| {
            let at = stamp.unwrap_or(t);
            t = at + sung * weight(w) / total;
            LyricWord {
                time_s: at,
                text: w.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LRC: &str = "[ar:Disclosure]\n[00:16.46] You lift my heart up\n[00:19.97] When the rest of me is down\n\n[00:24.36] You, you enchant me\n";

    #[test]
    fn parses_timed_lines_and_drops_tags_and_blanks() {
        let l = Lyrics::parse_lrc(LRC, "LRCLIB");
        assert_eq!(l.lines.len(), 3);
        assert!((l.lines[0].time_s - 16.46).abs() < 1e-3);
        assert_eq!(l.lines[1].text, "When the rest of me is down");
        assert_eq!(l.source, "LRCLIB");
    }

    #[test]
    fn a_line_with_two_stamps_appears_twice_in_order() {
        let l = Lyrics::parse_lrc("[00:30.00][00:10.00] Chorus\n[00:20.00] Verse\n", "");
        let times: Vec<f32> = l.lines.iter().map(|x| x.time_s).collect();
        assert_eq!(times, vec![10.0, 20.0, 30.0]);
        assert_eq!(l.lines[2].text, "Chorus");
    }

    #[test]
    fn finds_the_line_and_word_being_sung() {
        let l = Lyrics::parse_lrc(LRC, "");
        assert!(l.at(10.0).is_none(), "nothing before the first line");
        let a = l.at(17.5).unwrap();
        assert_eq!(a.line, 0);
        assert!(a.line_progress > 0.2 && a.line_progress < 0.4, "{a:?}");
        assert!(a.word >= 1, "a second into the line we are past the first word: {a:?}");
        assert_eq!(l.at(20.5).unwrap().line, 1);
        assert!(l.at(24.36 + 6.0).is_none(), "the last line is held, then cleared");
    }

    #[test]
    fn spreads_words_over_the_line_but_not_through_a_long_pause() {
        let l = Lyrics::parse_lrc("[00:10.00] one two three\n[00:40.00] next\n", "");
        let w = &l.lines[0].words;
        assert_eq!(w.len(), 3);
        assert!((w[0].time_s - 10.0).abs() < 1e-3);
        assert!(w[1].time_s > w[0].time_s && w[2].time_s > w[1].time_s);
        assert!(w[2].time_s < 12.0, "three words are sung in under two seconds: {w:?}");
    }

    #[test]
    fn keeps_enhanced_word_stamps() {
        let l = Lyrics::parse_lrc("[00:05.00] <00:05.00>hey <00:05.80>there\n", "");
        let w = &l.lines[0].words;
        assert_eq!(l.lines[0].text, "hey there");
        assert!((w[1].time_s - 5.8).abs() < 1e-3, "{w:?}");
    }
}
