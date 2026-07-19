# Changelog

## 0.4.2 - 2026-07-19

- Multi-select in the file list: Ctrl+click toggles rows, Shift+click selects a range, Shift+↑/↓ extends from the keyboard. Remove, the Delete key, and the context menu act on the whole selection.
- Right-click context menu on file rows, themed to match the app. Single file: rename override, show in Explorer, remove. Multiple selected: remove selected, keep only selected, clear selection.

## 0.4.1 - 2026-07-19

- Case-insensitive Replace and List Replace compile their matchers once instead of once per file (about 100× faster in the 100,000-operation benchmark).
- Large previews reuse normalized path keys, making 10,000-file planning about 20% faster.

## 0.4.0 - 2026-07-17

- Metadata is now built in — ExifTool is no longer required (the `IRON_RENAMER_EXIFTOOL` setting is gone). `<exif:TAG>`, the metadata aliases, `--touch =exif`, and `--set-meta` all work out of the box via pure-Rust readers/writers.
- `<width>`/`<height>` now work even for images without EXIF data (read from the file header).
- Known limits vs ExifTool: no RAW-format or PDF/Office metadata (`<author>` still works for videos), and `--set-meta` supports a curated tag set — audio: artist, album, title, genre, comment, track, year, albumartist; images: artist, description, copyright, make, model, datetimeoriginal, createdate, software.

## 0.3.4 - 2026-07-17

- Header now shows the app icon instead of a hand-drawn anvil mark.
- Fixed Copy and Move for folder batches, including cross-volume moves.
- Internal cleanup: the GUI and the batch planner now share one per-folder listing cache, and file masks compile to regexes once instead of using a hand-rolled matcher (also removes its pathological slowdown on masks like `*a*a*a*`).

## 0.3.3 - 2026-07-17

- Fixed the GUI freezing for many seconds when dropping a large number of files, especially from network shares: the drop is now processed as a single batch, folder lookups are batched per directory and run in parallel, and the scan happens off the UI thread with a "scanning…" status while the list fills in.

## 0.3.2 - 2026-07-16

- Added an About dialog with the app version and Slint attribution.
- Added Windows and Linux ARM64 release builds and Windows MSIX packages.

## 0.3.1 - 2026-07-16

- Made large rename chains scale linearly instead of quadratically.

## 0.3.0 - 2026-07-16

- Frameless window with a custom in-app title bar on Windows and Linux (VS Code style): the header hosts the caption buttons, drags the window, and double-click maximizes; edge grips resize. macOS keeps the native frame.

## 0.2.1 - 2026-07-15

- Internal cleanup: shared invalid-character constant, removed redundant CLI bindings.

## 0.2.0 - 2026-07-15

- Added Linux and macOS support alongside Windows.
- Added cross-platform CI and release builds.

## 0.1.0 - 2026-07-14

- Initial release with GUI and CLI batch renaming, previews, collision handling, and undo history.
