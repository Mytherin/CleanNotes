# CleanNotes

A notes app written in Rust that looks and behaves like Apple Notes, with pluggable sync: **Amazon S3** (or an S3-compatible service) or a **SQLite database** file.
The desktop app is built on [egui/eframe](https://github.com/emilk/egui), like its sibling [CleanCalendar](../CleanCalendar); the [iPhone app](#iphone-app) is SwiftUI and UIKit on the same Rust core.

## Features

- **Three panes like Apple Notes:** folders (per account, nested, with note counts), the notes list (title, time and first line, in sections: Pinned, Today, Yesterday, Previous 7 Days, Previous 30 Days, months, years), and the note. A **gallery view** shows notes as thumbnails (double-click opens one).
- **The macOS 26 look:** the sidebar floats as a rounded panel with the selected folder as a yellow pill; the list has a header with the folder's name and note count and a round "…" menu (list or gallery, sort, group by date, note counts), with large date sections; the toolbar has round glass buttons (New Note, "…" note menu, Search, which opens into a field) and a capsule with Aa, checklist and link.
- **Rich text editor** with Apple Notes' paragraph styles: Title, Heading, Subheading, Body, Monospaced, bulleted (•), dashed (–) and numbered lists, checklists, and block quotes; bold, italic, underline, strikethrough and links. Lists indent and outdent (Tab / Shift+Tab), list items move up and down, and checked items can sort to the bottom. Typing `- `, `* `, `1. ` or `[] ` at the start of a line starts a list. Plain URLs and `#tags` are highlighted; links open on click.
- **Images:** "Attach Image…" in the note's "…" menu or the editor's right-click menu, dropping picture files on the window, or pasting a picture (Ctrl+V) puts it in the note on a line of its own, as large as it is but no wider than the text. Double-click a picture to open it in the system's viewer. Pictures (PNG, JPEG, GIF, WebP, BMP; HEIC is kept but shown as a box) are copied into the data folder, named by their content, and sync with the note (see [Sync](#sync)). Locked notes can't have pictures, and notes with pictures can't be locked, since the pictures wouldn't be encrypted with the note.
- **The Aa menu** (B I U S on top, paragraph styles shown in their own style below), checklist and link buttons in the toolbar, the note's "…" menu (pin, find, move, duplicate, copy as Markdown, delete), and a right-click menu in the editor.
- **Find in note** (Ctrl+F, with match count and next/previous) and **search all notes** from the toolbar (every word must match; matches are highlighted in the note).
- **Folders:** new (Ctrl+Shift+N or the New Folder button at the top of the sidebar), rename (double-click), delete (notes go to Recently Deleted), subfolders, drag notes onto a folder, or "Move to" from a note's menu, also into another account. **Drag a folder** onto another folder to nest it there, or onto an account's name to move it to the top level (also "Move To" in the folder's right-click menu); it takes its subfolders and notes along, into another account too. A name already used there gets a number ("Work 2").
- **Locked notes:** "Lock Note" in a note's "…" menu or right-click menu encrypts it with a password you choose (with several notes selected, the right-click menu's "Lock N Notes" locks them all at once) (set the first time, with a hint; change it in Settings → General). As in Apple Notes, a locked note's title stays visible in the list; its text is encrypted with AES-256 and an HMAC check, under a key made from the password with Argon2id, and it is stored and synced only in that form, so sync services never see it. One password opens every locked note until Lock Now, 15 minutes without use, or quitting; passwords are never saved (except, on the iPhone, in its keychain behind Face ID, if you turn that on). A locked note edited on two devices at once is kept as two notes. If the password is forgotten, locked notes can't be recovered.
- **All Notes** at the top of the sidebar once there is more than one folder: every note of every account in one list, each with its folder.
- **Recently Deleted** per account: notes stay 30 days, can be recovered, and can't be edited until they are (as in Apple Notes).
- **Tags:** `#words` in notes become tags in the sidebar; a new note made in a tag carries it.
- **Pinning, duplicating** (Ctrl+D), **copy as Markdown**, and a clipboard that keeps formatting within the app.
- **Settings** (⚙ or Ctrl+,), laid out like Apple Notes': default account, sort notes by (date edited, date created, title), new notes start with (Title, Heading, Subheading, Body), sort checked items (manually or automatically), default text size, group notes by date, show note count, and how often synced accounts check for changes. **Accounts** adds (+) and removes (−) sync accounts.
- **Import from Evernote:** Settings → General → "Import from Evernote…" (or drop `.enex` files on the window) adds the notes of Evernote export files, also those made from Apple Notes, to an account of your choice, keeping their dates and formatting (headings, lists, checklists, monospaced text, tables, links). Each notebook (the file's name) becomes a folder, and its tags become #tags; in exports where every note has exactly one tag, as in those made from Apple Notes, that tag is the note's folder. Pictures become the note's own images, where they were; other attachments (PDFs, audio, …) are saved in the data folder's `Attachments` folder and linked from the note. Importing a file again skips the notes already imported.
- Empty new notes are thrown away when you leave them, as in Apple Notes.
- Follows the system's light and dark mode and uses the platform's fonts (Segoe UI and Consolas on Windows, San Francisco and Menlo on macOS, DejaVu on Linux).

## Running

```sh
cargo run            # debug
cargo run --release  # optimized; no console window on Windows
cargo test --workspace  # the core, the desktop app and the bindings
```

Set `CLEANNOTES_DATA=<dir>` to keep the data in a folder of your choice, `CLEANNOTES_SEED=1` (with an empty data folder) to start with example notes, and `CLEANNOTES_THEME=light|dark` to override the system's appearance.

On Windows, `build.rs` embeds `assets/CleanNotes.ico` and version information in the executable. The icon is drawn in code (`src/app_icon.rs`); `cargo test app_icon::tests::write_exe_icon -- --ignored` regenerates the file.

## Downloads

The [Build workflow](.github/workflows/build.yml) builds every push to `main` on GitHub Actions; the builds are the run's artifacts (Actions → the run → Artifacts). Pushing a tag like `v0.2.0` also attaches them to a GitHub release.

- **Windows:** `CleanNotes.exe`, nothing to install.
- **macOS:** `CleanNotes.dmg` with the app for Apple silicon and Intel Macs (`macos/make-app.sh` makes it on a Mac). It isn't signed with an Apple Developer ID, so the first time, open it with right-click → Open.
- **iPhone:** `CleanNotes-unsigned.ipa`. iPhones only run signed apps: install it with a tool that signs it with your Apple ID, such as [Sideloadly](https://sideloadly.io) or [AltStore](https://altstore.io) (with a free Apple ID, it has to be signed again every 7 days). `CleanNotes-Simulator.zip` runs in Xcode's iOS Simulator. TestFlight or App Store builds would need a paid Apple Developer account's certificate in the workflow's secrets.

## iPhone app

`ios/` is the iPhone app: native SwiftUI and UIKit for the screens, so it looks, scrolls and responds like Apple Notes. It shares everything else with the desktop app through the Rust core (`core/`), which it calls through [UniFFI](https://mozilla.github.io/uniffi-rs/) bindings (`ffi/`). That includes the notes, folders and accounts, the list and its sections, Markdown, the editing engine behind the text view, locked notes, storage and sync. Both apps read and write the same notes, and sync with each other through an S3 account.

### Building

On a Mac with Xcode 15 or later, [rustup](https://rustup.rs) and [XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`):

```sh
ios/build-rust.sh          # the Rust core as an XCFramework, and its Swift bindings
cd ios && xcodegen         # makes CleanNotes.xcodeproj from project.yml
open CleanNotes.xcodeproj  # choose your team under Signing, then Run
```

Run `build-rust.sh` once before `xcodegen`, since the project includes the generated `Generated/cleannotes_ffi.swift`. After that, Xcode runs the script before every build: a debug Rust build for Debug and a release build for Release. Set `CLEANNOTES_SEED=1` in the scheme's environment to start with example notes.

**Tests:** ⌘U in Xcode, or `xcodebuild test -project CleanNotes.xcodeproj -scheme CleanNotes -destination 'platform=iOS Simulator,name=iPhone 16'`. `ios/CleanNotesTests` drives the real text view and the Rust engine together, typing as the keyboard does (lists started and ended, Backspace, emoji, undo, checklist commands, list markers, sharing), and checks after each edit that the two agree. The Build workflow runs them on every push and pull request. The engine's side of the same cases is in `cargo test` (`ffi/src/lib.rs`).

### What it does, as in Apple Notes

- **Folders:** each account's folders with note counts (nested, collapsible accounts), Recently Deleted, tags, and All Notes once there are several folders. New Folder and the compose button are in the bottom toolbar. **Smart Folders** (New Folder → New Smart Folder) gather the notes of every folder that pass their filters: #tags (any or all of them), checklists (any, with unchecked items, all checked), pinned, locked, and edited today or in the last 7 or 30 days; touch and hold one to edit it. They're kept on the device, with the settings. Swipe a folder to delete it, or touch and hold to rename it, add a subfolder or move it (Move). Drag a folder onto another to nest it there, or onto an account's name to move it to the top level. Search there looks through every note: results come as **Top Hits** (notes whose title matches) and **Notes**, with the line where the words are found and the words in bold, and a note opened from them shows its matches (also when searching a folder's list).
- **Notes list:** large title, date sections (Pinned, Today, Yesterday, Previous 7 Days, …; tap a section's header to fold it away), each note's title, time, first line and (in mixed lists) folder. Swipe right to pin. Swipe left to delete or move. Touch and hold for a preview and a menu (pin, lock, share, move, duplicate, delete). The "…" menu switches to the gallery view, selects several notes (Move, Delete, Recover All in Recently Deleted), and sets sort and grouping. The bottom toolbar shows the note count and the compose button. Pull down to sync; **shake to undo** deleting notes.
- **Note:** the date on top, then the rich text with the same paragraph styles as the desktop: titles, headings, lists, checklists you tick by tapping the circle, quotes and monospaced. While typing, undo, redo and Done are in the navigation bar, and the keyboard bar has Aa, checklist, indent, **photo** (the system photo picker; photos are saved upright as JPEG so every device can show them) and link. Pasting a picture attaches it too, and pictures synced from other devices show as they arrive. **Aa** replaces the keyboard with the format panel (styles, B I U S, lists, indents). The usual iOS editing works: autocorrect, dictation, the edit menu, ⌘B/⌘I/⌘U and ⌘Z on a keyboard, and shake to undo typing. The edit menu also has Format (B I U S), Add Link, Move Up and Down for list items, and a checklist's Check All, Uncheck All, Move Checked to Bottom and Delete Checked Items. A hardware keyboard has Apple Notes' shortcuts: ⇧⌘T Title, ⇧⌘H Heading, ⇧⌘J Subheading, ⇧⌘B Body, ⇧⌘M Monospaced, ⇧⌘7/8/9 lists, ⇧⌘L checklist, ⇧⌘U mark as checked, ⌘' quote, ⌘] and ⌘[ indent, ⌘K link, ⌃⌘↑/↓ move items. "…" has Find in Note (the system find bar), the checklist actions (in a checklist), Move, Pin, Lock, Print and Delete. **Tables** (the Table button in the toolbar and keyboard bar, ⌥⌘T, or the edit menu) are the same as the desktop's: each row shows as a box with its cells one under another, since the phone is too narrow for the columns side by side; the Table menu adds and deletes rows and columns, turns on a header row or column, and goes to the next cell (Tab and ⇧Tab on a keyboard), and Return in a cell is a new line in it. Recently Deleted notes can be read and recovered but not edited. Swipe from the left edge to go back.
- **Home Screen quick action:** touch and hold the app's icon for **New Note**, as in Apple Notes.
- **Locked notes** with the same password and encryption as the desktop, and **Face ID** (or Touch ID) as in Apple Notes: turn on "Use Face ID" in Settings → Password and the password is kept in the keychain, on this iPhone only, readable only after Face ID (enrolling another face forgets it). It's off until you turn it on. **Settings** has the same options, and the accounts: **Add S3 Account** checks the bucket as the desktop does (optionally with an encryption key), and **Change Access Key** gives an account new keys. SQLite syncing is only in the desktop app, since a database file needs a shared folder.

### How the text view uses the shared editor

The note is a `UITextView` (TextKit 1) whose text is the editing engine's document, positions converted to UTF-16 as UIKit counts them (`ffi/src/lib.rs`, `NoteEditor`). Every edit goes to the engine first:

- **Plain typing** (the engine confirms the result is exactly what the text view would make) is left to the text view, so the keyboard's autocorrect, predictions and marked text work as in any app.
- **Edits that change paragraphs** are refused and the note is shown again from the engine. These include Return continuing or ending a list, `- ` / `* ` / `1. ` / `[] ` starting one, Backspace taking a list style off, pasting several lines, and anything from the Aa panel. This is the same code the desktop editor runs (`core/src/editing.rs`, `Editing::replace_range`).
- If the text view ever changes its text without saying how, the engine catches up from a diff.

List markers, checklist circles, quote bars and code backgrounds are drawn in the margin by a layout manager (`ios/CleanNotes/NoteTextView.swift`).

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
| Image | `![](attachments/<sha256>.<ext>)` |

Characters that would read as markup are escaped. Markdown written elsewhere is read leniently (`+` bullets, `_italic_`, `__bold__`, deeper headings, `1)`, any indentation). See `src/markdown.rs`.

## Sync

Like Apple Notes, every folder belongs to an **account**: *On My PC* (only on this computer) or a synced one. Add accounts under **Settings → Accounts → +**. Each one shows in the sidebar with its own folders and Recently Deleted.

Earlier versions could also sync with Bitwarden, iCloud (CloudKit) and Standard Notes. Those are gone: an account of one of them becomes a folder named after it in *On My PC*, with its folders and notes as they were here, and its saved sign-in is deleted. Nothing on the service itself is touched.

### Amazon S3

Notes go into a folder of your choice in an S3 bucket, as Markdown files any tool can read. S3-compatible services (Cloudflare R2, MinIO, …) work too, if they support conditional writes:

```text
<folder>/head                  rewritten with every change, so other devices notice cheaply
<folder>/notes/<id>.md         a note: YAML front matter (id, folder, dates, pin, tags), then its Markdown
<folder>/folders/<id>.json     a folder: its path, e.g. ["Work","2026"]
<folder>/attachments/<name>    a picture a note shows, named by its SHA-256 (as in the note's Markdown)
```

1. In the AWS console, create a bucket (keep "Block all public access" on; turning on versioning keeps old versions of notes as a backup).
2. In IAM, create a user without console access, with an inline policy that allows only `s3:ListBucket` on `arn:aws:s3:::BUCKET` and `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject` on `arn:aws:s3:::BUCKET/*`, and create an access key for it ("Application running outside AWS").
3. In CleanNotes, **Settings → Accounts → + → Amazon S3**: enter the account name, bucket, region, folder and access key. Adding the account checks that the key can list, read and write the folder, and that the service refuses outdated writes. The key is kept in the system's credential store.

An object's ETag is its revision: updates are written with `If-Match` and new notes with `If-None-Match: *`, so S3 refuses a save based on an outdated copy and the sync engine merges instead. A note deleted for good leaves a small tombstone file (cleaned up after 30 days), so a delete loses to an edit made elsewhere. When nothing changed, a sync is a single `HEAD` request on `head`; otherwise the folder is listed and only changed notes are downloaded, several at once. Everything is listed at least every 15 minutes. A `.md` file without front matter put into `notes/` by hand shows up as a note.

**Pictures** are uploaded (with `If-None-Match: *`: one already there is the same picture) just before the note that shows them, so another device never gets a note without its pictures. They're downloaded after the notes, only those this device doesn't have, and kept only if their content matches their name. A note whose pictures haven't arrived shows a box in their place, and syncs keep fetching them, even when nothing else changed.

**Encryption (optional):** enter an **encryption key** (a passphrase) when adding the account, and notes and folders are encrypted on your computer before upload, so S3 and anyone with access to the bucket see only ciphertext. The key is made from the passphrase with Argon2id (64 MiB, 3 passes); its random salt and a check value are in `<folder>/encryption.json`, so every device with the same passphrase gets the same key, and a wrong one is refused. Each file is XChaCha20-Poly1305 with a random nonce, bound to its file name. Pictures are encrypted the same way (as bytes), under names that are an HMAC of their content hash, so the bucket's listing doesn't reveal which (known) pictures are there. Only the key is saved, in the credential store, never the passphrase. Other devices must enter the same key; one without it sees the notes as unreadable and leaves them alone. If the passphrase is lost, the notes can't be recovered. Notes already in a folder before encryption is turned on stay readable and are encrypted the next time they're saved. Without an encryption key, notes are stored as plain Markdown (locked notes stay encrypted either way).

Requests are signed with AWS Signature V4 by the app itself (`core/src/sync/s3/`), on the same blocking HTTP client as the other backends.

### SQLite database

Notes go into a SQLite file of your choice, as plain tables any SQLite tool can read:

```sql
folders(id, parent_id, name)
notes(id, folder_id, title, body, created, modified, pinned, deleted, revision)
tags(note_id, tag)
meta(key, value)
attachments(name, data)
```

`body` is the note's Markdown; `title` and `tags` are kept up to date for other tools. `attachments` holds the pictures notes show, by name. Several computers can share the file, on a network drive or in a folder synced by Dropbox, OneDrive and the like (as long as they don't write at the same moment).

### How sync works

All backends go through one sync engine (`src/sync/engine.rs`) on a background thread (`src/sync/worker.rs`). A backend only stores and lists folders, notes and pictures (`Backend` in `src/sync/mod.rs`):

- Changes are uploaded a few seconds after you stop typing; changes from elsewhere are fetched on launch and every few minutes (Settings → General → Check for changes), or right away with **Sync Now** in an account's right-click menu. When nothing changed on either side, a sync only asks the backend for a change marker.
- **Conflicts:** every upload says which version it was based on. If a note changed on both sides, the edits are merged paragraph by paragraph (pins, folders and deletion field by field). If both sides changed the same paragraph, your version stays in the note and the other becomes a separate note next to it, so nothing is lost.
- **An edit beats a delete:** a note deleted elsewhere but edited here is put back.
- A note edited while a sync is running keeps the edit, merged with what the sync brought in.
- A create whose reply was lost isn't duplicated: notes are matched up by their id.
- **Pictures** go up before the note that shows them and come down after it. They're named by their content, so they never conflict, and a damaged download is never kept.
- If a saved sign-in stops working, the account shows a warning in the sidebar and **Sign In Again…** in Settings.

To add a backend: implement `Backend`, add a `BackendConfig` variant, and open it in `sync::open`.

## Data

Everything is kept in `data.db`, a SQLite database in `%APPDATA%\CleanNotes` on Windows (`~/Library/Application Support/CleanNotes` on macOS, `~/.local/share/cleannotes` on Linux), or `CLEANNOTES_DATA`. Notes are rows with their Markdown; accounts, folders and sync bookkeeping are JSON. Pictures are files in the `Attachments` folder next to it, named `<sha256>.<ext>`. Sign-ins (an S3 access key and encryption key) go to the OS credential store (Windows Credential Manager, macOS Keychain, Secret Service), under `CleanNotes` / `account-<id>`; without one, into `data.db` with a warning.

## Code layout

A Cargo workspace: the shared core, the desktop app on it, and the core's Swift bindings for the iPhone app.

**`core/` (cleannotes-core), shared by both apps:** everything that isn't drawing.

| File | Purpose |
|---|---|
| `core/src/library.rs` | The notes session: the list and its sections, new notes, deleting and undoing it, moving, pinning, folders, accounts, saving, sync scheduling, locked notes' password and keys |
| `core/src/editing.rs` | The editing engine: typing, Return and Backspace in lists, auto-lists, paragraph and character styles, undo grouping, find, clipboard, edits as a system text view reports them |
| `core/src/doc.rs` | The rich-text document and all editing operations; links, tags and font sizes |
| `core/src/markdown.rs` | Markdown ↔ document, titles and snippets |
| `core/src/attachments.rs` | Pictures attached to notes: content-addressed files, their names and references |
| `core/src/model.rs` | Accounts, folders, notes, and the in-memory store |
| `core/src/db.rs` | Local SQLite storage and the credential store |
| `core/src/settings.rs`, `core/src/dates.rs` | Preferences and their defaults; how dates are shown |
| `core/src/lock.rs` | Locked notes' encryption |
| `core/src/sync/mod.rs` | The `Backend` trait and backend settings |
| `core/src/sync/engine.rs` | The sync algorithm, shared by all backends |
| `core/src/sync/merge.rs` | Three-way merges of note text |
| `core/src/sync/worker.rs` | The background sync thread |
| `core/src/sync/sqlite.rs` | The SQLite backend |
| `core/src/sync/s3/` | The S3 backend: Signature V4, Markdown files, optional encryption |

**`src/` (the desktop app):** what's drawn and selected.

| File | Purpose |
|---|---|
| `src/main.rs` | Window setup |
| `src/app.rs` | Selection, focus, shortcuts, dialogs, and the panes on the library |
| `src/app/sidebar.rs` | Accounts, folders, Recently Deleted, tags, New Folder |
| `src/app/list.rs` | The notes list and the gallery |
| `src/app/locked.rs` | The lock screen and password sheets |
| `src/app/note_pane.rs` | The note's toolbar, Aa and "…" menus, search, find bar, link sheet |
| `src/app/settings_window.rs` | Settings: General and Accounts, with the sign-in sheets |
| `src/app/import.rs`, `src/enex.rs` | Importing Evernote exports (`.enex`) |
| `src/editor/mod.rs` | The editor on the editing engine: keys, pointer, caret, selection, context menu |
| `src/editor/layout.rs` | Laying out and painting paragraphs, list markers, checkboxes, pictures |
| `src/editor/images.rs`, `src/app/attach.rs` | Pictures as textures (decoded off the UI thread); attaching them (dialog, drop, paste) |
| `src/theme.rs`, `src/fonts.rs`, `src/icons.rs`, `src/app_icon.rs` | Colors, fonts, drawn icons, the app icon (`write_ios_icon` makes the iPhone's) |

**`ffi/` (cleannotes-ffi)** is the core for Swift: `NotesCore` (the library), `NoteEditor` (the editing engine, in UTF-16 positions) and sign-in, as UniFFI records and enums. **`ios/`** is the iPhone app: `CleanNotes/*.swift` (screens, the text view), `project.yml` (XcodeGen) and `build-rust.sh`.

## Tests

`cargo test --workspace` runs everything offline:

- **Document and Markdown:** every paragraph and character style round-trips; escaping; Markdown from elsewhere; editing operations (Return in lists, Backspace at a bullet, pasting, numbering, moving items); the editor's undo grouping, auto-lists, find, links and checklist sorting.
- **Sync engine** (`core/src/sync/engine/tests.rs`): two "devices" with their own stores share one SQLite backend: merges, clashing edits kept as two notes, edit beats delete, Recently Deleted and pins, folder moves, renames and deletes, edits during a sync, lost replies, offline changes on both sides.
- **S3** (`core/src/sync/s3/tests.rs`) against a fake S3 that checks Signature V4 and honours conditional writes: Markdown objects, conflicts, tombstones (and never deleting a note brought back over one), hand-written and unreadable files, paged listings, downloading only what changed, folder renames moving their notes, encryption, the bucket check, two devices syncing, and pictures travelling with their notes (byte for byte, uploaded before the note, sealed and renamed when encrypted, fetched again when missing).
- **SQLite backend, local storage, model** unit tests; the **library** (list sections, new notes, deletion undo, folders, sign-ins replaced mid-sync) and the **bindings** (UTF-16 positions, which edits the text view may make itself, notes and locking through `NotesCore`).

## Ideas for next steps

- Tables, attachments other than pictures (PDFs, audio), pictures in locked notes, sharing
- Removing pictures no note shows any more (locally and from the backends)
- Smart folders, Quick Notes, multi-select in the list, print and export
- A menu bar on Windows and Linux
