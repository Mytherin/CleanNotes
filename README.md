# CleanNotes

A desktop notes app written in Rust that looks and behaves like Apple Notes, with pluggable sync: **Bitwarden**, **iCloud (CloudKit)**, **Standard Notes**, or a **SQLite database** file.
Built on [egui/eframe](https://github.com/emilk/egui), like its sibling [CleanCalendar](../CleanCalendar).

## Features

- **Three panes like Apple Notes:** folders (per account, nested, with note counts), the notes list (title, time and first line, in sections: Pinned, Today, Yesterday, Previous 7 Days, Previous 30 Days, months, years), and the note. A **gallery view** shows notes as thumbnails (double-click opens one).
- **The macOS 26 look:** the sidebar floats as a rounded panel with the selected folder as a yellow pill; the list has a header with the folder's name and note count and a round "…" menu (list or gallery, sort, group by date, note counts), with large date sections; the toolbar has round glass buttons (New Note, "…" note menu, Search, which opens into a field) and a capsule with Aa, checklist and link.
- **Rich text editor** with Apple Notes' paragraph styles: Title, Heading, Subheading, Body, Monospaced, bulleted (•), dashed (–) and numbered lists, checklists, and block quotes; bold, italic, underline, strikethrough and links. Lists indent and outdent (Tab / Shift+Tab), list items move up and down, and checked items can sort to the bottom. Typing `- `, `* `, `1. ` or `[] ` at the start of a line starts a list. Plain URLs and `#tags` are highlighted; links open on click.
- **The Aa menu** (B I U S on top, paragraph styles shown in their own style below), checklist and link buttons in the toolbar, the note's "…" menu (pin, find, move, duplicate, copy as Markdown, delete), and a right-click menu in the editor.
- **Find in note** (Ctrl+F, with match count and next/previous) and **search all notes** from the toolbar (every word must match; matches are highlighted in the note).
- **Folders:** new (Ctrl+Shift+N or the New Folder button at the top of the sidebar), rename (double-click), delete (notes go to Recently Deleted), subfolders, drag notes onto a folder, or "Move to" from a note's menu, also into another account.
- **Locked notes:** "Lock Note" in a note's "…" menu encrypts it with a password you choose (set the first time, with a hint; change it in Settings → General). As in Apple Notes, a locked note's title stays visible in the list; its text is encrypted with AES-256 and an HMAC check, under a key made from the password with Argon2id, and it is stored and synced only in that form, so sync services never see it. One password opens every locked note until Lock Now, 15 minutes without use, or quitting; passwords are never saved. A locked note edited on two devices at once is kept as two notes. If the password is forgotten, locked notes can't be recovered.
- **Recently Deleted** per account: notes stay 30 days, can be recovered, and can't be edited until they are (as in Apple Notes).
- **Tags:** `#words` in notes become tags in the sidebar; a new note made in a tag carries it.
- **Pinning, duplicating** (Ctrl+D), **copy as Markdown**, and a clipboard that keeps formatting within the app.
- **Settings** (⚙ or Ctrl+,), laid out like Apple Notes': default account, sort notes by (date edited, date created, title), new notes start with (Title, Heading, Subheading, Body), sort checked items (manually or automatically), default text size, group notes by date, show note count, and how often synced accounts check for changes. **Accounts** adds (+) and removes (−) sync accounts.
- Empty new notes are thrown away when you leave them, as in Apple Notes.
- Follows the system's light and dark mode and uses the platform's fonts (Segoe UI and Consolas on Windows, San Francisco and Menlo on macOS, DejaVu on Linux).

## Running

```sh
cargo run            # debug
cargo run --release  # optimized; no console window on Windows
cargo test
```

Set `CLEANNOTES_DATA=<dir>` to keep the data in a folder of your choice, `CLEANNOTES_SEED=1` (with an empty data folder) to start with example notes, and `CLEANNOTES_THEME=light|dark` to override the system's appearance.

On Windows, `build.rs` embeds `assets/CleanNotes.ico` and version information in the executable. The icon is drawn in code (`src/app_icon.rs`); `cargo test app_icon::tests::write_exe_icon -- --ignored` regenerates the file.

## Keyboard shortcuts

Ctrl on Windows and Linux is ⌘ on macOS.

| Keys | Action |
|---|---|
| Ctrl+N | New note |
| Ctrl+Shift+N | New folder |
| Ctrl+Alt+F | Search all notes |
| Ctrl+F | Find in note (Enter / Shift+Enter, Ctrl+G / Ctrl+Shift+G for next / previous) |
| Ctrl+1 / Ctrl+2 | List / gallery view |
| Ctrl+Alt+S | Show or hide folders |
| Ctrl+D | Duplicate note |
| Ctrl+Backspace (Delete or Backspace in the list) | Delete note |
| Ctrl+Z, Ctrl+Shift+Z (in the list or sidebar) | Undo, redo deleting notes: they come back pinned and dated as before |
| ↑ / ↓, Enter or Tab | Move through the list, go into the note |
| Ctrl+Click, Shift+Click, Shift+↑/↓, Ctrl+A (in the list) | Select several notes: add or remove one, or a range; then delete, move (drag or "Move to"), pin or copy them together |
| Ctrl+, | Settings |
| Ctrl+Shift+T / H / J / B | Title / Heading / Subheading / Body |
| Ctrl+Shift+M | Monospaced |
| Ctrl+Shift+7 / 8 / 9 | Bulleted / dashed / numbered list |
| Ctrl+Shift+L | Checklist |
| Ctrl+Shift+U | Mark as checked |
| Ctrl+' | Block quote |
| Ctrl+B / I / U, Ctrl+Shift+X | Bold / italic / underline, strikethrough |
| Ctrl+K | Add link |
| Ctrl+] / Ctrl+[ (Tab / Shift+Tab in lists) | Indent / outdent |
| Ctrl+Alt+↑ / ↓ (⌃⌘↑ / ↓ on macOS) | Move list item up / down |
| Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y | Undo, redo |
| Ctrl+Alt+Shift+C | Copy as Markdown |
| Ctrl+= / Ctrl+- / Ctrl+0 | Bigger / smaller / default text |

## Notes are Markdown

The editor works on a rich-text document, but notes are stored as Markdown, one line per paragraph, so they read the same in any text editor:

| In the note | Markdown |
|---|---|
| Title / Heading / Subheading | `# `, `## `, `### ` |
| Monospaced | a fenced code block |
| Bulleted / dashed / numbered list | `* `, `- `, `1. ` (four spaces per indent level) |
| Checklist | `- [ ] ` and `- [x] ` |
| Block quote | `> ` |
| Bold, italic, strikethrough, underline | `**…**`, `*…*`, `~~…~~`, `<u>…</u>` |
| Link | `[text](url)` |

Characters that would read as markup are escaped. Markdown written elsewhere is read leniently (`+` bullets, `_italic_`, `__bold__`, deeper headings, `1)`, any indentation). See `src/markdown.rs`.

## Sync

Like Apple Notes, every folder belongs to an **account**: *On My PC* (only on this computer) or a synced one. Add accounts under **Settings → Accounts → +**. Each one shows in the sidebar with its own folders and Recently Deleted.

### Bitwarden

Notes go into your Bitwarden vault (bitwarden.com, bitwarden.eu, or a self-hosted Bitwarden or Vaultwarden server) as **secure notes**, encrypted on your computer before they're sent:

- Each note is a secure note named after the note's title. Its notes field holds the note **Jekyll style**: YAML front matter with the note's metadata, then the Markdown:

  ```text
  ---
  id: 6f1c0d3e-…
  created: 2026-11-15T12:03:00.000Z
  modified: 2026-11-15T12:30:12.500Z
  pinned: true
  tags: [travel]
  ---
  # Weekend trip
  - [ ] Passport
  ```

  This format belongs to the Bitwarden backend alone (`src/sync/bitwarden/frontmatter.rs`); the rest of the app only sees notes.
- Notes live in one vault folder (the "Vault folder" setting, `Notes` by default). The app's default *Notes* folder is that vault folder itself, and other app folders are vault folders inside it: app folder `Work/2026` is vault folder `Notes/Work/2026`, which Bitwarden shows nested. Nothing else in the vault is touched.
- Bitwarden allows 10,000 encrypted characters in an item's notes. Longer notes continue in hidden custom fields (`cleannotes-part-2`, …), and `parts:` in the front matter says how many there are.
- A secure note you type into that folder in Bitwarden itself (without front matter) shows up as a note too.
- Notes deleted for good go to Bitwarden's trash. Updating an item keeps what the app doesn't own: its favorite star, master-password re-prompt, other custom fields, password history and its own item key.
- Sign in with your email and master password (authenticator-app or email codes for two-step login, and Bitwarden's new-device email code, are asked for when needed), or with a **personal API key** (Account Settings → Security → Keys), which skips two-step login and captchas. The master password is never stored: signing in unlocks the vault key, which is kept with the session tokens in the OS credential store.

The app speaks Bitwarden's client protocol itself (`src/sync/bitwarden/`): PBKDF2-SHA256 or Argon2id key derivation, HKDF-stretched master keys, AES-256-CBC + HMAC-SHA256 "type 2" encrypted strings, user and per-item keys, token refresh.

### iCloud (CloudKit)

Notes go into the **private database** of a CloudKit container in your iCloud account, through CloudKit Web Services. This is the app's own data in iCloud; Apple doesn't let other apps read Apple Notes' notes.

You need a container of your own:

1. In the [Apple Developer portal](https://developer.apple.com/account/resources/identifiers/list/cloudContainer) create an iCloud container (e.g. `iCloud.com.yourname.notes`).
2. In [CloudKit Console](https://icloud.developer.apple.com/) open the container, go to **API Access → Tokens**, create an API token, and set its **Sign in Callback** to **URL Redirect** with `http://127.0.0.1:52789/` (or another port you then enter in the app).
3. In CleanNotes, **Settings → Accounts → + → iCloud (CloudKit)**: enter the container, environment and API token, and click **Sign In**. Your browser opens Apple's sign-in page; afterwards it returns to the app.

Notes are `Note` records in a custom zone `CleanNotes` (named after the note's id, with its folder path, Markdown body, title, dates, pin and deletion), folders are `Folder` records. In the **Development** environment CloudKit creates these record types on first use; for **Production**, deploy the schema from CloudKit Console first. A record's change tag is its revision, so CloudKit refuses a save based on an outdated copy, and the sync engine merges instead.

### Standard Notes

Notes go into a [Standard Notes](https://standardnotes.com) account (`api.standardnotes.com` or a self-hosted server), end-to-end encrypted on your computer with Standard Notes' protocol 004, so they show up in the Standard Notes apps:

- Each note is a Standard Notes note. Its text is the note's Markdown as it is (new notes use Standard Notes' plain text editor), its title the note's title; pins, dates and the trash (Recently Deleted) map onto Standard Notes' own. Updating a note keeps everything the app doesn't own (other app data, links to other notes and files, the editor, spellcheck, …).
- A note written in Standard Notes with a title that isn't its first line shows with the title on top as a heading; saving takes it off again.
- Folders are **tags**, nested as Standard Notes nests them. A note is in the folder of the tag holding it (the deepest, if several do); untagged notes are in *Notes*. Moving a note to another folder takes it out of its other tags.
- Notes written with another editor (**Super**, rich text, code, spreadsheets, …) and locked notes are shown **read-only**, as plain as their text can be made, under a line saying so. Pinning, moving and deleting them works; changing their text saves the change as a *new* plain note next to the original, which is never overwritten.
- Notes deleted for good are deleted in Standard Notes too.
- Sign in with your email and password (and your authenticator code if you use two-factor authentication). The password is never stored: signing in derives your keys, and only the session and the key that unlocks your items keys are kept, in the OS credential store. Only accounts on protocol 004 work (the Standard Notes apps offer to upgrade older ones); items in shared vaults and hardware-key two-factor aren't supported.

The app speaks Standard Notes' client protocol itself (`src/sync/standardnotes/`): Argon2id root keys, XChaCha20-Poly1305 with the client's authenticated data, items keys, PKCE sign-in, session refresh and the paged items sync. Add an account under Settings → Accounts → + → Standard Notes (standardnotes.com or a self-hosted server; two-factor codes are asked for when needed).

### SQLite database

Notes go into a SQLite file of your choice, as plain tables any SQLite tool can read:

```sql
folders(id, parent_id, name)
notes(id, folder_id, title, body, created, modified, pinned, deleted, revision)
tags(note_id, tag)
meta(key, value)
```

`body` is the note's Markdown; `title` and `tags` are kept up to date for other tools. Several computers can share the file, on a network drive or in a folder synced by Dropbox, OneDrive and the like (as long as they don't write at the same moment).

### How sync works

All backends go through one sync engine (`src/sync/engine.rs`) on a background thread (`src/sync/worker.rs`). A backend only stores and lists folders and notes (`Backend` in `src/sync/mod.rs`):

- Changes are uploaded a few seconds after you stop typing; changes from elsewhere are fetched on launch and every few minutes (Settings → General → Check for changes), or right away with **Sync Now** in an account's right-click menu. When nothing changed on either side, a sync only asks the backend for a change marker.
- **Conflicts:** every upload says which version it was based on. If a note changed on both sides, the edits are merged paragraph by paragraph (pins, folders and deletion field by field). If both sides changed the same paragraph, your version stays in the note and the other becomes a separate note next to it, so nothing is lost.
- **An edit beats a delete:** a note deleted elsewhere but edited here is put back.
- A note edited while a sync is running keeps the edit, merged with what the sync brought in.
- A create whose reply was lost isn't duplicated: notes are matched up by their id.
- If a saved sign-in stops working, the account shows a warning in the sidebar and **Sign In Again…** in Settings.

To add a backend: implement `Backend`, add a `BackendConfig` variant, and open it in `sync::open`.

## Data

Everything is kept in `data.db`, a SQLite database in `%APPDATA%\CleanNotes` on Windows (`~/Library/Application Support/CleanNotes` on macOS, `~/.local/share/cleannotes` on Linux), or `CLEANNOTES_DATA`. Notes are rows with their Markdown; accounts, folders and sync bookkeeping are JSON. Sign-ins (a Bitwarden session, a CloudKit web auth token, a Standard Notes session and key) go to the OS credential store (Windows Credential Manager, macOS Keychain, Secret Service), under `CleanNotes` / `account-<id>`; without one, into `data.db` with a warning.

## Code layout

| File | Purpose |
|---|---|
| `src/main.rs` | Window setup |
| `src/app.rs` | App state, note and folder actions, shortcuts, saving, sync scheduling, dialogs |
| `src/app/sidebar.rs` | Accounts, folders, Recently Deleted, tags, New Folder |
| `src/app/list.rs` | The notes list and the gallery |
| `src/lock.rs`, `src/app/locked.rs` | Locked notes: encryption, and the password, lock screen and unlocking |
| `src/app/note_pane.rs` | The note's toolbar, Aa and "…" menus, search, find bar, link sheet |
| `src/app/settings_window.rs` | Settings: General and Accounts, with the sign-in sheets |
| `src/editor/mod.rs` | The rich-text editor: input, commands, undo, find, clipboard |
| `src/editor/layout.rs` | Laying out and painting paragraphs, list markers, checkboxes |
| `src/doc.rs` | The rich-text document and all editing operations (no UI) |
| `src/markdown.rs` | Markdown ↔ document, titles and snippets |
| `src/model.rs` | Accounts, folders, notes, and the in-memory store |
| `src/db.rs` | Local SQLite storage and the credential store |
| `src/settings.rs` | Preferences and their defaults |
| `src/sync/mod.rs` | The `Backend` trait and backend settings |
| `src/sync/engine.rs` | The sync algorithm, shared by all backends |
| `src/sync/merge.rs` | Three-way merges of note text |
| `src/sync/worker.rs` | The background sync thread |
| `src/sync/sqlite.rs` | The SQLite backend |
| `src/sync/bitwarden/` | The Bitwarden backend: protocol, crypto, front matter |
| `src/sync/cloudkit/` | The CloudKit backend |
| `src/sync/standardnotes/` | The Standard Notes backend: protocol 004 crypto, sign-in, items sync, notes and tags |
| `src/theme.rs`, `src/fonts.rs`, `src/icons.rs`, `src/app_icon.rs` | Colors, fonts, drawn icons, the app icon |

## Tests

`cargo test` runs everything offline:

- **Document and Markdown:** every paragraph and character style round-trips; escaping; Markdown from elsewhere; editing operations (Return in lists, Backspace at a bullet, pasting, numbering, moving items); the editor's undo grouping, auto-lists, find, links and checklist sorting.
- **Sync engine** (`src/sync/engine/tests.rs`): two "devices" with their own stores share one SQLite backend: merges, clashing edits kept as two notes, edit beats delete, Recently Deleted and pins, folder moves, renames and deletes, edits during a sync, lost replies, offline changes on both sides.
- **Bitwarden** (`src/sync/bitwarden/tests.rs`) against a fake server (`fake.rs`) that only ever sees encrypted data and enforces revision checks, Bitwarden's length limits and token expiry: password, Argon2id, two-step, new-device and API-key sign-ins; round trips, conflicts, 100 KB notes, items typed into the vault, fields kept on update, folder renames, trash, token refresh, and two devices syncing through the vault. The key derivation and encryption are checked against vectors computed independently in Node.js (`kat.rs`).
- **CloudKit** (`src/sync/cloudkit/tests.rs`) against a fake CloudKit Web Services server: browser sign-in through the loopback callback, records and change tags, conflicts, paging, folder renames, expired sign-ins, and two devices syncing.
- **SQLite backend, local storage, model** unit tests.

- **Standard Notes** (`src/sync/standardnotes/tests.rs`) against a fake server that only sees protocol 004 strings, checks the PKCE verifier, enforces sync conflicts by timestamp, pages downloads and expires sessions: sign-in (two-factor, email retried in lowercase), items keys, round trips, conflicts, tags as folders, the trash, read-only Super notes, paging and incremental syncs, session refresh, and two devices syncing. The key derivation and encryption are checked against the official client's own test vectors, including an items key from a real account's backup (`kat.rs`).

The Bitwarden, CloudKit and Standard Notes backends have only been tested against those fakes, not against the real services.

## Ideas for next steps

- Tables, images and attachments, locked notes, sharing
- Smart folders, Quick Notes, multi-select in the list, print and export
- A menu bar on Windows and Linux
