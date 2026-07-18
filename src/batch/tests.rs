use super::*;
use crate::engine::build_rule;
use std::time::UNIX_EPOCH;

fn rules(specs: &[(&str, &str, &str)]) -> Vec<RuleEntry> {
    specs
        .iter()
        .map(|(kind, a, b)| {
            let (rule, part) = build_rule(kind, &[], a, b).unwrap();
            RuleEntry {
                rule,
                part,
                cond: None,
            }
        })
        .collect()
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("iron_renamer_test_{name}_{}", std::process::id()));
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

// Perf guard for execute()'s ready queue and finals()'s lookup map, both
// O(n²) before 0.3.1. Run: cargo test --release -- --ignored bench --nocapture
#[test]
#[ignore]
fn bench_chain_execute_and_finals() {
    for n in [5_000usize, 10_000, 20_000] {
        let d = tmpdir(&format!("bench{n}"));
        let files: Vec<PathBuf> = (0..=n)
            .map(|i| d.join(format!("file_{i:06}.txt")))
            .collect();
        for f in &files[..n] {
            fs::write(f, "x").unwrap();
        }
        // Shift-by-one chain, given in fully blocked order (worst case).
        let ops: Vec<Op> = (0..n)
            .map(|i| Op {
                from: files[i].clone(),
                to: files[i + 1].clone(),
            })
            .collect();
        let t = std::time::Instant::now();
        let res = execute(ops, Mode::Rename);
        let exec_t = t.elapsed();
        assert!(res.failed.is_empty());
        assert_eq!(res.renamed.len(), n);

        let items: Vec<PlanItem> = (0..n)
            .map(|i| PlanItem {
                from: files[i].clone(),
                new_name: String::new(),
                target: files[i + 1].clone(),
                changed: true,
                issue: None,
            })
            .collect();
        let t = std::time::Instant::now();
        let fin = finals(&items, &res);
        let finals_t = t.elapsed();
        assert_eq!(fin[0], files[1]);

        println!("n={n}: execute {exec_t:?}, finals {finals_t:?}");
        let _ = fs::remove_dir_all(&d);
    }
}

// Perf guard for the GUI/CLI preview planner.
// Run: cargo test --release -- --ignored bench_plan_preview --nocapture
#[test]
#[ignore]
fn bench_plan_preview() {
    const N: usize = 10_000;
    let d = tmpdir("bench_plan");
    let files: Vec<PathBuf> = (0..N)
        .map(|i| put(&d, &format!("file_{i:06}.txt"), "x"))
        .collect();
    let rules = rules(&[("replace", "file", "photo")]);
    let none = HashMap::new();
    let cfg = BatchCfg::rename(&rules, 1, 1, &none);
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let items = plan(&files, &cfg);
        assert_eq!(items.len(), N);
        assert!(items.iter().all(|item| item.issue.is_none()));
        println!("plan {N}: {:?}", start.elapsed());
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn export_rows_formats_all_outputs() {
    let d = tmpdir("export");
    let rows = vec![(vec!["a,b".into(), "x".into()], "a,b -> x".into())];
    for (ext, expected) in [
        ("csv", "from,to\n\"a,b\",x\n"),
        ("json", "[\n  {\"from\": \"a,b\", \"to\": \"x\"}\n]\n"),
        ("txt", "a,b -> x\n"),
    ] {
        let path = d.join(format!("out.{ext}"));
        export_rows(&path, &["from", "to"], rows.clone()).unwrap();
        assert_eq!(read(&path), expected);
    }
}

#[test]
fn validates_names() {
    assert!(name_issue("ok.txt").is_none());
    assert!(name_issue("common.txt").is_none()); // COM without a digit is fine
    assert!(name_issue("").is_some());
    assert!(name_issue("a<b.txt").is_some());
    assert!(name_issue("a\tb.txt").is_some());
    assert!(name_issue("CON.txt").is_some());
    assert!(name_issue("com3").is_some());
    assert!(name_issue("trailing.").is_some());
    assert!(name_issue("trailing ").is_some());
}

#[test]
fn swap_and_chain() {
    let d = tmpdir("swap");
    let a = put(&d, "a.txt", "A");
    let b = put(&d, "b.txt", "B");
    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: b.clone(),
            },
            Op {
                from: b.clone(),
                to: a.clone(),
            },
        ],
        Mode::Rename,
    );
    assert_eq!(res.renamed.len(), 2);
    assert!(res.failed.is_empty());
    assert_eq!(read(&a), "B");
    assert_eq!(read(&b), "A");
    assert_eq!(
        fs::read_dir(&d).unwrap().count(),
        2,
        "no temp files left behind"
    );

    let d = tmpdir("chain");
    let one = put(&d, "1.txt", "one");
    let two = put(&d, "2.txt", "two");
    let three = d.join("3.txt");
    let res = execute(
        vec![
            Op {
                from: one.clone(),
                to: two.clone(),
            },
            Op {
                from: two.clone(),
                to: three.clone(),
            },
        ],
        Mode::Rename,
    );
    assert!(res.failed.is_empty());
    assert_eq!(read(&two), "one");
    assert_eq!(read(&three), "two");
    assert!(!one.exists());
}

