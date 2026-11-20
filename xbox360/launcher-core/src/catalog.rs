//! Catalog TSV parser, Aurora 14-genre bitmask classifier, sorting, and user state.

use std::collections::HashMap;

/// All 14 Aurora genre flags from `ContentItems.GenreFlag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Genre {
    ActionAdventure,
    Shooter,
    RacingFlying,
    Family,
    Platformer,
    Classics,
    PuzzleTrivia,
    SportsRecreation,
    RolePlaying,
    Fighting,
    StrategySimulation,
    CardBoard,
    Music,
    Other,
}

impl Genre {
    pub const ALL: [Genre; 14] = [
        Genre::ActionAdventure,
        Genre::Shooter,
        Genre::RacingFlying,
        Genre::Family,
        Genre::Platformer,
        Genre::Classics,
        Genre::PuzzleTrivia,
        Genre::SportsRecreation,
        Genre::RolePlaying,
        Genre::Fighting,
        Genre::StrategySimulation,
        Genre::CardBoard,
        Genre::Music,
        Genre::Other,
    ];

    pub const fn mask(self) -> u32 {
        match self {
            Self::Other => 0x0001,
            Self::ActionAdventure => 0x0002,
            Self::Family => 0x0004,
            Self::Fighting => 0x0008,
            Self::Music => 0x0010,
            Self::Platformer => 0x0020,
            Self::RacingFlying => 0x0040,
            Self::RolePlaying => 0x0080,
            Self::Shooter => 0x0100,
            Self::StrategySimulation => 0x0200,
            Self::SportsRecreation => 0x0400,
            Self::CardBoard => 0x0800,
            Self::Classics => 0x1000,
            Self::PuzzleTrivia => 0x2000,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::ActionAdventure => "Action & Adventure",
            Self::Shooter => "Shooter",
            Self::RacingFlying => "Racing & Flying",
            Self::Family => "Family",
            Self::Platformer => "Platformer",
            Self::Classics => "Classics",
            Self::PuzzleTrivia => "Puzzle & Trivia",
            Self::SportsRecreation => "Sports & Recreation",
            Self::RolePlaying => "Role Playing",
            Self::Fighting => "Fighting",
            Self::StrategySimulation => "Strategy & Simulation",
            Self::CardBoard => "Card & Board",
            Self::Music => "Music",
            Self::Other => "Other",
        }
    }

    pub fn from_bitmask(flag: u32) -> Vec<Genre> {
        Self::ALL
            .iter()
            .copied()
            .filter(|g| (flag & g.mask()) != 0)
            .collect()
    }
}

/// Persistent Plex-style left sidebar destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarTab {
    AllGames,
    RecentlyPlayed,
    Favorites,
    Genre(Genre),
    Uncategorized,
    OpenAurora,
}

impl SidebarTab {
    pub fn ordered_tabs() -> Vec<SidebarTab> {
        let mut out = vec![
            SidebarTab::AllGames,
            SidebarTab::RecentlyPlayed,
            SidebarTab::Favorites,
        ];
        for g in Genre::ALL {
            out.push(SidebarTab::Genre(g));
        }
        out.push(SidebarTab::Uncategorized);
        out.push(SidebarTab::OpenAurora);
        out
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AllGames => "All Games",
            Self::RecentlyPlayed => "Recently Played",
            Self::Favorites => "Favorites",
            Self::Genre(g) => g.label(),
            Self::Uncategorized => "Uncategorized",
            Self::OpenAurora => "Open Aurora",
        }
    }

    pub const fn is_filter_tab(self) -> bool {
        !matches!(self, Self::OpenAurora)
    }
}

/// Content platform badge derived from Aurora `ContentGroup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentBadge {
    Xbox360,
    Xbla,
    OriginalXbox,
    Homebrew,
    Other,
}

impl ContentBadge {
    pub const fn from_group(group: u32) -> Self {
        match group {
            1 => Self::Xbox360,
            2 => Self::Xbla,
            3 => Self::OriginalXbox,
            5 => Self::Homebrew,
            _ => Self::Other,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Xbox360 => "Xbox 360",
            Self::Xbla => "XBLA",
            Self::OriginalXbox => "Original Xbox",
            Self::Homebrew => "Homebrew",
            Self::Other => "Game",
        }
    }
}

/// Sort options cycled by the Y button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    TitleAsc,
    RecentlyPlayed,
    ReleaseYearDesc,
    FavoritesFirst,
}

impl SortMode {
    pub const fn next(self) -> Self {
        match self {
            Self::TitleAsc => Self::RecentlyPlayed,
            Self::RecentlyPlayed => Self::ReleaseYearDesc,
            Self::ReleaseYearDesc => Self::FavoritesFirst,
            Self::FavoritesFirst => Self::TitleAsc,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::TitleAsc => "A-Z",
            Self::RecentlyPlayed => "Recently Played",
            Self::ReleaseYearDesc => "Release Year",
            Self::FavoritesFirst => "Favorites First",
        }
    }
}

