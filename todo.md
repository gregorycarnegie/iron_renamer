# Iron Renamer feature-parity checklist

Compared with the [Advanced Renamer v4 user guide](https://www.advancedrenamer.com/user_guide/v4/complete_guide) and the bundled v4.23 reference programs. Iron Renamer already has ordered Replace, Regex, Case, and Pattern rules; natural sorting; live preview; collision warnings; batch rename; and one-level undo.

## P0 — make the current renamer safe and complete

- [x] Share one batch planner/executor between the GUI and CLI so both report and handle errors identically. (`src/batch.rs`)
- [x] Make rename batches safe for swaps and chains (`a -> b`, `b -> a`) with temporary names and rollback after partial failure.
- [x] Report the number actually renamed, not the number planned, and preserve failed operations for retry.
- [x] Validate empty/invalid filenames, reserved Windows names, path-length limits, and case-only renames before starting.
- [x] Persist dated batch history and allow selective undo; keep history after failed or partial undo. (CLI `history` / `undo [ID]`; GUI undoes the latest batch)
- [x] Add folder renaming as a separate mode; never mix files and folders in one batch. (CLI `--dirs`; GUI `＋ Folders`)

## P1 — everyday workflow

### File list

- [x] Add drag-and-drop for files and folders. (winit `DroppedFile` hook; dropped folders load contents per mask/recurse settings, or add as folder items in folder mode)
- [x] Let users remove selected rows and reorder rows (numbering follows list order), instead of only clearing everything. (click a row to select; ▲/▼/Remove in the selection bar)
- [x] Add recursive folder loading plus include/exclude masks such as `*.jpg;*.png`. (`recurse` toggle; masks field, `!` prefix excludes)
- [x] Add list search, sort controls, and ascending/descending order. (search filters the view only; sort by name/ext/size/date reorders the list)
- [x] Save and load plain-text file lists.
- [x] Allow a per-item manual new-name override. (select a row, type in the override field; overrides bypass rules but are validated the same)

### Rules

- [x] Add `Apply to: name | extension | both` to every applicable rule; the current rules always edit the full filename. (`:name`/`:ext` flag mods; GUI apply-to chips)
- [x] Add per-rule conditions on original/new name, extension, and path (contains, starts/ends with, equals, regex, and negation). (CLI `--if`; engine-level — no GUI editor yet)
- [x] Expand Replace with case sensitivity, first/Nth/all occurrence, and multiple replacement pairs. (pairs = stacked Replace rules, same effect)
- [x] Add Insert/Add text at a position, from either end, with literal or regex anchors.
- [x] Add Remove by position, pattern, character class/list, numbers, case, or diacritics.
- [x] Add Trim at start, end, both, or throughout, including inverse matching.
- [x] Add Renumber for an existing number (Nth number, absolute/relative, start, step, and padding).
- [x] Add Move-substring and Swap-around-separator rules.
- [x] Add List names: paste/load one new name per item and populate from current names. (GUI paste box; CLI `--names FILE`; populate-from-current still todo)
- [x] Expand Case with inverted case and location controls (all, first letter, each word, position, or pattern).

### Patterns and tags

- [x] Replace the three hard-coded Pattern substitutions with one tag parser shared by every text rule and destination path. (`src/tags.rs`: `<name> <ext> <num> <index> <parent>`; destination paths arrive with Copy/Move modes)
- [x] Add counter tags with start, step, padding, decrementing, alphabetic, hex, Roman, and per-folder variants. (`<num|hex|alpha|roman|dirnum[:START[:STEP]]>`; negative STEP decrements; pad via batch pad or `|pad:N`)
- [x] Add original name/extension, parent-folder, path, index, file-size, and checksum tags. (`<oname> <oext> <parent> <path> <index> <size[:kb|mb]> <crc32>`)
- [x] Add current/created/modified date-time tags with formatting and offsets. (`<now|created|modified[:FMT[:OFFSET]]>`, UTC)
- [x] Add random number/string tags. (`<rand[:MIN[:MAX]]> <rands[:LEN]>`)
- [x] Add tag modifiers (fallback/default, upper/lower/title, substring, pad, trim, replace, and arithmetic).
- [x] Add a tag picker and item-details panel instead of requiring users to memorize tag syntax. (collapsible tag chips insert into the active rule; clicking a file row shows path/size/dates)

### Batch features

- [ ] Add Rename/Copy/Move modes with a previewed destination path, tag-expanded subfolders, and automatic directory creation.
- [ ] Add collision policies: fail, append incrementing number/letter, or append a tag pattern.
- [ ] Save/load rule presets and remember recent presets/patterns.
- [ ] Import original/new names from CSV and export the preview as text, CSV, or JSON.

## P2 — media and automation

- [ ] Read image, audio, video, document, and executable metadata and expose fields as tags. The reference bundle uses ExifTool; prefer invoking a user-installed ExifTool rather than shipping its Perl runtime.
- [ ] Add common image dimensions/date-taken, audio artist/album/track, video duration, and document author/title tags.
- [ ] Add file-pair mode so sidecars and alternate formats with the same stem receive the same generated name.
- [ ] Add a Timestamp rule for created/modified/accessed times using absolute, delta, filename-pattern, parent-folder, or metadata values.
- [ ] Let the CLI execute a saved preset against a directory, recursive tree, item-list file, or explicit files, with masks/regex, sorting, verify-only mode, and a result log.
- [ ] Add keyboard shortcuts for add/remove/select/search, manual override, and starting a batch.

## P3 — only if real users need full parity

- [ ] Add sandboxed JavaScript rules with pre-batch state and explicit file-read permissions. The reference bundle includes QuickJS, but this is not needed for normal rename jobs.
- [ ] Add GPS coordinate tags and optional online reverse geocoding for city/state/country.
- [ ] Add a metadata writer/editor separate from filename and filesystem-timestamp operations.
- [ ] Add Windows Explorer context-menu and preset file association integration.
- [ ] Add localization and configurable UI/metadata-analysis settings.

## Verification

- [ ] Add one end-to-end test covering preview, a swap/chain rename, collision handling, and undo in a temporary directory.
- [ ] Add table-driven rule/tag tests for Unicode names, dotfiles, multiple extensions, extensionless files, and case-insensitive filesystems.
- [ ] Test copy/move/rename recovery by forcing a failure halfway through a batch.
