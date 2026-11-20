//! Deterministic D-pad / thumbstick navigation, 5-column grid scroll, and PIN modal state.

use crate::catalog::{Catalog, LetterFilter, SidebarTab, SortMode};

pub const GRID_COLS: usize = 5;
pub const VISIBLE_ROWS: usize = 2;

/// UI zone currently owning controller focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusZone {
    Sidebar,
    Grid,
    LockModal,
    LaunchError,
}

/// Logical controller inputs mapped from Xbox 360 keystrokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    X,
    Y,
    LB,
    RB,
    LT,
    RT,
    Start,
    Back,
}

/// Interactive options displayed inside the parental lock modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockChoice {
    RequestResume,
    Request15m,
    Request30m,
    EnterPin,
    Cancel,
}

impl LockChoice {
    pub fn choices(paused: bool) -> Vec<LockChoice> {
        if paused {
            vec![
                Self::RequestResume,
                Self::Request15m,
                Self::EnterPin,
                Self::Cancel,
            ]
        } else {
            vec![
                Self::Request15m,
                Self::Request30m,
                Self::EnterPin,
                Self::Cancel,
            ]
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::RequestResume => "Ask Parent to Resume",
            Self::Request15m => "Ask Parent for +15 Minutes",
            Self::Request30m => "Ask Parent for +30 Minutes",
            Self::EnterPin => "Enter 4-Button Parent PIN",
            Self::Cancel => "Back to Library",
        }
    }
}

/// Side effect emitted when controller input requires I/O or launching a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEvent {
    None,
    LaunchGame(usize),
    LaunchAurora,
    ToggleFavorite { title_id: String, favorite: bool },
    RequestMoreTime(u32),
    RequestResume,
    PinUnlocked,
}

/// Complete navigation state for the Plex-style sidebar + poster grid UI.
#[derive(Debug, Clone)]
pub struct NavState {
    pub zone: FocusZone,
    pub tabs: Vec<SidebarTab>,
    pub sidebar_idx: usize,
    pub active_filter_tab_idx: usize,
    pub grid_idx: usize,
    pub scroll_row: usize,
    pub sort_mode: SortMode,
    pub letter_filter: LetterFilter,
    pub local_mp_only: bool,
    pub lock_cursor: usize,
    pub entering_pin: bool,
    pub pin_buffer: Vec<u16>,
    pub pin_error: Option<String>,
    pub launch_error: Option<String>,
    pub pending_launch: Option<usize>,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            zone: FocusZone::Grid,
            tabs: SidebarTab::ordered_tabs(),
            sidebar_idx: 0,
            active_filter_tab_idx: 0,
            grid_idx: 0,
            scroll_row: 0,
            sort_mode: SortMode::TitleAsc,
            letter_filter: LetterFilter::All,
            local_mp_only: false,
            lock_cursor: 0,
            entering_pin: false,
            pin_buffer: Vec::new(),
            pin_error: None,
            launch_error: None,
            pending_launch: None,
        }
    }
}

impl NavState {
    pub fn active_filter_tab(&self) -> SidebarTab {
        self.tabs
            .get(self.active_filter_tab_idx)
            .copied()
            .unwrap_or(SidebarTab::AllGames)
    }

    pub fn hovered_sidebar_tab(&self) -> SidebarTab {
        self.tabs
            .get(self.sidebar_idx)
            .copied()
            .unwrap_or(SidebarTab::AllGames)
    }

    pub fn set_launch_error(&mut self, msg: impl Into<String>) {
        self.launch_error = Some(msg.into());
        self.zone = FocusZone::LaunchError;
    }

    pub fn take_pending_launch(&mut self) -> Option<usize> {
        self.pending_launch.take()
    }

