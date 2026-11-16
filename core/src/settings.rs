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
    /// Smart Folders (kept on this device, as the other settings are).
    pub smart_folders: Vec<SmartFolder>,
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
            smart_folders: Vec::new(),
            view: ListView::default(),
            sidebar_visible: true,
            collapsed: Vec::new(),
            last_folder: None,
            last_note: None,
        }
    }
}

/// A Smart Folder, as in Apple Notes: the notes of every folder that pass
/// its filter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartFolder {
    pub id: String,
    pub name: String,
    pub filter: NoteFilter,
}

/// What a Smart Folder's notes have. Every filter that's set must pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteFilter {
    /// #tags (without the #); none: any note.
    pub tags: Vec<String>,
    /// Every one of the tags, rather than any of them.
    pub all_tags: bool,
    pub checklists: ChecklistFilter,
    /// Only pinned notes.
    pub pinned: bool,
    /// Only locked notes.
    pub locked: bool,
    /// Edited in the last this many days (0: any time).
    pub edited_within_days: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChecklistFilter {
    /// Checklists don't matter.
    #[default]
    Off,
    /// Notes with a checklist.
    Any,
    /// Notes with an unticked item.
    Unchecked,
    /// Notes whose checklist items are all ticked.
    AllChecked,
}

impl NoteFilter {
    /// Whether a note passes: its body (Markdown; a locked note's is
    /// sealed, so only `pinned`, `locked` and the date filter see it),
    /// whether it's pinned and locked, and how many days ago it was edited.
    pub fn matches(&self, body: &str, pinned: bool, locked: bool, days_since_edit: i64) -> bool {
        if (self.pinned && !pinned) || (self.locked && !locked) {
            return false;
        }
        if self.edited_within_days > 0 && days_since_edit >= self.edited_within_days as i64 {
            return false;
        }
        if !self.tags.is_empty() {
            let have: Vec<String> = body.lines().flat_map(crate::doc::tags_in).map(|t| t.to_lowercase()).collect();
            let has = |t: &String| have.contains(&t.to_lowercase());
            let ok = if self.all_tags { self.tags.iter().all(has) } else { self.tags.iter().any(has) };
            if !ok {
                return false;
            }
        }
        if self.checklists != ChecklistFilter::Off {
            let (mut ticked, mut unticked) = (0, 0);
            for line in body.lines() {
                let t = line.trim_start();
                let item = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ "));
                match item.and_then(|i| i.get(..3)) {
                    Some("[ ]") => unticked += 1,
                    Some("[x]" | "[X]") => ticked += 1,
                    _ => {}
                }
            }
            let ok = match self.checklists {
                ChecklistFilter::Any => ticked + unticked > 0,
                ChecklistFilter::Unchecked => unticked > 0,
                ChecklistFilter::AllChecked => ticked > 0 && unticked == 0,
                ChecklistFilter::Off => true,
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_folder_filters() {
        let shop = "# Shop #errands\n- [ ] milk\n- [x] bread\n";
        let done = "# Done #errands #home\n- [x] a\n";
        let plain = "# Plain\ntext #Home\n";
        let f = |filter: NoteFilter, body: &str| filter.matches(body, false, false, 3);
        let tags = |t: &[&str], all| NoteFilter { tags: t.iter().map(|s| s.to_string()).collect(), all_tags: all, ..Default::default() };
        assert!(f(tags(&["errands"], false), shop));
        assert!(f(tags(&["home"], false), plain), "tags match whatever their case");
        assert!(!f(tags(&["errands", "home"], true), shop));
        assert!(f(tags(&["errands", "home"], true), done));
        assert!(f(tags(&["errands", "home"], false), plain));

        let lists = |c| NoteFilter { checklists: c, ..Default::default() };
        assert!(f(lists(ChecklistFilter::Any), shop) && f(lists(ChecklistFilter::Any), done) && !f(lists(ChecklistFilter::Any), plain));
        assert!(f(lists(ChecklistFilter::Unchecked), shop) && !f(lists(ChecklistFilter::Unchecked), done));
        assert!(!f(lists(ChecklistFilter::AllChecked), shop) && f(lists(ChecklistFilter::AllChecked), done));

        let recent = NoteFilter { edited_within_days: 7, ..Default::default() };
        assert!(recent.matches(plain, false, false, 6) && !recent.matches(plain, false, false, 7));
        let pinned = NoteFilter { pinned: true, ..Default::default() };
        assert!(pinned.matches(plain, true, false, 0) && !pinned.matches(plain, false, false, 0));
        assert!(NoteFilter::default().matches(plain, false, false, 999), "no filter: every note");
    }
}
