# Iron Renamer

Batch file renamer in Rust — a personal, minimal take on [Advanced Renamer](https://www.advancedrenamer.com/).
One binary, two faces: run with no arguments for the GUI (Slint), with arguments for the CLI.

## GUI

Run the binary with no arguments:

```
cargo run --release          # from the repo
iron_renamer                 # or the built exe / after cargo install --path .
```

- Load files with **＋ Files** / **＋ From folder**, or rename folders themselves
  with **＋ Folders** (a batch is either all files or all folders, never mixed).
- Preview is live — every edit recomputes the table. Conflicts (duplicate targets,
  name already on disk, reserved Windows names, over-long paths) show per-row
  in red and are skipped on rename.
- **Rename N files** applies the batch; **Undo last batch** reverts it. Batches
  are recorded in a dated history, so undo works across restarts too.

## CLI

Preview by default; nothing is touched until `-x/--apply`.

```
iron_renamer [RULES] [OPTIONS] <files or globs>...
iron_renamer history         # list applied batches with dates and IDs
iron_renamer undo [ID]       # revert a batch (the latest if no ID)

RULES (applied in order given). Every rule flag takes suffix mods, e.g.
-r:ci:first — ':name' or ':ext' limits a rule to the stem or the extension
(default: the whole name).

  -r, --replace <OLD> <NEW>    literal replace; mods: ci, first|last|n<N>
  -e, --regex <PAT> <REPL>     regex replace ($1, $2 for groups)
  -c, --case <MODE>            lower | upper | title | first | invert
  -p, --pattern <PAT>          rebuild the name from a tag template
  -i, --insert <TEXT> <POS>    insert text (tags ok) at POS
      --remove <WHAT>          TEXT | re:PAT | pos:START,LEN | chars:LIST |
                               digits | upper | lower | diacritics
  -t, --trim <CHARS>           mods: start|end|both|all, inv ('' = whitespace)
      --renumber <NTH> <SPEC>  +N | -N (shift) or START[/STEP]; mod: pad<N>
      --move <PAT> <POS>       move first match (re:PAT for regex) to POS
      --swap <SEP>             swap around first separator: 'a - b' -> 'b - a'
      --names <FILE>           one new name per line, matched to list order
      --if <COND> <VALUE>      condition on the previous rule:
                               [not:]<name|new|ext|path>:<has|starts|ends|eq|re>

  POS:  start | end | N | -N | before:TEXT | after:TEXT | rbefore:PAT | rafter:PAT
  TAGS: <name> <ext> <num> <index> <parent>   (in pattern/insert/replacements)

OPTIONS:
  --start <N>                  counter start (default 1)
  --pad <N>                    zero-pad width (default: fits the largest number)
  -d, --dirs                   rename folders instead of files
  -x, --apply                  actually rename (otherwise preview only)
```

Examples:

```
iron_renamer -r " " "_" *.mp3
iron_renamer -e "(\d+)" "ep$1" *.mkv
iron_renamer -c:ext lower -p "trip_<num>.<ext>" --pad 3 *.jpg -x
iron_renamer -i:name "<parent>_" start --if ext:eq jpg *.* -x
iron_renamer --renumber 1 +100 --remove:name " copy" *.mkv
```

Globs (`*`, `?`) are expanded internally, so they work from PowerShell too.
Files are natural-sorted (`img9` before `img10`) before numbering.

Names are validated before anything is renamed (empty/invalid names, reserved
Windows names like `CON`, trailing dots/spaces, over-long paths); case-only
renames are allowed. Chains (`1→2, 2→3`) are ordered automatically and swap
cycles are broken with temporary names. A failed item leaves its file
untouched — re-run the same command to retry. Applied batches are appended to
`%LOCALAPPDATA%\iron_renamer\history.tsv` for `history`/`undo`.

## Build

```
cargo build --release        # -> target/release/iron_renamer.exe
cargo test
```

## Layout

| Path             | What                                        |
|------------------|---------------------------------------------|
| `src/engine.rs`  | Rule engine (rules, shared rule parsing, natural sort, globbing) + tests |
| `src/tags.rs`    | Tag parser shared by pattern/insert/replacement text + tests |
| `src/batch.rs`   | Shared planner/executor: validation, chain/swap-safe renaming, dated undo history + tests |
| `src/cli.rs`     | CLI front-end                                |
| `src/gui.rs`     | GUI state and callbacks                      |
| `ui/main.slint`  | All UI markup and styling                    |

## Not included (on purpose)

Metadata tags (EXIF/ID3), move/copy modes, drag-and-drop.
The rule engine is the extension point — a new `Rule` variant in `engine.rs`
shows up in both front-ends.