/// Quick first-letter filter cycled by LT / RT triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetterFilter {
    All,
    Digits,
    Letter(char),
}

impl LetterFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Digits,
            Self::Digits => Self::Letter('A'),
            Self::Letter('Z') => Self::All,
            Self::Letter(c) if c.is_ascii_uppercase() => Self::Letter((c as u8 + 1) as char),
            Self::Letter(_) => Self::All,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::All => Self::Letter('Z'),
            Self::Digits => Self::All,
            Self::Letter('A') => Self::Digits,
            Self::Letter(c) if c.is_ascii_uppercase() => Self::Letter((c as u8 - 1) as char),
            Self::Letter(_) => Self::All,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::All => String::from("All"),
            Self::Digits => String::from("0-9"),
            Self::Letter(c) => c.to_string(),
        }
    }

    pub fn matches_title(self, title: &str) -> bool {
        let Some(first) = title.trim_start().chars().next() else {
            return matches!(self, Self::All);
        };
        let upper = first.to_ascii_uppercase();
        match self {
            Self::All => true,
            Self::Digits => !upper.is_ascii_alphabetic(),
            Self::Letter(target) => upper == target.to_ascii_uppercase(),
        }
    }
}

/// Single game or homebrew title in the Shelf library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEntry {
    pub id: u32,
    pub title_id: String,
    pub media_id: String,
    pub title_name: String,
    pub genre_flag: u32,
    pub content_group: u32,
    pub file_type: u32,
    pub device: String,
    pub directory: String,
    pub executable: String,
    pub publisher: String,
    pub release_year: String,
    pub has_poster: bool,
    pub has_backdrop: bool,
    pub description: String,
    pub favorite: bool,
    pub last_played_at: u64,
    pub play_count: u32,
}

impl GameEntry {
    pub const fn badge(&self) -> ContentBadge {
        ContentBadge::from_group(self.content_group)
    }

    pub fn genres(&self) -> Vec<Genre> {
        Genre::from_bitmask(self.genre_flag)
    }

    pub fn matches_tab(&self, tab: SidebarTab) -> bool {
        match tab {
            SidebarTab::AllGames => true,
            SidebarTab::RecentlyPlayed => self.last_played_at > 0,
            SidebarTab::Favorites => self.favorite,
            SidebarTab::Genre(g) => (self.genre_flag & g.mask()) != 0,
            SidebarTab::Uncategorized => self.genre_flag == 0,
            SidebarTab::OpenAurora => true,
        }
    }

    /// Returns a 1-2 character uppercase monogram for posterless fallback tiles.
    pub fn monogram(&self) -> String {
        let mut letters = String::with_capacity(2);
        for word in self.title_name.split_whitespace() {
            if let Some(ch) = word.chars().find(|c| c.is_ascii_alphanumeric()) {
                letters.push(ch.to_ascii_uppercase());
                if letters.len() == 2 {
                    break;
                }
            }
        }
        if letters.is_empty() {
            self.title_id.chars().take(2).collect()
        } else {
            letters
        }
    }
}

/// Parsed game library plus synced user state (favorites and play history).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub updated_unix: u64,
    entries: Vec<GameEntry>,
}

impl Catalog {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn parse_tsv(raw: &str) -> Result<Self, String> {
        let mut lines = raw.lines().filter(|l| !l.trim().is_empty());
        let Some(header) = lines.next() else {
            return Err(String::from("empty catalog TSV"));
        };
        let hdr_cols: Vec<&str> = header.split('\t').collect();
        if hdr_cols.len() < 4 || hdr_cols[0] != "SHELF_CATALOG" || hdr_cols[1] != "v1" {
            return Err(String::from("invalid SHELF_CATALOG v1 header"));
        }
        let updated_unix = hdr_cols[2].trim().parse::<u64>().unwrap_or(0);

        let mut entries = Vec::new();
        for (line_no, line) in lines.enumerate() {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() != 15 {
                return Err(format!(
                    "catalog line {}: expected 15 columns, got {}",
                    line_no + 2,
                    cols.len()
                ));
            }
            let tid = normalize_hex_id(cols[1])
                .ok_or_else(|| format!("catalog line {}: invalid title_id {:?}", line_no + 2, cols[1]))?;
            let media_id = normalize_hex_id(cols[2]).unwrap_or_else(|| String::from("00000000"));
            entries.push(GameEntry {
                id: cols[0].trim().parse::<u32>().unwrap_or(0),
                title_id: tid,
                media_id,
                title_name: cols[3].to_string(),
                genre_flag: cols[4].trim().parse::<u32>().unwrap_or(0),
                content_group: cols[5].trim().parse::<u32>().unwrap_or(0),
                file_type: cols[6].trim().parse::<u32>().unwrap_or(0),
                device: cols[7].trim().to_string(),
                directory: cols[8].to_string(),
                executable: cols[9].to_string(),
                publisher: cols[10].to_string(),
                release_year: cols[11].trim().to_string(),
                has_poster: cols[12].trim() == "1",
                has_backdrop: cols[13].trim() == "1",
                description: cols[14].to_string(),
                favorite: false,
                last_played_at: 0,
                play_count: 0,
            });
        }

