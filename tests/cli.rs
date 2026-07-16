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
