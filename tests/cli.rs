// Integration tests: drive the real binary end to end. LOCALAPPDATA/HOME
// point at the test dir so batch history never touches the user's own.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("iron_cli_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_iron_renamer"))
        .args(args)
        .current_dir(dir)
        .env("LOCALAPPDATA", dir)
        .env("HOME", dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn preview_apply_history_undo() {
    let d = tmpdir("flow");
    fs::write(d.join("img 1.txt"), "one").unwrap();
    fs::write(d.join("img 2.txt"), "two").unwrap();

    // Preview (glob expanded by the binary, not a shell): nothing on disk moves.
    let (out, _, ok) = run(&d, &["-r", " ", "_", "*.txt"]);
    assert!(ok, "{out}");
    assert!(out.contains("preview only"), "{out}");
    assert!(out.contains("img_1.txt"), "{out}");
    assert!(d.join("img 1.txt").exists());

    // Apply renames both files and records the batch.
    let (out, _, ok) = run(&d, &["-r", " ", "_", "*.txt", "-x"]);
    assert!(ok, "{out}");
    assert_eq!(fs::read_to_string(d.join("img_1.txt")).unwrap(), "one");
    assert!(!d.join("img 1.txt").exists());

    let (out, _, ok) = run(&d, &["history"]);
    assert!(ok, "{out}");
    assert!(out.contains("ITEMS"), "{out}");

    // Undo restores the original names and empties the history.
    let (out, _, ok) = run(&d, &["undo"]);
    assert!(ok, "{out}");
    assert!(out.contains("reverted 2 item(s)"), "{out}");
    assert_eq!(fs::read_to_string(d.join("img 1.txt")).unwrap(), "one");
    let (out, _, ok) = run(&d, &["history"]);
    assert!(ok);
    assert!(out.contains("no batch history"), "{out}");
}

#[test]
fn errors_exit_nonzero() {
    let d = tmpdir("errors");
    fs::write(d.join("a.txt"), "").unwrap();
    fs::write(d.join("b.txt"), "").unwrap();

    // No rules given.
    let (_, err, ok) = run(&d, &["a.txt"]);
    assert!(!ok);
    assert!(err.contains("no rules"), "{err}");

    // No files matched.
    let (_, err, ok) = run(&d, &["-c", "upper", "nothing*.xyz"]);
    assert!(!ok);
    assert!(err.contains("no files matched"), "{err}");

    // Applying with unresolved conflicts renames nothing.
    let (_, err, ok) = run(&d, &["-p", "same.txt", "a.txt", "b.txt", "-x"]);
    assert!(!ok);
    assert!(err.contains("conflict"), "{err}");
    assert!(d.join("a.txt").exists() && d.join("b.txt").exists());

    // Undo with nothing recorded.
    let (_, err, ok) = run(&d, &["undo"]);
    assert!(!ok);
    assert!(err.contains("no batch history"), "{err}");
}

#[test]
fn preset_and_export() {
    let d = tmpdir("preset");
    fs::write(d.join("a.jpg"), "").unwrap();
    fs::write(d.join("b.jpg"), "").unwrap();
    fs::write(
        d.join("mine.preset"),
        "rule\tpattern\t\tpic_<num>.<ext>\t\nset\tstart\t5\n",
    )
    .unwrap();

    let (out, _, ok) = run(
        &d,
        &[
            "--preset",
            "mine.preset",
            "--export",
            "preview.csv",
            "*.jpg",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("pic_5.jpg"), "preset start honored: {out}");
    let csv = fs::read_to_string(d.join("preview.csv")).unwrap();
    assert!(csv.contains("pic_6.jpg"), "{csv}");
    // preview only: nothing renamed
    assert!(d.join("a.jpg").exists());
}

// ── mutation-testing gap fills (2026-08-30 baseline) ──

#[test]
fn input_sources_in_list_and_recursion() {
    let d = tmpdir("inputs");
    let sub = d.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(d.join("a.jpg"), "").unwrap();
    fs::write(d.join("b.png"), "").unwrap();
    fs::write(d.join("thumb.jpg"), "").unwrap();
    fs::write(sub.join("deep.jpg"), "").unwrap();

    // --in takes a whole folder, without recursing by default.
    let (out, _, ok) = run(&d, &["--in", ".", "-c", "upper"]);
    assert!(ok, "{out}");
    assert!(out.contains("A.JPG") && out.contains("B.PNG"), "{out}");
    assert!(!out.contains("DEEP.JPG"), "no recursion by default: {out}");

    // --recurse reaches the subfolder.
    let (out, _, ok) = run(&d, &["--in", ".", "--recurse", "-c", "upper"]);
    assert!(ok, "{out}");
    assert!(out.contains("DEEP.JPG"), "{out}");

    // --mask filters includes and excludes.
    let (out, _, ok) = run(&d, &["--in", ".", "--mask", "*.jpg;!thumb*", "-c", "upper"]);
    assert!(ok, "{out}");
    assert!(out.contains("A.JPG"), "{out}");
    assert!(!out.contains("B.PNG"), "excluded by mask: {out}");
    assert!(!out.contains("THUMB.JPG"), "excluded by !thumb*: {out}");

    // --list reads paths from a file, one per line.
    fs::write(d.join("files.txt"), "a.jpg\nb.png\n").unwrap();
    let (out, _, ok) = run(&d, &["--list", "files.txt", "-c", "upper"]);
    assert!(ok, "{out}");
    assert!(out.contains("A.JPG") && out.contains("B.PNG"), "{out}");
}

#[test]
fn sort_order_drives_the_counter() {
    let d = tmpdir("sorting");
    // Sizes deliberately disagree with names so the two orders differ.
    fs::write(d.join("a.txt"), "xxxxx").unwrap();
    fs::write(d.join("b.txt"), "x").unwrap();
    fs::write(d.join("c.txt"), "xxx").unwrap();

    let numbered = |args: &[&str]| -> Vec<String> {
        let (out, err, ok) = run(&d, args);
        assert!(ok, "{err}");
        out.lines()
            .filter(|l| l.contains("->"))
            .map(|l| l.trim().to_string())
            .collect()
    };

    let by_name = numbered(&["-p", "<num>.<ext>", "--sort", "name", "*.txt"]);
    assert!(by_name[0].starts_with("a.txt"), "{by_name:?}");
    assert!(by_name[0].ends_with("1.txt"), "{by_name:?}");

    let by_size = numbered(&["-p", "<num>.<ext>", "--sort", "size", "*.txt"]);
    assert!(
        by_size[0].starts_with("b.txt"),
        "smallest first: {by_size:?}"
    );

    // --desc reverses whatever the sort produced.
    let desc = numbered(&["-p", "<num>.<ext>", "--sort", "name", "--desc", "*.txt"]);
    assert!(desc[0].starts_with("c.txt"), "{desc:?}");
}

#[test]
fn collision_policies_from_the_command_line() {
    let d = tmpdir("collide");
    fs::write(d.join("a.txt"), "").unwrap();
    fs::write(d.join("b.txt"), "").unwrap();

    // Default policy refuses.
    let (_, err, ok) = run(&d, &["-p", "same.<ext>", "*.txt", "-x"]);
    assert!(!ok, "{err}");

    // number and letter each resolve it, differently.
    let (out, _, ok) = run(&d, &["-p", "same.<ext>", "--collide", "number", "*.txt"]);
    assert!(ok, "{out}");
    assert!(out.contains("same (2).txt"), "{out}");

    let (out, _, ok) = run(&d, &["-p", "same.<ext>", "--collide", "letter", "*.txt"]);
    assert!(ok, "{out}");
    assert!(out.contains("same_b.txt"), "{out}");

    // Anything else is treated as a tag pattern for the suffix. (A value
    // starting with '-' would be parsed as a flag, so it needs a leading char.)
    let (out, _, ok) = run(&d, &["-p", "same.<ext>", "--collide", "_v<index>", "*.txt"]);
    assert!(ok, "{out}");
    assert!(out.contains("same_v2.txt"), "{out}");
}

#[test]
fn copy_and_move_write_to_a_destination_folder() {
    let d = tmpdir("dest");
    fs::write(d.join("a.txt"), "one").unwrap();

    // Copy leaves the source in place.
    let (out, err, ok) = run(&d, &["--copy-to", "out", "-c", "upper", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    assert_eq!(
        fs::read_to_string(d.join("out").join("A.TXT")).unwrap(),
        "one"
    );
    assert!(d.join("a.txt").exists(), "copy kept the source");

    // Move does not.
    let (out, err, ok) = run(&d, &["--move-to", "gone", "-c", "upper", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    assert_eq!(
        fs::read_to_string(d.join("gone").join("A.TXT")).unwrap(),
        "one"
    );
    assert!(!d.join("a.txt").exists(), "move left the source behind");
}

#[test]
fn folder_mode_renames_directories_not_files() {
    let d = tmpdir("dirs");
    fs::create_dir_all(d.join("one")).unwrap();
    fs::create_dir_all(d.join("two")).unwrap();
    fs::write(d.join("loose.txt"), "").unwrap();

    let (out, err, ok) = run(&d, &["-d", "-c", "upper", "*", "-x"]);
    assert!(ok, "{err}{out}");
    assert!(d.join("ONE").exists() && d.join("TWO").exists(), "{out}");
    // The file was never a candidate. Path::exists is case-insensitive on
    // Windows, so compare the spelling the directory actually reports.
    let names: Vec<String> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"loose.txt".to_string()), "{names:?}");
}

#[test]
fn conditions_gate_the_rule_before_them() {
    let d = tmpdir("cond");
    fs::write(d.join("keep.jpg"), "").unwrap();
    fs::write(d.join("skip.txt"), "").unwrap();

    let (out, err, ok) = run(&d, &["-c", "upper", "--if", "ext:eq", "jpg", "*.*"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("KEEP.JPG"), "{out}");
    assert!(!out.contains("SKIP.TXT"), "condition did not gate: {out}");
}

#[test]
fn name_and_pair_lists_come_from_files() {
    let d = tmpdir("lists");
    fs::write(d.join("a.txt"), "").unwrap();
    fs::write(d.join("b.txt"), "").unwrap();

    // --names supplies one new name per line, in list order.
    fs::write(d.join("names.txt"), "first.txt\nsecond.txt\n").unwrap();
    let (out, err, ok) = run(&d, &["--names", "names.txt", "a.txt", "b.txt"]);
    assert!(ok, "{err}{out}");
    assert!(
        out.contains("first.txt") && out.contains("second.txt"),
        "{out}"
    );

    // --replace-list applies OLD=NEW pairs.
    fs::write(d.join("pairs.txt"), "a=alpha\nb=bravo\n").unwrap();
    let (out, err, ok) = run(&d, &["--replace-list", "pairs.txt", "a.txt", "b.txt"]);
    assert!(ok, "{err}{out}");
    assert!(
        out.contains("alpha.txt") && out.contains("bravo.txt"),
        "{out}"
    );
}

#[test]
fn javascript_rules_run_sandboxed() {
    let d = tmpdir("js");
    fs::write(d.join("a b.txt"), "").unwrap();
    // `name` is the whole file name, `stem` the part before the extension.
    let (out, err, ok) = run(&d, &["--js", "stem.toUpperCase() + \".\" + ext", "a b.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("A B.txt"), "{out}");

    // No file or network access is exposed: the usual escape hatches are not
    // merely blocked, they are absent, so a script cannot reach the host.
    let (out, err, ok) = run(
        &d,
        &[
            "--js",
            "typeof require + '_' + typeof process + '_' + typeof fetch",
            "a b.txt",
        ],
    );
    assert!(ok, "{err}{out}");
    assert!(out.contains("undefined_undefined_undefined"), "{out}");
}

#[test]
fn csv_rows_feed_the_csv_tag() {
    let d = tmpdir("csv");
    fs::write(d.join("a.txt"), "").unwrap();
    fs::write(d.join("b.txt"), "").unwrap();
    fs::write(d.join("rows.csv"), "id,who\n1,alice\n2,bob\n").unwrap();

    let (out, err, ok) = run(
        &d,
        &[
            "--csv",
            "rows.csv",
            "-p",
            "<csv:who>.<ext>",
            "a.txt",
            "b.txt",
        ],
    );
    assert!(ok, "{err}{out}");
    assert!(
        out.contains("alice.txt") && out.contains("bob.txt"),
        "{out}"
    );
}

#[test]
fn timestamps_and_metadata_are_written_after_the_batch() {
    let d = tmpdir("meta");
    fs::write(d.join("song.wav"), "").unwrap();
    fs::write(d.join("a.txt"), "").unwrap();

    // --touch sets an absolute date, readable straight back as a tag.
    let (out, err, ok) = run(
        &d,
        &[
            "--touch",
            "modified=2001-05-06 07:08",
            "-c",
            "lower",
            "a.txt",
            "-x",
        ],
    );
    assert!(ok, "{err}{out}");
    let (out, err, ok) = run(&d, &["-p", "<modified:yyyy-MM-dd>.<ext>", "a.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("2001-05-06.txt"), "{out}");

    // An unknown timestamp field is rejected at parse time.
    let (_, err, ok) = run(&d, &["--touch", "invented=2001-05-06 07:08", "a.txt", "-x"]);
    assert!(!ok, "{err}");

    // A --set-meta failure is reported on stderr. Note it does NOT change the
    // exit code: the rename itself succeeded, and only the tag write failed.
    let (_, err, ok) = run(
        &d,
        &["--set-meta", "bogus=1", "-c", "lower", "song.wav", "-x"],
    );
    assert!(err.contains("METADATA FAILED"), "{err}");
    assert!(ok, "the batch itself succeeded");
    assert!(d.join("song.wav").exists());
}

#[test]
fn undo_targets_a_specific_batch_by_id() {
    let d = tmpdir("undoid");
    fs::write(d.join("a.txt"), "one").unwrap();
    fs::write(d.join("b.txt"), "two").unwrap();

    let (out, err, ok) = run(&d, &["-p", "first.<ext>", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    let (out, err, ok) = run(&d, &["-p", "second.<ext>", "b.txt", "-x"]);
    assert!(ok, "{err}{out}");

    // Two batches recorded; the ids are the first column of history.
    let (hist, _, ok) = run(&d, &["history"]);
    assert!(ok, "{hist}");
    let ids: Vec<&str> = hist
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| w.chars().all(|c| c.is_ascii_digit()) && !w.is_empty())
        .collect();
    assert_eq!(ids.len(), 2, "{hist}");

    // Undoing the older batch by id leaves the newer one applied.
    let (out, err, ok) = run(&d, &["undo", ids[1]]);
    assert!(ok, "{err}{out}");
    assert!(d.join("a.txt").exists(), "older batch reverted: {out}");
    assert!(d.join("second.txt").exists(), "newer batch left alone");
}

#[test]
fn a_preset_can_carry_mode_destination_and_collision_policy() {
    let d = tmpdir("presetmode");
    fs::write(d.join("a.txt"), "one").unwrap();
    fs::write(d.join("b.txt"), "two").unwrap();
    fs::write(
        d.join("copy.preset"),
        "rule\tpattern\t\tsame.<ext>\t\nset\tmode\tcopy\nset\tdest\tout\nset\tcollide\tnumber\n",
    )
    .unwrap();

    let (out, err, ok) = run(&d, &["--preset", "copy.preset", "*.txt", "-x"]);
    assert!(ok, "{err}{out}");
    // mode=copy leaves the sources where they are...
    assert!(d.join("a.txt").exists() && d.join("b.txt").exists());
    // ...dest sends them to a folder, and collide=number resolves the clash.
    assert_eq!(
        fs::read_to_string(d.join("out").join("same.txt")).unwrap(),
        "one"
    );
    assert_eq!(
        fs::read_to_string(d.join("out").join("same (2).txt")).unwrap(),
        "two"
    );

    // mode=move is the other arm of the same setting.
    fs::write(
        d.join("move.preset"),
        "rule\tpattern\t\tmoved.<ext>\t\nset\tmode\tmove\nset\tdest\tgone\n",
    )
    .unwrap();
    let (out, err, ok) = run(&d, &["--preset", "move.preset", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    assert!(d.join("gone").join("moved.txt").exists(), "{out}");
    assert!(!d.join("a.txt").exists(), "move left the source behind");
}

#[test]
fn automatic_pad_width_covers_start_through_the_last_index() {
    let d = tmpdir("padwidth");
    for i in 1..=9 {
        fs::write(d.join(format!("f{i}.txt")), "").unwrap();
    }
    // 1 + 9 - 1 = 9: one digit, so no padding at all.
    let (out, err, ok) = run(&d, &["-p", "<num>.<ext>", "*.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("  ->  1.txt"), "{out}");

    // Starting at 2 pushes the last index to 10 and widens every name.
    let (out, err, ok) = run(&d, &["-p", "<num>.<ext>", "--start", "2", "*.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("  ->  02.txt"), "{out}");

    // An explicit --pad wins over the computed width.
    let (out, err, ok) = run(&d, &["-p", "<num>.<ext>", "--pad", "4", "*.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("  ->  0001.txt"), "{out}");
}

#[test]
fn preview_shows_bare_names_for_renames_and_paths_for_copies() {
    let d = tmpdir("shown");
    fs::write(d.join("a.txt"), "").unwrap();

    // A plain rename has nowhere to go, so only the new name is shown.
    let (out, err, ok) = run(&d, &["-c", "upper", "a.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("  ->  A.TXT"), "{out}");

    // A copy names the destination, so the preview has to show the path.
    let (out, err, ok) = run(&d, &["--copy-to", "out", "-c", "upper", "a.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("out"), "{out}");
    assert!(out.contains("A.TXT"), "{out}");
}

#[test]
fn summary_lines_report_conflicts_and_the_undo_hint() {
    let d = tmpdir("summary");
    fs::write(d.join("a.txt"), "").unwrap();
    fs::write(d.join("b.txt"), "").unwrap();

    // A clash is flagged per row in the preview and excluded from the count,
    // rather than being silently dropped or silently applied.
    let (out, _, ok) = run(&d, &["-p", "same.<ext>", "*.txt"]);
    assert!(ok, "{out}");
    assert!(out.contains("duplicate target"), "{out}");
    assert!(
        out.contains("rename 1 item(s)"),
        "only the clean row counts: {out}"
    );

    // A successful rename reports how many moved and how to undo it.
    let (out, err, ok) = run(&d, &["-c", "upper", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("of 1 item(s)"), "{out}");
    assert!(out.contains("undo"), "rename should offer undo: {out}");

    // A copy creates nothing to undo, so it must not offer it.
    let (out, err, ok) = run(&d, &["--copy-to", "out", "-c", "lower", "b.txt", "-x"]);
    assert!(ok, "{err}{out}");
    assert!(!out.contains("undo"), "copy should not offer undo: {out}");
}

#[test]
fn metadata_only_runs_need_no_rules() {
    let d = tmpdir("metaonly");
    fs::write(d.join("a.txt"), "").unwrap();
    // --touch alone is enough work to justify the batch: no "no rules" error.
    let (out, err, ok) = run(&d, &["--touch", "modified=2001-05-06 07:08", "a.txt", "-x"]);
    assert!(ok, "{err}{out}");
    let (out, err, ok) = run(&d, &["-p", "<modified:yyyy>.<ext>", "a.txt"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("2001.txt"), "{out}");
}

#[test]
fn a_run_that_changes_nothing_prints_no_summary() {
    let d = tmpdir("noop");
    fs::write(d.join("ALREADY.TXT"), "").unwrap();
    // Upper-casing an already-upper-case name plans zero renames.
    let (out, err, ok) = run(&d, &["-c", "upper", "ALREADY.TXT", "-x"]);
    assert!(ok, "{err}{out}");
    assert!(out.contains("nothing to rename"), "{out}");
    // None of the after-the-fact lines belong on a batch that did nothing.
    assert!(!out.contains("item(s)\n"), "summary line printed: {out}");
    assert!(!out.contains("undo"), "undo hint printed: {out}");
    assert!(!out.contains("conflict"), "conflict line printed: {out}");
    // And nothing was recorded to undo.
    let (hist, _, _) = run(&d, &["history"]);
    assert!(hist.contains("no batch history"), "{hist}");
}