        Ok(Self {
            updated_unix,
            entries,
        })
    }

    pub fn entries(&self) -> &[GameEntry] {
        &self.entries
    }

    pub fn get(&self, idx: usize) -> Option<&GameEntry> {
        self.entries.get(idx)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn apply_userstate_tsv(&mut self, raw: &str) {
        let mut by_tid: HashMap<String, (bool, u64, u32)> = HashMap::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                continue;
            }
            let Some(tid) = normalize_hex_id(cols[0]) else {
                continue;
            };
            let fav = cols[1].trim() == "1";
            let last_played = cols[2].trim().parse::<u64>().unwrap_or(0);
            let play_count = cols[3].trim().parse::<u32>().unwrap_or(0);
            by_tid.insert(tid, (fav, last_played, play_count));
        }

        for entry in &mut self.entries {
            if let Some(&(fav, last_played, play_count)) = by_tid.get(&entry.title_id) {
                entry.favorite = fav;
                entry.last_played_at = last_played;
                entry.play_count = play_count;
            }
        }
    }

    pub fn serialize_userstate_tsv(&self) -> String {
        let mut merged: HashMap<&str, (bool, u64, u32)> = HashMap::new();
        for entry in &self.entries {
            if !entry.favorite && entry.last_played_at == 0 && entry.play_count == 0 {
                continue;
            }
            let cur = merged
                .entry(entry.title_id.as_str())
                .or_insert((false, 0, 0));
            cur.0 |= entry.favorite;
            cur.1 = cur.1.max(entry.last_played_at);
            cur.2 = cur.2.max(entry.play_count);
        }
        let mut keys: Vec<&str> = merged.keys().copied().collect();
        keys.sort_unstable();

        let mut out = String::new();
        for tid in keys {
            let (fav, last, count) = merged[tid];
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                tid,
                if fav { 1 } else { 0 },
                last,
                count
            ));
        }
        out
    }

    pub fn toggle_favorite(&mut self, title_id: &str) -> Option<bool> {
        let tid = normalize_hex_id(title_id)?;
        let mut target_state: Option<bool> = None;
        for entry in &self.entries {
            if entry.title_id == tid {
                target_state = Some(!entry.favorite);
                break;
            }
        }
        let next = target_state?;
        for entry in &mut self.entries {
            if entry.title_id == tid {
                entry.favorite = next;
            }
        }
        Some(next)
    }

    pub fn record_played(&mut self, title_id: &str, unix_ts: u64) {
        let Some(tid) = normalize_hex_id(title_id) else {
            return;
        };
        for entry in &mut self.entries {
            if entry.title_id == tid {
                if unix_ts > entry.last_played_at {
                    entry.last_played_at = unix_ts;
                }
                entry.play_count = entry.play_count.saturating_add(1);
            }
        }
    }

    pub fn tab_count(&self, tab: SidebarTab) -> usize {
        if !tab.is_filter_tab() {
            return 0;
        }
        self.entries.iter().filter(|e| e.matches_tab(tab)).count()
    }

    pub fn filtered_indices(
        &self,
        tab: SidebarTab,
        sort: SortMode,
        letter: LetterFilter,
    ) -> Vec<usize> {
        let mut indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches_tab(tab) && letter.matches_title(&e.title_name))
            .map(|(i, _)| i)
            .collect();

        indices.sort_by(|&a, &b| {
            let ea = &self.entries[a];
            let eb = &self.entries[b];
            match sort {
                SortMode::TitleAsc => cmp_title(ea, eb),
                SortMode::RecentlyPlayed => eb
                    .last_played_at
                    .cmp(&ea.last_played_at)
                    .then_with(|| cmp_title(ea, eb)),
                SortMode::ReleaseYearDesc => {
                    let ya = ea.release_year.parse::<u32>().unwrap_or(0);
                    let yb = eb.release_year.parse::<u32>().unwrap_or(0);
                    yb.cmp(&ya).then_with(|| cmp_title(ea, eb))
                }
                SortMode::FavoritesFirst => eb
                    .favorite
                    .cmp(&ea.favorite)
                    .then_with(|| cmp_title(ea, eb)),
            }
        });

        indices
    }
}