    pub fn clamp_grid(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            self.grid_idx = 0;
            self.scroll_row = 0;
            return;
        }
        if self.grid_idx >= filtered_len {
            self.grid_idx = filtered_len - 1;
        }
        let cur_row = self.grid_idx / GRID_COLS;
        if cur_row < self.scroll_row {
            self.scroll_row = cur_row;
        } else if cur_row >= self.scroll_row + VISIBLE_ROWS {
            self.scroll_row = cur_row + 1 - VISIBLE_ROWS;
        }
    }

    fn cycle_filter_tab(&mut self, forward: bool) {
        let filter_count = self.tabs.iter().filter(|t| t.is_filter_tab()).count();
        if filter_count == 0 {
            return;
        }
        let next = if forward {
            (self.active_filter_tab_idx + 1) % filter_count
        } else if self.active_filter_tab_idx == 0 {
            filter_count - 1
        } else {
            self.active_filter_tab_idx - 1
        };
        self.active_filter_tab_idx = next;
        self.sidebar_idx = next;
        self.grid_idx = 0;
        self.scroll_row = 0;
    }

    /// Processes a non-PIN button input against the current filtered item slice.
    pub fn apply_input(
        &mut self,
        action: InputAction,
        catalog: &mut Catalog,
        filtered: &[usize],
        launch_blocked: bool,
        guard_paused: bool,
    ) -> NavEvent {
        match self.zone {
            FocusZone::LaunchError => {
                if matches!(action, InputAction::A | InputAction::B | InputAction::Back) {
                    self.launch_error = None;
                    self.zone = FocusZone::Grid;
                }
                return NavEvent::None;
            }
            FocusZone::LockModal => {
                if self.entering_pin {
                    if matches!(action, InputAction::B | InputAction::Back) {
                        self.entering_pin = false;
                        self.pin_buffer.clear();
                        self.pin_error = None;
                    }
                    return NavEvent::None;
                }

                let choices = LockChoice::choices(guard_paused);
                match action {
                    InputAction::Up => {
                        if self.lock_cursor == 0 {
                            self.lock_cursor = choices.len().saturating_sub(1);
                        } else {
                            self.lock_cursor -= 1;
                        }
                    }
                    InputAction::Down => {
                        if !choices.is_empty() {
                            self.lock_cursor = (self.lock_cursor + 1) % choices.len();
                        }
                    }
                    InputAction::B | InputAction::Back => {
                        self.zone = FocusZone::Grid;
                        self.pending_launch = None;
                    }
                    InputAction::A => {
                        let picked = choices.get(self.lock_cursor).copied().unwrap_or(LockChoice::Cancel);
                        match picked {
                            LockChoice::RequestResume => return NavEvent::RequestResume,
                            LockChoice::Request15m => return NavEvent::RequestMoreTime(15),
                            LockChoice::Request30m => return NavEvent::RequestMoreTime(30),
                            LockChoice::EnterPin => {
                                self.entering_pin = true;
                                self.pin_buffer.clear();
                                self.pin_error = None;
                            }
                            LockChoice::Cancel => {
                                self.zone = FocusZone::Grid;
                                self.pending_launch = None;
                            }
                        }
                    }
                    _ => {}
                }
                return NavEvent::None;
            }
            FocusZone::Sidebar | FocusZone::Grid => {}
        }

        // Global shortcuts while browsing Library.
        match action {
            InputAction::Start => return NavEvent::LaunchAurora,
            InputAction::LB => {
                self.cycle_filter_tab(false);
                return NavEvent::None;
            }
            InputAction::RB => {
                self.cycle_filter_tab(true);
                return NavEvent::None;
            }
            InputAction::Y => {
                self.sort_mode = self.sort_mode.next();
                self.grid_idx = 0;
                self.scroll_row = 0;
                return NavEvent::None;
            }
            InputAction::LT => {
                self.letter_filter = self.letter_filter.prev();
                self.grid_idx = 0;
                self.scroll_row = 0;
                return NavEvent::None;
            }
            InputAction::RT => {
                self.letter_filter = self.letter_filter.next();
                self.grid_idx = 0;
                self.scroll_row = 0;
                return NavEvent::None;
            }
            InputAction::Back => {
                self.local_mp_only = !self.local_mp_only;
                self.grid_idx = 0;
                self.scroll_row = 0;
                return NavEvent::None;
            }
            _ => {}
        }

        if self.zone == FocusZone::Sidebar {
            match action {
                InputAction::Up => {
                    if self.sidebar_idx == 0 {
                        self.sidebar_idx = self.tabs.len().saturating_sub(1);
                    } else {
                        self.sidebar_idx -= 1;
                    }
                    if self.tabs[self.sidebar_idx].is_filter_tab() {
                        self.active_filter_tab_idx = self.sidebar_idx;
                        self.grid_idx = 0;
                        self.scroll_row = 0;
                    }
                }
                InputAction::Down => {
                    if !self.tabs.is_empty() {
                        self.sidebar_idx = (self.sidebar_idx + 1) % self.tabs.len();
                        if self.tabs[self.sidebar_idx].is_filter_tab() {
                            self.active_filter_tab_idx = self.sidebar_idx;
                            self.grid_idx = 0;
                            self.scroll_row = 0;
                        }
                    }
                }
                InputAction::Right => {
                    if !filtered.is_empty() {
                        self.zone = FocusZone::Grid;
                    }
                }
                InputAction::A => {
                    if self.hovered_sidebar_tab() == SidebarTab::OpenAurora {
                        return NavEvent::LaunchAurora;
                    }
                    if !filtered.is_empty() {
                        self.zone = FocusZone::Grid;
                    }
                }
                _ => {}
            }
            return NavEvent::None;
        }

        // FocusZone::Grid
        self.clamp_grid(filtered.len());
        match action {
            InputAction::B => {
                self.zone = FocusZone::Sidebar;
            }
            InputAction::Left => {
                if filtered.is_empty() || self.grid_idx % GRID_COLS == 0 {
                    self.zone = FocusZone::Sidebar;
                } else {
                    self.grid_idx -= 1;
                }
            }
            InputAction::Right => {
                if !filtered.is_empty()
                    && (self.grid_idx % GRID_COLS) + 1 < GRID_COLS
                    && self.grid_idx + 1 < filtered.len()
                {
                    self.grid_idx += 1;
                }
            }
            InputAction::Up => {
                if self.grid_idx >= GRID_COLS {
                    self.grid_idx -= GRID_COLS;
                }
            }
            InputAction::Down => {
                if !filtered.is_empty() {
                    let cur_row = self.grid_idx / GRID_COLS;
                    let last_row = (filtered.len() - 1) / GRID_COLS;
                    if self.grid_idx + GRID_COLS < filtered.len() {
                        self.grid_idx += GRID_COLS;
                    } else if cur_row < last_row {
                        self.grid_idx = filtered.len() - 1;
                    }
                }
            }
            InputAction::X => {
                if let Some(&entry_idx) = filtered.get(self.grid_idx)
                    && let Some(entry) = catalog.get(entry_idx)
                {
                    let tid = entry.title_id.clone();
                    if let Some(fav) = catalog.toggle_favorite(&tid) {
                        return NavEvent::ToggleFavorite {
                            title_id: tid,
                            favorite: fav,
                        };
                    }
                }
            }
            InputAction::A => {
                if let Some(&entry_idx) = filtered.get(self.grid_idx) {
                    if launch_blocked {
                        self.pending_launch = Some(entry_idx);
                        self.lock_cursor = 0;
                        self.entering_pin = false;
                        self.pin_buffer.clear();
                        self.pin_error = None;
                        self.zone = FocusZone::LockModal;
                    } else {
                        return NavEvent::LaunchGame(entry_idx);
                    }
                }
            }
            _ => {}
        }
        self.clamp_grid(filtered.len());
        NavEvent::None
    }

    /// Appends a controller virtual-key while in PIN entry mode.
    pub fn submit_pin_key(&mut self, vk: u16, expected_pin: &[u16; 4]) -> NavEvent {
        if self.zone != FocusZone::LockModal || !self.entering_pin {
            return NavEvent::None;
        }
        self.pin_error = None;
        self.pin_buffer.push(vk);
        if self.pin_buffer.len() < 4 {
            return NavEvent::None;
        }

        let matched = self.pin_buffer.as_slice() == expected_pin;
        self.pin_buffer.clear();
        if matched {
            self.entering_pin = false;
            self.zone = FocusZone::Grid;
            NavEvent::PinUnlocked
        } else {
            self.pin_error = Some(String::from("Incorrect PIN - try again"));
            NavEvent::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::{VK_PAD_DPAD_DOWN, VK_PAD_DPAD_LEFT, VK_PAD_DPAD_RIGHT, VK_PAD_DPAD_UP};

    fn twelve_game_catalog() -> Catalog {
        let mut lines = vec![String::from("SHELF_CATALOG\tv1\t1700000000\t12")];
        for i in 0..12u32 {
            lines.push(format!(
                "{}\t4541{:04X}\t00000001\tGame {:02}\t2\t1\t1\tUsb0:\t\\games\\g{}\tdefault.xex\tPub\t2010\t1\t0\tDesc",
                i + 1,
                i + 1,
                i,
                i
            ));
        }
        Catalog::parse_tsv(&lines.join("\n")).unwrap()
    }

    #[test]
    fn grid_navigation_scroll_and_sidebar_transitions() {
        let mut cat = twelve_game_catalog();
        let mut nav = NavState::default();
        let filtered = cat.filtered_indices(nav.active_filter_tab(), nav.sort_mode, nav.letter_filter);
        assert_eq!(filtered.len(), 12);

        // Down twice moves row 0 -> row 1 -> row 2 (index 10) and updates scroll_row to 1.
        nav.apply_input(InputAction::Down, &mut cat, &filtered, false, false);
        assert_eq!(nav.grid_idx, 5);
        assert_eq!(nav.scroll_row, 0);

        nav.apply_input(InputAction::Down, &mut cat, &filtered, false, false);
        assert_eq!(nav.grid_idx, 10);
        assert_eq!(nav.scroll_row, 1);

        // Left at col 0 enters Sidebar.
        nav.apply_input(InputAction::Left, &mut cat, &filtered, false, false);
        assert_eq!(nav.zone, FocusZone::Sidebar);

        // Down to OpenAurora (last tab) keeps previous filter tab active, pressing A launches Aurora.
        let last_idx = nav.tabs.len() - 1;
        nav.sidebar_idx = last_idx;
        let ev = nav.apply_input(InputAction::A, &mut cat, &filtered, true, false);
        assert_eq!(ev, NavEvent::LaunchAurora);

        // Toggle favorite with X while on Grid.
        nav.zone = FocusZone::Grid;
        nav.grid_idx = 0;
        let fav_ev = nav.apply_input(InputAction::X, &mut cat, &filtered, false, false);
        assert_eq!(
            fav_ev,
            NavEvent::ToggleFavorite {
                title_id: String::from("45410001"),
                favorite: true
            }
        );

        // RB switches category cleanly; Start triggers Aurora directly.
        nav.apply_input(InputAction::RB, &mut cat, &filtered, false, false);
        assert_eq!(nav.active_filter_tab_idx, 1);
        assert_eq!(
            nav.apply_input(InputAction::Start, &mut cat, &filtered, false, false),
            NavEvent::LaunchAurora
        );
    }

    #[test]
    fn parental_lock_modal_and_pin_unlock_flow() {
        let mut cat = twelve_game_catalog();
        let mut nav = NavState::default();
        let filtered = cat.filtered_indices(nav.active_filter_tab(), nav.sort_mode, nav.letter_filter);

        // Pressing A when blocked opens LockModal with pending_launch.
        let ev = nav.apply_input(InputAction::A, &mut cat, &filtered, true, false);
        assert_eq!(ev, NavEvent::None);
        assert_eq!(nav.zone, FocusZone::LockModal);
        assert_eq!(nav.pending_launch, Some(0));

        // Option 0 is Ask Parent for +15m when not paused.
        let req_ev = nav.apply_input(InputAction::A, &mut cat, &filtered, true, false);
        assert_eq!(req_ev, NavEvent::RequestMoreTime(15));

        // Move down twice to EnterPin (Request15m -> Request30m -> EnterPin).
        nav.apply_input(InputAction::Down, &mut cat, &filtered, true, false);
        nav.apply_input(InputAction::Down, &mut cat, &filtered, true, false);
        nav.apply_input(InputAction::A, &mut cat, &filtered, true, false);
        assert!(nav.entering_pin);

        let expected = [
            VK_PAD_DPAD_UP,
            VK_PAD_DPAD_DOWN,
            VK_PAD_DPAD_LEFT,
            VK_PAD_DPAD_RIGHT,
        ];
        assert_eq!(nav.submit_pin_key(VK_PAD_DPAD_UP, &expected), NavEvent::None);
        assert_eq!(nav.submit_pin_key(VK_PAD_DPAD_DOWN, &expected), NavEvent::None);
        assert_eq!(nav.submit_pin_key(VK_PAD_DPAD_LEFT, &expected), NavEvent::None);
        let unlock = nav.submit_pin_key(VK_PAD_DPAD_RIGHT, &expected);
        assert_eq!(unlock, NavEvent::PinUnlocked);
        assert_eq!(nav.zone, FocusZone::Grid);
        assert_eq!(nav.take_pending_launch(), Some(0));
    }

    #[test]
    fn back_toggles_local_multiplayer_filter_without_leaving_zone() {
        let mut cat = twelve_game_catalog();
        let mut nav = NavState::default();
        let filtered = cat.filtered_indices(nav.active_filter_tab(), nav.sort_mode, nav.letter_filter);

        assert!(!nav.local_mp_only);
        nav.grid_idx = 4;
        nav.apply_input(InputAction::Back, &mut cat, &filtered, false, false);
        assert!(nav.local_mp_only);
        assert_eq!(nav.grid_idx, 0);
        assert_eq!(nav.zone, FocusZone::Grid);

        // B moves from Grid to Sidebar, Back still toggles local_mp_only in Sidebar too.
        nav.apply_input(InputAction::B, &mut cat, &filtered, false, false);
        assert_eq!(nav.zone, FocusZone::Sidebar);
        nav.apply_input(InputAction::Back, &mut cat, &filtered, false, false);
        assert!(!nav.local_mp_only);
        assert_eq!(nav.zone, FocusZone::Sidebar);
    }
}
