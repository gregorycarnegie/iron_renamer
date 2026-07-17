# Changelog

## Unreleased

- Header now shows the app icon instead of a hand-drawn anvil mark.

## 0.3.3 - 2026-07-17

- Fixed the GUI freezing for many seconds when dropping a large number of files, especially from network shares: the drop is now processed as a single batch, folder lookups are batched per directory and run in parallel, and the scan happens off the UI thread with a "scanning…" status while the list fills in.

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
