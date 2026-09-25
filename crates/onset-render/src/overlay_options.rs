//! What the DJ chooses to show on the HUD and the Now Playing card, and how big.
use serde::{Deserialize, Serialize};

/// The technical readout, top left. Each switch adds or removes a piece of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // independent switches of a settings file
pub struct HudOptions {
    /// Text size, 1 = design size (0.6..2).
    pub size: f32,
    /// Scene name and output size.
    pub scene: bool,
    /// Frame time and CPU time.
    pub performance: bool,
    /// rekordbox link, playing or paused, playhead.
    pub transport: bool,
    pub tempo: bool,
    pub key: bool,
    /// Current and next phrase.
    pub phrase: bool,
    /// Beats to the drop.
    pub drop: bool,
    /// The next hot cue and how far away it is.
    pub next_cue: bool,
    /// rekordbox's bands and vocal level at the playhead, and the live drums.
    pub analysis: bool,
}

impl Default for HudOptions {
    fn default() -> Self {
        Self {
            size: 1.0,
            scene: true,
            performance: true,
            transport: true,
            tempo: true,
            key: false,
            phrase: true,
            drop: true,
            next_cue: false,
            analysis: false,
        }
    }
}

/// Where the Now Playing card sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    #[default]
    BottomLeft,
    BottomRight,
    TopLeft,
    TopRight,
}

impl Corner {
    pub const ALL: [Self; 4] = [Self::BottomLeft, Self::BottomRight, Self::TopLeft, Self::TopRight];

    pub fn label(self) -> &'static str {
        match self {
            Self::BottomLeft => "Bottom left",
            Self::BottomRight => "Bottom right",
            Self::TopLeft => "Top left",
            Self::TopRight => "Top right",
        }
    }

    pub fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }

    pub fn is_right(self) -> bool {
        matches!(self, Self::BottomRight | Self::TopRight)
    }
}

/// The Now Playing card: which details it shows, where, and how big.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // independent switches of a settings file
pub struct CardOptions {
    /// 1 = design size (0.6..1.6).
    pub size: f32,
    pub corner: Corner,
    pub artwork: bool,
    pub album: bool,
    pub year: bool,
    pub key: bool,
    pub bpm: bool,
    pub genre: bool,
    pub label: bool,
    /// rekordbox star rating.
    pub rating: bool,
    /// The DJ's My Tags.
    pub tags: bool,
    pub comment: bool,
    pub play_count: bool,
}

impl Default for CardOptions {
    fn default() -> Self {
        Self {
            size: 1.0,
            corner: Corner::BottomLeft,
            artwork: true,
            album: true,
            year: true,
            key: true,
            bpm: true,
            genre: false,
            label: false,
            rating: false,
            tags: false,
            comment: false,
            play_count: false,
        }
    }
}

/// How the lyrics look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricStyle {
    /// White text with a soft shadow; only the line being sung.
    Clean,
    /// The sung line glows in the track's colours, the next line waits below.
    #[default]
    Glow,
    /// Words light up one by one as they are sung.
    Karaoke,
}

impl LyricStyle {
    pub const ALL: [Self; 3] = [Self::Clean, Self::Glow, Self::Karaoke];

    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "Clean",
            Self::Glow => "Glow",
            Self::Karaoke => "Karaoke",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Self::Clean => "White text, the line being sung only. Reads anywhere.",
            Self::Glow => "The sung line glows in the track's colours; the next line waits below.",
            Self::Karaoke => "Each word lights up as it is sung.",
        }
    }
}

/// Where the lyrics sit on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricPlace {
    Top,
    #[default]
    Centre,
    Lower,
}

impl LyricPlace {
    pub const ALL: [Self; 3] = [Self::Top, Self::Centre, Self::Lower];

    pub fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Centre => "Centre",
            Self::Lower => "Lower third",
        }
    }
}

/// Synced lyrics over the show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsOptions {
    pub enabled: bool,
    /// Look lyrics up online (LRCLIB) when none are cached; only the title, artist, album and
    /// length are sent.
    pub online: bool,
    pub style: LyricStyle,
    pub place: LyricPlace,
    /// 1 = design size (0.6..2).
    pub size: f32,
    /// How much the lyrics move at drops and build-ups: 0 still, 1 full (colour split,
    /// rainbow sweep, a bounce on the kick). Never flashes.
    pub drop_fx: f32,
    /// Seconds to shift the lyrics by, for tracks whose lyrics run early or late.
    pub offset_s: f32,
}

impl Default for LyricsOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            online: true,
            style: LyricStyle::Glow,
            place: LyricPlace::Centre,
            size: 1.0,
            drop_fx: 0.6,
            offset_s: 0.0,
        }
    }
}
