// End-to-end tests against real temporary directories: the full
// preview -> apply -> undo flow, awkward names through the rule engine,
// and forced mid-batch failures in every mode.

use crate::batch::{self, BatchCfg, Collision, Mode};
use crate::engine::{Ctx, RuleEntry, apply_entry, build_rule};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("iron_renamer_e2e_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn rules(specs: &[(&str, &[&str], &str, &str)]) -> Vec<RuleEntry> {
    specs
        .iter()
        .map(|(kind, mods, a, b)| {
            let (rule, part) = build_rule(kind, mods, a, b).unwrap();
            RuleEntry { rule, part, cond: None }
        })
        .collect()
}

/// Preview a chain rename, hit a collision, resolve it with a policy,
/// apply, and undo — the whole life of a batch.
#[test]
fn full_flow_preview_chain_collision_undo() {
    let d = tmpdir("full");
    put(&d, "img1.jpg", "one");
    put(&d, "img2.jpg", "two");
    put(&d, "img3.jpg", "three");
    let files = vec![d.join("img1.jpg"), d.join("img2.jpg"), d.join("img3.jpg")];
    let shift = rules(&[("renumber", &[], "1", "+1")]); // img1->img2->img3->img4: a chain
    let none = HashMap::new();
    let cfg = BatchCfg {
        rules: &shift,
        start: 1,
        pad: 1,
        overrides: &none,
        mode: Mode::Rename,
        dest: "",
        collision: Collision::Fail,
    };

    // Preview: every target is vacated by the batch itself, so no conflicts.
    let items = batch::plan(&files, &cfg);
    let names: Vec<&str> = items.iter().map(|i| i.new_name.as_str()).collect();
    assert_eq!(names, vec!["img2.jpg", "img3.jpg", "img4.jpg"]);
    assert!(items.iter().all(|i| i.changed && i.issue.is_none()));

    // Collision: an unrelated img4.jpg blocks the end of the chain...
    put(&d, "img4.jpg", "blocker");
    let items2 = batch::plan(&files, &cfg);
    assert_eq!(items2[2].issue.as_deref(), Some("target exists"));
    // ...and the Number policy resolves it in the preview.
    let cfg_num = BatchCfg { collision: Collision::Number, ..cfg };
    let items3 = batch::plan(&files, &cfg_num);
    assert_eq!(items3[2].new_name, "img4 (2).jpg");
    assert!(items3.iter().all(|i| i.issue.is_none()));
    fs::remove_file(d.join("img4.jpg")).unwrap();

    // Apply: the executor orders the chain (img3 must move before img2...).
    let items = batch::plan(&files, &cfg);
    let ops = items.iter().filter(|i| i.changed && i.issue.is_none()).map(|i| i.op()).collect();
    let res = batch::execute(ops, Mode::Rename);
    assert_eq!((res.renamed.len(), res.failed.len()), (3, 0));
    assert_eq!(read(&d.join("img2.jpg")), "one");
    assert_eq!(read(&d.join("img3.jpg")), "two");
    assert_eq!(read(&d.join("img4.jpg")), "three");
    assert!(!d.join("img1.jpg").exists());

    // Undo restores the original names, chain and all.
    let hist = d.join("history.tsv");
    batch::record_at(&hist, &res.renamed).unwrap();
    let (reverted, errors) = batch::undo_at(&hist, None).unwrap();
    assert_eq!((reverted.len(), errors.len()), (3, 0));
    assert_eq!(read(&d.join("img1.jpg")), "one");
    assert_eq!(read(&d.join("img2.jpg")), "two");
    assert_eq!(read(&d.join("img3.jpg")), "three");
}

