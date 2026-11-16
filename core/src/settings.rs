//! The user's preferences (Settings → General, as in Apple Notes) and the
//! window state that's remembered between runs.

use serde::{Deserialize, Serialize};

use crate::doc::Kind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortBy {
    #[default]
    Edited,
    Created,
    Title,
}

impl SortBy {
    pub const ALL: [SortBy; 3] = [SortBy::Edited, SortBy::Created, SortBy::Title];
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Edited => "Date Edited",
            SortBy::Created => "Date Created",
            SortBy::Title => "Title",
        }
    }
}

/// "New notes start with".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartWith {
    #[default]
    Title,
    Heading,
    Subheading,
    Body,
}

impl StartWith {
    pub const ALL: [StartWith; 4] = [StartWith::Title, StartWith::Heading, StartWith::Subheading, StartWith::Body];
    pub fn label(self) -> &'static str {
        match self {
            StartWith::Title => "Title",
            StartWith::Heading => "Heading",
            StartWith::Subheading => "Subheading",
            StartWith::Body => "Body",
        }
    }
    pub fn kind(self) -> Kind {
        match self {
            StartWith::Title => Kind::Title,
            StartWith::Heading => Kind::Heading,
            StartWith::Subheading => Kind::Subheading,
            StartWith::Body => Kind::Body,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListView {
    #[default]
    List,
    Gallery,
}

/// How often synced accounts check for changes.
pub const REFRESH_CHOICES: [u32; 6] = [1, 5, 15, 30, 60, 0];

pub fn refresh_label(minutes: u32) -> String {
    match minutes {
        0 => "Manually".into(),
        1 => "Every minute".into(),
        60 => "Every hour".into(),
        m => format!("Every {m} minutes"),
    }
}

pub const TEXT_SIZES: [f32; 7] = [12.0, 13.0, 14.0, 15.0, 17.0, 19.0, 22.0];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where new notes go when no folder is selected.
    pub default_account: String,
    pub sort_by: SortBy,
    pub start_with: StartWith,
    /// Checked items move to the bottom of their checklist.
    pub sort_checked: bool,
    /// The body text size of notes, in points.
    pub text_size: f32,
    /// Notes list sections: Pinned, Today, Previous 7 Days, …
    pub group_by_date: bool,
    /// Note counts next to folders in the sidebar.
    pub show_note_count: bool,
    pub refresh_minutes: u32,
    /// The locked-notes password's check (see `app::locked`) and hint.
    pub lock_check: Option<String>,
    pub lock_hint: String,
    // ---- Window state ------------------------------------------------
    pub view: ListView,
    pub sidebar_visible: bool,
    pub collapsed: Vec<String>,
    pub last_folder: Option<String>,
    pub last_note: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_account: crate::model::LOCAL.to_owned(),
            sort_by: SortBy::default(),
            start_with: StartWith::default(),
            sort_checked: false,
            text_size: 15.0,
            group_by_date: true,
            show_note_count: true,
            refresh_minutes: 5,
            lock_check: None,
            lock_hint: String::new(),
            view: ListView::default(),
            sidebar_visible: true,
            collapsed: Vec::new(),
            last_folder: None,
            last_note: None,
        }
    }
}