fn cmp_title(a: &GameEntry, b: &GameEntry) -> std::cmp::Ordering {
    a.title_name
        .to_ascii_lowercase()
        .cmp(&b.title_name.to_ascii_lowercase())
        .then_with(|| a.id.cmp(&b.id))
}

pub fn normalize_hex_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(s.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tsv() -> String {
        [
            "SHELF_CATALOG\tv1\t1700000100\t4",
            "12\t45410912\t0E7A7EB3\tPortal 2\t2\t1\t1\tUsb0:\t\\games\\portal 2\tdefault.xex\tElectronic Arts\t2012\t1\t1\tCo-op puzzle game.",
            "13\t454108E6\t5C087C2C\tSkate 3\t1024\t1\t3\tUsb0:\t\\games\\skate 3\\454108e6\\00007000\t5c087c2c\tElectronic Arts\t2014\t1\t0\tSkateboarding.",
            "102\t584108B7\t11223344\tCastle Crashers\t6\t2\t3\tUsb0:\t\\content\\0000000000000000\\584108b7\\000d0000\tf9432a\tThe Behemoth\t2008\t1\t0\tBrawler.",
            "470\tC0DE9999\t00000000\t0 Day Tool\t0\t5\t3\tUsb0:\t\\content\\0000000000000000\\c0de9999\\00080000\tc0de\t\t\t0\t0\tHomebrew.",
        ]
        .join("\n")
    }

    #[test]
    fn parses_catalog_genres_and_filters() {
        let mut cat = Catalog::parse_tsv(&sample_tsv()).expect("valid tsv");
        assert_eq!(cat.entries().len(), 4);
        assert_eq!(cat.tab_count(SidebarTab::AllGames), 4);
        assert_eq!(cat.tab_count(SidebarTab::Genre(Genre::ActionAdventure)), 2);
        assert_eq!(cat.tab_count(SidebarTab::Genre(Genre::Family)), 1);
        assert_eq!(cat.tab_count(SidebarTab::Genre(Genre::SportsRecreation)), 1);
        assert_eq!(cat.tab_count(SidebarTab::Uncategorized), 1);

        let all_az = cat.filtered_indices(SidebarTab::AllGames, SortMode::TitleAsc, LetterFilter::All);
        assert_eq!(cat.get(all_az[0]).unwrap().title_name, "0 Day Tool");
        assert_eq!(cat.get(all_az[1]).unwrap().title_name, "Castle Crashers");

        let digits = cat.filtered_indices(SidebarTab::AllGames, SortMode::TitleAsc, LetterFilter::Digits);
        assert_eq!(digits.len(), 1);
        assert_eq!(cat.get(digits[0]).unwrap().title_id, "C0DE9999");

        let p_only = cat.filtered_indices(
            SidebarTab::AllGames,
            SortMode::TitleAsc,
            LetterFilter::Letter('P'),
        );
        assert_eq!(p_only.len(), 1);
        assert_eq!(cat.get(p_only[0]).unwrap().title_id, "45410912");

        // Userstate merge + round-trip
        cat.apply_userstate_tsv("584108b7\t1\t1700000500\t3\n45410912\t0\t1700000900\t1\n");
        assert_eq!(cat.tab_count(SidebarTab::Favorites), 1);
        assert_eq!(cat.tab_count(SidebarTab::RecentlyPlayed), 2);

        let recent = cat.filtered_indices(
            SidebarTab::RecentlyPlayed,
            SortMode::RecentlyPlayed,
            LetterFilter::All,
        );
        assert_eq!(cat.get(recent[0]).unwrap().title_id, "45410912");
        assert_eq!(cat.get(recent[1]).unwrap().title_id, "584108B7");

        let toggled = cat.toggle_favorite("45410912");
        assert_eq!(toggled, Some(true));
        cat.record_played("454108E6", 1700001000);
        let round_trip = cat.serialize_userstate_tsv();
        assert!(round_trip.contains("45410912\t1\t1700000900\t1"));
        assert!(round_trip.contains("454108E6\t0\t1700001000\t1"));
    }

    #[test]
    fn genre_bitmask_counts_all_fourteen() {
        let mut combined = 0u32;
        for g in Genre::ALL {
            assert_eq!(combined & g.mask(), 0, "duplicate mask on {:?}", g);
            combined |= g.mask();
        }
        assert_eq!(Genre::from_bitmask(combined).len(), 14);
    }

    #[test]
    fn letter_filter_cycles_cleanly() {
        let f = LetterFilter::All;
        assert_eq!(f.next(), LetterFilter::Digits);
        assert_eq!(f.next().next(), LetterFilter::Letter('A'));
        assert_eq!(LetterFilter::Letter('Z').next(), LetterFilter::All);
        assert_eq!(f.prev(), LetterFilter::Letter('Z'));
    }
}