/// Table-driven rules over Unicode names, dotfiles, multiple extensions,
/// and extensionless files.
#[test]
fn rules_on_awkward_names() {
    let cases: &[(&str, &[&str], &str, &str, &str, &str)] = &[
        // (kind, mods, a, b, input, expected)
        ("replace", &[], "é", "e", "café.txt", "cafe.txt"),
        ("replace", &["ci"], "CAFÉ", "x", "café.txt", "x.txt"),
        ("case", &[], "upper", "", "grüße.txt", "GRÜSSE.TXT"),
        ("case", &["name"], "title", "", "мой файл.txt", "Мой Файл.txt"),
        ("case", &[], "invert", "", "aBc🎉.TXT", "AbC🎉.txt"),
        // dotfiles are all stem, no extension
        ("pattern", &[], "<name>_x", "", ".gitignore", ".gitignore_x"),
        ("case", &["ext"], "upper", "", ".gitignore", ".gitignore"),
        // only the last extension is the extension
        ("insert", &["name"], "_v2", "end", "archive.tar.gz", "archive.tar_v2.gz"),
        ("case", &["ext"], "upper", "", "archive.tar.gz", "archive.tar.GZ"),
        // extensionless: ext rules build one, name rules take the whole name
        ("insert", &["ext"], "bak", "end", "README", "README.bak"),
        ("case", &["name"], "lower", "", "README", "readme"),
        ("remove", &[], "diacritics", "", "žluťoučký.txt", "zlutoucky.txt"),
        ("trim", &["name"], "", "", " spaced .txt", "spaced.txt"),
        ("insert", &["name"], "✨", "start", "🎉party.txt", "✨🎉party.txt"),
        ("remove", &["name"], "pos:0,1", "", "🎉party.txt", "party.txt"),
    ];
    for (kind, mods, a, b, input, expected) in cases {
        let (rule, part) = build_rule(kind, mods, a, b).unwrap();
        let e = RuleEntry { rule, part, cond: None };
        let path = PathBuf::from(input);
        let ctx = Ctx { index: 0, num: 1, pad: 1, folder_num: 1, path: &path, original: input };
        assert_eq!(
            apply_entry(&e, input, &ctx),
            *expected,
            "{kind} {mods:?} {a:?} {b:?} on {input:?}"
        );
    }
}

/// Case-insensitive collision detection: two names differing only in case
/// count as duplicates, like NTFS treats them.
#[test]
fn case_insensitive_duplicate_detection() {
    let d = tmpdir("caseins");
    put(&d, "x.txt", "");
    put(&d, "y.txt", "");
    let files = vec![d.join("x.txt"), d.join("y.txt")];
    let none = HashMap::new();
    // x -> same.txt, y -> SAME.txt: same file on a case-insensitive volume.
    let two = rules(&[("replace", &[], "x.txt", "same.txt"), ("replace", &[], "y.txt", "SAME.txt")]);
    let cfg = BatchCfg {
        rules: &two,
        start: 1,
        pad: 1,
        overrides: &none,
        mode: Mode::Rename,
        dest: "",
        collision: Collision::Fail,
    };
    let items = batch::plan(&files, &cfg);
    assert!(items[0].issue.is_none());
    assert_eq!(items[1].issue.as_deref(), Some("duplicate target"));
}

/// Force a failure halfway through a batch in every mode: the failed item's
/// file stays untouched and the same op succeeds on retry.
#[test]
fn mid_batch_failure_recovery_all_modes() {
    for (label, mode) in [("rename", Mode::Rename), ("copy", Mode::Copy), ("move", Mode::Move)] {
        let d = tmpdir(&format!("recover_{label}"));
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        let out = d.join("out");
        let blocked = out.join("a.txt");
        fs::create_dir_all(&out).unwrap();
        put(&out, "a.txt", "blocker");

        let ops = vec![
            batch::Op { from: a.clone(), to: blocked.clone() },
            batch::Op { from: b.clone(), to: out.join("b.txt") },
        ];
        let res = batch::execute(ops, mode);
        assert_eq!((res.renamed.len(), res.failed.len()), (1, 1), "{label}");
        assert_eq!(read(&a), "A", "{label}: failed op leaves its file untouched");
        assert_eq!(read(&blocked), "blocker", "{label}: never overwrites");
        assert_eq!(read(&out.join("b.txt")), "B", "{label}");
        if mode == Mode::Copy {
            assert_eq!(read(&b), "B", "copy keeps the source");
        } else {
            assert!(!b.exists(), "{label} removes the source");
        }

        // Retry the failed op after clearing the blocker.
        fs::remove_file(&blocked).unwrap();
        let retry: Vec<batch::Op> = res.failed.into_iter().map(|(op, _)| op).collect();
        let res2 = batch::execute(retry, mode);
        assert_eq!((res2.renamed.len(), res2.failed.len()), (1, 0), "{label} retry");
        assert_eq!(read(&blocked), "A", "{label} retry lands");
    }
}