#[test]
fn partial_failure_preserves_files_for_retry() {
    let d = tmpdir("partial");
    let a = put(&d, "a.txt", "A");
    let b = put(&d, "b.txt", "B");
    let blocker = put(&d, "taken.txt", "X");
    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: d.join("taken.txt"),
            },
            Op {
                from: b.clone(),
                to: d.join("free.txt"),
            },
        ],
        Mode::Rename,
    );
    assert_eq!(res.renamed.len(), 1);
    assert_eq!(res.failed.len(), 1);
    assert_eq!(read(&a), "A", "failed op leaves its file untouched");
    assert_eq!(read(&blocker), "X", "existing file never overwritten");
    assert_eq!(read(&d.join("free.txt")), "B");
}

#[test]
fn copy_and_move_modes() {
    let d = tmpdir("copymove");
    let a = put(&d, "a.txt", "A");
    let sub = d.join("out").join("deep");

    // Copy into a subfolder that does not exist yet.
    let res = execute(
        vec![Op {
            from: a.clone(),
            to: sub.join("a.txt"),
        }],
        Mode::Copy,
    );
    assert!(res.failed.is_empty());
    assert_eq!(read(&a), "A", "copy keeps the source");
    assert_eq!(read(&sub.join("a.txt")), "A");

    // Copy refuses to overwrite.
    let res = execute(
        vec![Op {
            from: a.clone(),
            to: sub.join("a.txt"),
        }],
        Mode::Copy,
    );
    assert_eq!(res.failed.len(), 1);

    // Move creates directories and removes the source.
    let b = put(&d, "b.txt", "B");
    let res = execute(
        vec![Op {
            from: b.clone(),
            to: sub.join("b.txt"),
        }],
        Mode::Move,
    );
    assert!(res.failed.is_empty());
    assert!(!b.exists());
    assert_eq!(read(&sub.join("b.txt")), "B");

    // Folder copies recurse; the same copy/remove path backs cross-volume moves.
    let tree = d.join("tree");
    fs::create_dir_all(tree.join("nested")).unwrap();
    put(&tree.join("nested"), "c.txt", "C");
    let copied = d.join("copied-tree");
    let res = execute(
        vec![Op {
            from: tree.clone(),
            to: copied.clone(),
        }],
        Mode::Copy,
    );
    assert!(res.failed.is_empty());
    assert_eq!(read(&tree.join("nested/c.txt")), "C");
    assert_eq!(read(&copied.join("nested/c.txt")), "C");

    let moved = d.join("moved-tree");
    let res = execute(
        vec![Op {
            from: copied.clone(),
            to: moved.clone(),
        }],
        Mode::Move,
    );
    assert!(res.failed.is_empty());
    assert!(!copied.exists());
    assert_eq!(read(&moved.join("nested/c.txt")), "C");

    let inside = tree.join("inside/tree");
    let res = execute(
        vec![Op {
            from: tree.clone(),
            to: inside.clone(),
        }],
        Mode::Copy,
    );
    assert_eq!(res.failed.len(), 1);
    assert!(!inside.exists());
}

#[test]
fn plan_flags_conflicts_and_allows_case_only() {
    let d = tmpdir("plan");
    put(&d, "img1.jpg", "");
    put(&d, "img2.jpg", "");
    put(&d, "other.jpg", "");
    let files = vec![d.join("img1.jpg"), d.join("img2.jpg")];
    let none = HashMap::new();

    let case_rule = rules(&[("replace", "img", "IMG")]);
    let items = plan(&files, &BatchCfg::rename(&case_rule, 1, 1, &none));
    assert!(
        items.iter().all(|i| i.changed && i.issue.is_none()),
        "case-only renames are valid"
    );

    let dup_rule = rules(&[("pattern", "same.jpg", "")]);
    let items = plan(&files, &BatchCfg::rename(&dup_rule, 1, 1, &none));
    assert_eq!(items[1].issue.as_deref(), Some("duplicate target"));

    let clash_rule = rules(&[("replace", "img1", "other")]);
    let items = plan(&files, &BatchCfg::rename(&clash_rule, 1, 1, &none));
    assert_eq!(items[0].issue.as_deref(), Some("target exists"));
    assert!(items[1].issue.is_none());

    // Swap inside one batch is not a conflict: each target is vacated.
    let swap_rule = rules(&[
        ("replace", "img1", "tmpX"),
        ("replace", "img2", "img1"),
        ("replace", "tmpX", "img2"),
    ]);
    let items = plan(&files, &BatchCfg::rename(&swap_rule, 1, 1, &none));
    assert!(items.iter().all(|i| i.changed && i.issue.is_none()));

    // A manual override wins over rules but is validated like any name.
    let over: HashMap<PathBuf, String> = [(files[0].clone(), "manual.jpg".to_string())].into();
    let cfg = BatchCfg {
        overrides: &over,
        ..BatchCfg::rename(&case_rule, 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "manual.jpg");
    assert!(items[0].issue.is_none());
}

#[test]
fn collision_policies_resolve_in_preview() {
    let d = tmpdir("collide");
    put(&d, "a.jpg", "");
    put(&d, "b.jpg", "");
    put(&d, "same.jpg", "");
    let files = vec![d.join("a.jpg"), d.join("b.jpg")];
    let none = HashMap::new();
    let dup_rule = rules(&[("pattern", "same.jpg", "")]);

    let cfg = BatchCfg {
        collision: Collision::Number,
        ..BatchCfg::rename(&dup_rule, 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same (2).jpg", "disk collision numbered");
    assert_eq!(
        items[1].new_name, "same (3).jpg",
        "batch duplicate numbered"
    );
    assert!(items.iter().all(|i| i.issue.is_none()));

    let cfg = BatchCfg {
        collision: Collision::Letter,
        ..BatchCfg::rename(&dup_rule, 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same_b.jpg");
    assert_eq!(items[1].new_name, "same_c.jpg");

    let cfg = BatchCfg {
        collision: Collision::Pattern("_<index>".into()),
        ..BatchCfg::rename(&dup_rule, 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same_1.jpg");
    assert_eq!(items[1].new_name, "same_2.jpg");
}

#[test]
fn plan_copy_move_destinations() {
    let d = tmpdir("dest");
    put(&d, "a.jpg", "");
    put(&d, "b.txt", "");
    let files = vec![d.join("a.jpg"), d.join("b.txt")];
    let none = HashMap::new();

    // Tag-expanded relative destination: sorted/<ext>.
    let cfg = BatchCfg {
        mode: Mode::Copy,
        dest: "sorted/<ext>",
        ..BatchCfg::rename(&[], 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert!(items.iter().all(|i| i.changed && i.issue.is_none()));
    assert_eq!(items[0].target, d.join("sorted").join("jpg").join("a.jpg"));
    assert_eq!(items[1].target, d.join("sorted").join("txt").join("b.txt"));

    // Copy onto itself (empty dest, no rules) is a no-op, not a conflict.
    let cfg = BatchCfg {
        mode: Mode::Copy,
        ..BatchCfg::rename(&[], 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert!(items.iter().all(|i| !i.changed));
}

#[test]
fn file_pairs_share_the_generated_stem() {
    let d = tmpdir("pairs");
    put(&d, "img1.jpg", "");
    put(&d, "img1.xmp", "");
    put(&d, "img2.jpg", "");
    let files = vec![d.join("img1.jpg"), d.join("img1.xmp"), d.join("img2.jpg")];
    let none = HashMap::new();
    let pat = rules(&[("pattern", "pic_<num>.<ext>", "")]);
    let cfg = BatchCfg {
        pairs: true,
        ..BatchCfg::rename(&pat, 1, 1, &none)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "pic_1.jpg");
    assert_eq!(
        items[1].new_name, "pic_1.xmp",
        "sidecar adopts the pair's stem"
    );
    assert_eq!(
        items[2].new_name, "pic_3.jpg",
        "counters still count every row"
    );
    assert!(items.iter().all(|i| i.issue.is_none()));
    // Without pairs the sidecar gets its own counter value.
    let items = plan(&files, &BatchCfg::rename(&pat, 1, 1, &none));
    assert_eq!(items[1].new_name, "pic_2.xmp");
}

#[test]
fn touch_parses_and_sets_times() {
    assert!(parse_touch("no-equals").is_err());
    assert!(parse_touch("bogus=+1h").is_err());
    assert!(parse_touch("modified=junk").is_err());
    let spec = parse_touch("created,accessed=+1h");
    if cfg!(any(windows, target_os = "macos")) {
        let spec = spec.unwrap();
        assert!(spec.created && spec.accessed && !spec.modified);
    } else {
        assert!(spec.is_err());
    }

    let secs_of = |p: &Path| {
        fs::metadata(p)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    };
    let d = tmpdir("touch");
    let f = put(&d, "IMG_20240501_1230.jpg", "x");

    // Absolute (UTC).
    let spec = parse_touch("modified=2024-05-01 10:30").unwrap();
    let (n, errs) = apply_touch(std::slice::from_ref(&f), &spec);
    assert_eq!((n, errs.len()), (1, 0));
    let expected = crate::tags::epoch_from_civil(2024, 5, 1, 10, 30, 0);
    assert_eq!(secs_of(&f), expected);

    // Delta shifts the current value.
    let spec = parse_touch("modified=+1h").unwrap();
    apply_touch(std::slice::from_ref(&f), &spec);
    assert_eq!(secs_of(&f), expected + 3600);

    // From the file name.
    let spec = parse_touch("modified=name").unwrap();
    apply_touch(std::slice::from_ref(&f), &spec);
    assert_eq!(
        secs_of(&f),
        crate::tags::epoch_from_civil(2024, 5, 1, 12, 30, 0)
    );

    // No date in the name: skipped, not an error.
    let plain = put(&d, "plain.txt", "x");
    let before = secs_of(&plain);
    let (n, errs) = apply_touch(std::slice::from_ref(&plain), &spec);
    assert_eq!((n, errs.len()), (0, 0));
    assert_eq!(secs_of(&plain), before);
}

#[test]
fn history_records_and_selectively_undoes() {
    let d = tmpdir("hist");
    let hist = d.join("history.tsv");
    let a = put(&d, "a.txt", "A");
    let b = put(&d, "b.txt", "B");

    // Batch: swap a and b, then undo it through history.
    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: b.clone(),
            },
            Op {
                from: b.clone(),
                to: a.clone(),
            },
        ],
        Mode::Rename,
    );
    assert!(res.failed.is_empty());
    let id = record_at(&hist, &res.renamed).unwrap();
    assert_eq!(history_at(&hist), vec![(id, date_str(id), 2)]);

    let (reverted, errors) = undo_at(&hist, Some(id)).unwrap();
    assert_eq!(reverted.len(), 2);
    assert!(errors.is_empty());
    assert_eq!(read(&a), "A");
    assert_eq!(read(&b), "B");
    assert!(
        history_at(&hist).is_empty(),
        "fully undone batch is removed from history"
    );

    // A move batch undoes back out of its subfolder.
    let res = execute(
        vec![Op {
            from: a.clone(),
            to: d.join("sub").join("a.txt"),
        }],
        Mode::Move,
    );
    assert!(res.failed.is_empty());
    record_at(&hist, &res.renamed).unwrap();
    let (reverted, errors) = undo_at(&hist, None).unwrap();
    assert_eq!((reverted.len(), errors.len()), (1, 0));
    assert_eq!(read(&a), "A");
}

#[test]
fn failed_undo_entries_stay_in_history() {
    let d = tmpdir("histfail");
    let hist = d.join("history.tsv");
    let a = put(&d, "a.txt", "A");
    let renamed_a = d.join("a2.txt");
    let b = put(&d, "b.txt", "B");
    let renamed_b = d.join("b2.txt");
    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: renamed_a.clone(),
            },
            Op {
                from: b.clone(),
                to: renamed_b.clone(),
            },
        ],
        Mode::Rename,
    );
    record_at(&hist, &res.renamed).unwrap();

    // Occupy a's original name so undoing it must fail.
    put(&d, "a.txt", "squatter");
    let (reverted, errors) = undo_at(&hist, None).unwrap();
    assert_eq!(reverted.len(), 1);
    assert_eq!(errors.len(), 1);
    assert_eq!(read(&b), "B");
    assert_eq!(
        read(&renamed_a),
        "A",
        "failed revert leaves the file where it was"
    );
    assert_eq!(history_at(&hist).len(), 1, "failed entry kept for retry");

    // Clear the squatter and retry the same batch id.
    fs::remove_file(&a).unwrap();
    let (reverted, errors) = undo_at(&hist, None).unwrap();
    assert_eq!((reverted.len(), errors.len()), (1, 0));
    assert_eq!(read(&a), "A");
    assert!(history_at(&hist).is_empty());
}
