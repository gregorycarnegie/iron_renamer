use super::*;
use crate::engine::build_rule;
use googletest::prelude::*;
use proptest::prelude::*;
use rstest::rstest;
use std::time::UNIX_EPOCH;

proptest! {
    #[test]
    fn auto_pad_is_the_digit_count_of_the_last_counter(
        start in prop_oneof![0usize..1000, Just(usize::MAX), any::<usize>()],
        count in prop_oneof![0usize..1000, Just(usize::MAX), any::<usize>()],
    ) {
        let last = start as u128 + count.max(1) as u128 - 1;
        prop_assert_eq!(auto_pad(start, count) as usize, last.to_string().len());
    }
}

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
#[gtest]
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
        expect_that!(res.failed, is_empty());
        expect_that!(res.renamed, len(eq(n)));

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
#[gtest]
#[ignore]
fn bench_plan_preview() {
    const N: usize = 10_000;
    let d = tmpdir("bench_plan");
    let files: Vec<PathBuf> = (0..N)
        .map(|i| put(&d, &format!("file_{i:06}.txt"), "x"))
        .collect();
    let rules = rules(&[("replace", "file", "photo")]);
    let no_overrides = HashMap::new();
    let cfg = BatchCfg::rename(&rules, 1, 1, &no_overrides);
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let items = plan(&files, &cfg);
        expect_that!(items, len(eq(N)));
        expect_that!(items, each(field!(&PlanItem.issue, ref none())));
        println!("plan {N}: {:?}", start.elapsed());
    }
    let _ = fs::remove_dir_all(&d);
}

#[rstest]
#[case::csv("csv", "from,to\n\"a,b\",x\n")]
#[case::json("json", "[\n  {\"from\": \"a,b\", \"to\": \"x\"}\n]\n")]
#[case::text("txt", "a,b -> x\n")]
fn export_rows_formats_all_outputs(#[case] ext: &str, #[case] expected: &str) {
    let d = tmpdir(&format!("export_{ext}"));
    let rows = vec![(vec!["a,b".into(), "x".into()], "a,b -> x".into())];
    let path = d.join(format!("out.{ext}"));
    export_rows(&path, &["from", "to"], rows).unwrap();
    assert_eq!(read(&path), expected);
}

#[rstest]
#[case::normal("ok.txt", false)]
#[case::com_without_digit("common.txt", false)]
#[case::empty("", true)]
#[case::invalid_character("a<b.txt", true)]
#[case::control_character("a\tb.txt", true)]
#[case::reserved_name("CON.txt", true)]
#[case::reserved_device("com3", true)]
#[case::trailing_dot("trailing.", true)]
#[case::trailing_space("trailing ", true)]
fn validates_names(#[case] name: &str, #[case] invalid: bool) {
    assert_eq!(name_issue(name).is_some(), invalid);
}

#[gtest]
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
    expect_that!(res.renamed, len(eq(2)));
    expect_that!(res.failed, is_empty());
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
    expect_that!(res.failed, is_empty());
    assert_eq!(read(&two), "one");
    assert_eq!(read(&three), "two");
    assert!(!one.exists());
}

#[gtest]
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
    expect_that!(res.renamed, len(eq(1)));
    expect_that!(res.failed, len(eq(1)));
    assert_eq!(read(&a), "A", "failed op leaves its file untouched");
    assert_eq!(read(&blocker), "X", "existing file never overwritten");
    assert_eq!(read(&d.join("free.txt")), "B");
}

#[gtest]
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
    expect_that!(res.failed, is_empty());
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
    expect_that!(res.failed, len(eq(1)));

    // Move creates directories and removes the source.
    let b = put(&d, "b.txt", "B");
    let res = execute(
        vec![Op {
            from: b.clone(),
            to: sub.join("b.txt"),
        }],
        Mode::Move,
    );
    expect_that!(res.failed, is_empty());
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
    expect_that!(res.failed, is_empty());
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
    expect_that!(res.failed, is_empty());
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
    expect_that!(res.failed, len(eq(1)));
    assert!(!inside.exists());
}

#[gtest]
fn plan_flags_conflicts_and_allows_case_only() {
    let d = tmpdir("plan");
    put(&d, "img1.jpg", "");
    put(&d, "img2.jpg", "");
    put(&d, "other.jpg", "");
    let files = vec![d.join("img1.jpg"), d.join("img2.jpg")];
    let no_overrides = HashMap::new();

    let case_rule = rules(&[("replace", "img", "IMG")]);
    let items = plan(&files, &BatchCfg::rename(&case_rule, 1, 1, &no_overrides));
    assert!(
        items.iter().all(|i| i.changed && i.issue.is_none()),
        "case-only renames are valid"
    );

    let dup_rule = rules(&[("pattern", "same.jpg", "")]);
    let items = plan(&files, &BatchCfg::rename(&dup_rule, 1, 1, &no_overrides));
    assert_eq!(items[1].issue.as_deref(), Some("duplicate target"));

    let clash_rule = rules(&[("replace", "img1", "other")]);
    let items = plan(&files, &BatchCfg::rename(&clash_rule, 1, 1, &no_overrides));
    assert_eq!(items[0].issue.as_deref(), Some("target exists"));
    expect_that!(items[1].issue, none());

    // Swap inside one batch is not a conflict: each target is vacated.
    let swap_rule = rules(&[
        ("replace", "img1", "tmpX"),
        ("replace", "img2", "img1"),
        ("replace", "tmpX", "img2"),
    ]);
    let items = plan(&files, &BatchCfg::rename(&swap_rule, 1, 1, &no_overrides));
    expect_that!(
        items,
        each(all!(
            field!(&PlanItem.changed, eq(true)),
            field!(&PlanItem.issue, ref none())
        ))
    );

    // A manual override wins over rules but is validated like any name.
    let over: HashMap<PathBuf, String> = [(files[0].clone(), "manual.jpg".to_string())].into();
    let cfg = BatchCfg {
        overrides: &over,
        ..BatchCfg::rename(&case_rule, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "manual.jpg");
    expect_that!(items[0].issue, none());
}

#[gtest]
fn collision_policies_resolve_in_preview() {
    let d = tmpdir("collide");
    put(&d, "a.jpg", "");
    put(&d, "b.jpg", "");
    put(&d, "same.jpg", "");
    let files = vec![d.join("a.jpg"), d.join("b.jpg")];
    let no_overrides = HashMap::new();
    let dup_rule = rules(&[("pattern", "same.jpg", "")]);

    let cfg = BatchCfg {
        collision: Collision::Number,
        ..BatchCfg::rename(&dup_rule, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same (2).jpg", "disk collision numbered");
    assert_eq!(
        items[1].new_name, "same (3).jpg",
        "batch duplicate numbered"
    );
    expect_that!(items, each(field!(&PlanItem.issue, ref none())));

    let cfg = BatchCfg {
        collision: Collision::Letter,
        ..BatchCfg::rename(&dup_rule, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same_b.jpg");
    assert_eq!(items[1].new_name, "same_c.jpg");

    let cfg = BatchCfg {
        collision: Collision::Pattern("_<index>".into()),
        ..BatchCfg::rename(&dup_rule, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same_1.jpg");
    assert_eq!(items[1].new_name, "same_2.jpg");
}

#[gtest]
fn plan_copy_move_destinations() {
    let d = tmpdir("dest");
    put(&d, "a.jpg", "");
    put(&d, "b.txt", "");
    let files = vec![d.join("a.jpg"), d.join("b.txt")];
    let no_overrides = HashMap::new();

    // Tag-expanded relative destination: sorted/<ext>.
    let cfg = BatchCfg {
        mode: Mode::Copy,
        dest: "sorted/<ext>",
        ..BatchCfg::rename(&[], 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    expect_that!(
        items,
        each(all!(
            field!(&PlanItem.changed, eq(true)),
            field!(&PlanItem.issue, ref none())
        ))
    );
    assert_eq!(items[0].target, d.join("sorted").join("jpg").join("a.jpg"));
    assert_eq!(items[1].target, d.join("sorted").join("txt").join("b.txt"));

    // Copy onto itself (empty dest, no rules) is a no-op, not a conflict.
    let cfg = BatchCfg {
        mode: Mode::Copy,
        ..BatchCfg::rename(&[], 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    expect_that!(items, each(field!(&PlanItem.changed, eq(false))));
}

#[gtest]
fn file_pairs_share_the_generated_stem() {
    let d = tmpdir("pairs");
    put(&d, "img1.jpg", "");
    put(&d, "img1.xmp", "");
    put(&d, "img2.jpg", "");
    let files = vec![d.join("img1.jpg"), d.join("img1.xmp"), d.join("img2.jpg")];
    let no_overrides = HashMap::new();
    let pat = rules(&[("pattern", "pic_<num>.<ext>", "")]);
    let cfg = BatchCfg {
        pairs: true,
        ..BatchCfg::rename(&pat, 1, 1, &no_overrides)
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
    expect_that!(items, each(field!(&PlanItem.issue, ref none())));
    // Without pairs the sidecar gets its own counter value.
    let items = plan(&files, &BatchCfg::rename(&pat, 1, 1, &no_overrides));
    assert_eq!(items[1].new_name, "pic_2.xmp");
}

#[gtest]
fn touch_parses_and_sets_times() {
    expect_that!(parse_touch("no-equals"), err(anything()));
    expect_that!(parse_touch("bogus=+1h"), err(anything()));
    expect_that!(parse_touch("modified=junk"), err(anything()));
    let spec = parse_touch("created,accessed=+1h");
    if cfg!(any(windows, target_os = "macos")) {
        let spec = spec.unwrap();
        assert!(spec.created && spec.accessed && !spec.modified);
    } else {
        expect_that!(spec, err(anything()));
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

// Deltas past i64 used to overflow (a panic in debug builds, a wrapped time
// in release ones); far-future times can also exceed the OS time type.
#[gtest]
fn touch_deltas_out_of_range_fail_cleanly() {
    expect_that!(
        parse_touch("modified=+9223372036854775807d").err(),
        some(contains_substring("bad time delta"))
    );

    let d = tmpdir("touch_range");
    let f = put(&d, "a.txt", "x");
    let spec = parse_touch("modified=+9223372036854775807s").unwrap();
    let (n, errs) = apply_touch(std::slice::from_ref(&f), &spec);
    expect_that!(n, eq(0));
    expect_that!(errs, elements_are![contains_substring("out of range")]);

    // Far in the past clamps to the epoch rather than failing.
    let spec = parse_touch("modified=-9223372036854775807s").unwrap();
    expect_that!(
        apply_touch(std::slice::from_ref(&f), &spec),
        eq(&(1, vec![]))
    );
}

#[gtest]
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
    expect_that!(res.failed, is_empty());
    let id = record_at(&hist, &res.renamed).unwrap();
    assert_eq!(history_at(&hist), vec![(id, date_str(id), 2)]);

    let (reverted, errors) = undo_at(&hist, Some(id)).unwrap();
    expect_that!(reverted, len(eq(2)));
    expect_that!(errors, is_empty());
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
    expect_that!(res.failed, is_empty());
    record_at(&hist, &res.renamed).unwrap();
    let (reverted, errors) = undo_at(&hist, None).unwrap();
    assert_eq!((reverted.len(), errors.len()), (1, 0));
    assert_eq!(read(&a), "A");
}

#[gtest]
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
    expect_that!(reverted, len(eq(1)));
    expect_that!(errors, len(eq(1)));
    assert_eq!(read(&b), "B");
    assert_eq!(
        read(&renamed_a),
        "A",
        "failed revert leaves the file where it was"
    );
    expect_that!(history_at(&hist), len(eq(1)), "failed entry kept for retry");

    // Clear the squatter and retry the same batch id.
    fs::remove_file(&a).unwrap();
    let (reverted, errors) = undo_at(&hist, None).unwrap();
    assert_eq!((reverted.len(), errors.len()), (1, 0));
    assert_eq!(read(&a), "A");
    expect_that!(history_at(&hist), is_empty());
}

// ── mutation-testing gap fills (2026-08-30 baseline) ──

#[test]
fn counters_advance_per_item_and_per_folder() {
    let d = tmpdir("counters");
    let (one, two) = (d.join("one"), d.join("two"));
    fs::create_dir_all(&one).unwrap();
    fs::create_dir_all(&two).unwrap();
    let files = vec![
        put(&one, "a.txt", ""),
        put(&one, "b.txt", ""),
        put(&two, "c.txt", ""),
    ];
    let no_overrides = HashMap::new();
    // start 5, so the batch counter is start + i and not start * i or i alone.
    let r = rules(&[("pattern", "<num>_<dirnum>.<ext>", "")]);
    let items = plan(&files, &BatchCfg::rename(&r, 5, 1, &no_overrides));
    let names: Vec<&str> = items.iter().map(|i| i.new_name.as_str()).collect();
    // <dirnum> restarts in each folder; <num> never does.
    assert_eq!(names, ["5_1.txt", "6_2.txt", "7_1.txt"]);
}

#[gtest]
fn copy_mode_treats_a_case_only_target_as_a_collision() {
    let d = tmpdir("caseonly");
    let files = vec![put(&d, "photo.jpg", "x")];
    let no_overrides = HashMap::new();
    let r = rules(&[("case", "upper", "")]);

    // Renaming photo.jpg -> PHOTO.JPG is a legal case-only rename.
    let items = plan(&files, &BatchCfg::rename(&r, 1, 1, &no_overrides));
    assert_eq!(items[0].new_name, "PHOTO.JPG");
    expect_that!(items[0].issue, none(), "{:?}", items[0].issue);

    // Copying onto itself is not: the source file is still there.
    let cfg = BatchCfg {
        mode: Mode::Copy,
        ..BatchCfg::rename(&r, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].issue.as_deref(), Some("target exists"));
}

#[gtest]
fn a_collision_pattern_that_repeats_is_rejected() {
    let d = tmpdir("collidepat");
    put(&d, "same.jpg", "");
    let files = vec![put(&d, "a.jpg", ""), put(&d, "b.jpg", "")];
    let no_overrides = HashMap::new();
    let r = rules(&[("pattern", "same.<ext>", "")]);

    // The pattern is a stem suffix. A constant one cannot resolve a second
    // collision, so the planner must give up rather than loop or overwrite.
    let cfg = BatchCfg {
        collision: Collision::Pattern("_dup".into()),
        ..BatchCfg::rename(&r, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    assert_eq!(items[0].new_name, "same_dup.jpg");
    expect_that!(items[0].issue, none());
    assert_eq!(
        items[1].issue.as_deref(),
        Some("collision pattern is not unique")
    );

    // A suffix that does vary resolves both.
    let cfg = BatchCfg {
        collision: Collision::Pattern("_<index>".into()),
        ..BatchCfg::rename(&r, 1, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    expect_that!(items, each(field!(&PlanItem.issue, ref none())));
    assert_eq!(items[0].new_name, "same_1.jpg");
    assert_eq!(items[1].new_name, "same_2.jpg");

    // The counter inside a collision suffix is start + i, like <num> anywhere
    // else: from 5 over two files that is 5 and 6.
    let cfg = BatchCfg {
        collision: Collision::Pattern("_<num>".into()),
        ..BatchCfg::rename(&r, 5, 1, &no_overrides)
    };
    let items = plan(&files, &cfg);
    expect_that!(items, each(field!(&PlanItem.issue, ref none())));
    assert_eq!(items[0].new_name, "same_5.jpg");
    assert_eq!(items[1].new_name, "same_6.jpg");
}

#[gtest]
fn an_over_long_target_path_is_refused() {
    let d = tmpdir("toolong");
    let files = vec![put(&d, "a.txt", "")];
    let no_overrides = HashMap::new();
    let long = "x".repeat(MAX_WINDOWS_PATH_LEN);
    let r = rules(&[("pattern", &format!("{long}.<ext>"), "")]);
    let items = plan(&files, &BatchCfg::rename(&r, 1, 1, &no_overrides));
    assert_eq!(items[0].issue.as_deref(), Some("path too long"));

    // A short one is fine, so the check is a length test and not a constant.
    let r = rules(&[("pattern", "short.<ext>", "")]);
    let items = plan(&files, &BatchCfg::rename(&r, 1, 1, &no_overrides));
    expect_that!(items[0].issue, none());
}

#[gtest]
fn finals_report_targets_for_successes_and_sources_otherwise() {
    let d = tmpdir("finals");
    let files = vec![put(&d, "a.txt", "a"), put(&d, "b.txt", "b")];
    let no_overrides = HashMap::new();
    // Only a.txt changes; b.txt keeps its name.
    let r = rules(&[("replace", "a.txt", "renamed.txt")]);
    let items = plan(&files, &BatchCfg::rename(&r, 1, 1, &no_overrides));
    let ops: Vec<Op> = items
        .iter()
        .filter(|i| i.changed && i.issue.is_none())
        .map(|i| Op {
            from: i.from.clone(),
            to: i.target.clone(),
        })
        .collect();
    expect_that!(ops, len(eq(1)));
    let res = execute(ops, Mode::Rename);
    expect_that!(res.failed, is_empty(), "{:?}", res.failed);

    let out = finals(&items, &res);
    // One entry per planned item, in order: the new path where it moved, the
    // original where it did not.
    assert_eq!(out, vec![d.join("renamed.txt"), d.join("b.txt")]);
}

#[test]
fn a_folder_cannot_be_copied_or_moved_inside_itself() {
    let d = tmpdir("selfnest");
    let src = d.join("box");
    fs::create_dir_all(&src).unwrap();
    put(&src, "inner.txt", "x");
    let nested = src.join("box-copy");

    for mode in [Mode::Copy, Mode::Move] {
        let err = transfer(&src, &nested, mode).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
    // Somewhere else entirely is fine.
    let outside = d.join("elsewhere");
    transfer(&src, &outside, Mode::Copy).unwrap();
    assert_eq!(read(&outside.join("inner.txt")), "x");
}

#[gtest]
fn execute_refuses_to_clobber_an_existing_target() {
    let d = tmpdir("clobber");
    let a = put(&d, "a.txt", "keep me");
    let b = put(&d, "b.txt", "victim");
    let res = execute(
        vec![Op {
            from: a.clone(),
            to: b.clone(),
        }],
        Mode::Rename,
    );
    expect_that!(res.renamed, is_empty());
    expect_that!(res.failed, len(eq(1)));
    // Both files still hold what they held.
    assert_eq!(read(&a), "keep me");
    assert_eq!(read(&b), "victim");
}

#[gtest]
fn two_independent_cycles_resolve_in_one_batch() {
    // Each cycle needs its own file moved aside, so the temp names must not
    // collide with one another or with anything already on disk.
    let d = tmpdir("twocycles");
    let (a, b) = (put(&d, "a.txt", "A"), put(&d, "b.txt", "B"));
    let (c, e) = (put(&d, "c.txt", "C"), put(&d, "e.txt", "E"));
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
            Op {
                from: c.clone(),
                to: e.clone(),
            },
            Op {
                from: e.clone(),
                to: c.clone(),
            },
        ],
        Mode::Rename,
    );
    expect_that!(res.failed, is_empty(), "{:?}", res.failed);
    expect_that!(res.renamed, len(eq(4)));
    assert_eq!((read(&a), read(&b)), ("B".to_string(), "A".to_string()));
    assert_eq!((read(&c), read(&e)), ("E".to_string(), "C".to_string()));
    assert_eq!(
        fs::read_dir(&d).unwrap().count(),
        4,
        "temp files left behind"
    );
}

#[gtest]
fn a_three_way_cycle_rotates_every_file() {
    let d = tmpdir("threecycle");
    let (a, b, c) = (
        put(&d, "a.txt", "A"),
        put(&d, "b.txt", "B"),
        put(&d, "c.txt", "C"),
    );
    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: b.clone(),
            },
            Op {
                from: b.clone(),
                to: c.clone(),
            },
            Op {
                from: c.clone(),
                to: a.clone(),
            },
        ],
        Mode::Rename,
    );
    expect_that!(res.failed, is_empty(), "{:?}", res.failed);
    expect_that!(res.renamed, len(eq(3)));
    assert_eq!(read(&a), "C");
    assert_eq!(read(&b), "A");
    assert_eq!(read(&c), "B");
    assert_eq!(
        fs::read_dir(&d).unwrap().count(),
        3,
        "temp files left behind"
    );
}

#[gtest]
fn a_source_used_twice_only_frees_its_name_once() {
    // Two ops read the same file and a third wants to take its name. The
    // name must not be handed over until *both* readers are done with it,
    // or the third op overwrites a file the second still needs.
    let d = tmpdir("dupsource");
    let a = put(&d, "a.txt", "A");
    let (b, c) = (d.join("b.txt"), d.join("c.txt"));
    let e = put(&d, "e.txt", "E");

    let res = execute(
        vec![
            Op {
                from: a.clone(),
                to: b.clone(),
            },
            Op {
                from: a.clone(),
                to: c.clone(),
            },
            Op {
                from: e.clone(),
                to: a.clone(),
            },
        ],
        Mode::Rename,
    );
    // a.txt can only move once, so exactly one of the first two ops succeeds
    // and the other reports a missing source; e.txt then takes the name.
    expect_that!(res.renamed, len(eq(2)), "{:?}", res.failed);
    expect_that!(res.failed, len(eq(1)));
    assert_eq!(read(&a), "E", "e.txt took the freed name");
    let moved = if b.exists() { read(&b) } else { read(&c) };
    assert_eq!(moved, "A");
    assert!(b.exists() != c.exists(), "only one target should exist");
    assert!(!e.exists());
    assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
}

#[gtest]
fn export_results_writes_the_execution_log() {
    let d = tmpdir("exportres");
    let a = put(&d, "a.txt", "A");
    let b = d.join("b.txt");
    let res = execute(
        vec![Op {
            from: a.clone(),
            to: b.clone(),
        }],
        Mode::Rename,
    );
    expect_that!(res.renamed, len(eq(1)));

    let out = d.join("log.csv");
    export_results(&res, &out).unwrap();
    let text = fs::read_to_string(&out).unwrap();
    expect_that!(text, contains_substring("a.txt"));
    expect_that!(text, contains_substring("b.txt"));
    assert!(text.lines().count() >= 2, "header plus a row: {text}");
}

#[test]
fn renumber_handles_a_digit_run_at_the_very_end() {
    // The digit scan must stop at the last byte; walking one past it panics.
    let d = tmpdir("renum");
    let no_overrides = HashMap::new();
    let r = rules(&[("renumber", "1", "+10")]);

    let items = plan(
        &[put(&d, "shot7.txt", "")],
        &BatchCfg::rename(&r, 1, 1, &no_overrides),
    );
    assert_eq!(items[0].new_name, "shot17.txt");

    // A name that is nothing but digits is the same edge from the other side.
    let items = plan(
        &[put(&d, "42", "")],
        &BatchCfg::rename(&r, 1, 1, &no_overrides),
    );
    assert_eq!(items[0].new_name, "52");
}

#[gtest]
fn touch_from_exif_falls_back_when_the_first_tag_is_absent() {
    // A file carrying CreateDate but no DateTimeOriginal. The first lookup
    // yields Some("") -- present but empty -- which must be treated as absent
    // so the fallback runs, not fed to the date parser.
    let d = tmpdir("touchexif");
    let p = d.join("shot.jpg");
    fs::write(&p, include_bytes!("../../tests/fixtures/1x1.jpg")).unwrap();
    crate::meta::set(
        std::slice::from_ref(&p),
        &["createdate=2019:03:04 05:06:07".into()],
    )
    .unwrap();

    let spec = parse_touch("modified=exif").unwrap();
    let (done, errs) = apply_touch(std::slice::from_ref(&p), &spec);
    assert_eq!((done, errs.len()), (1, 0), "{errs:?}");

    let secs = fs::metadata(&p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(crate::tags::civil_utc(secs).0, 2019, "took the CreateDate");

    // A readable file with no date metadata is skipped silently, by design.
    let plain = put(&d, "plain.txt", "");
    assert_eq!(apply_touch(&[plain], &spec), (0, vec![]));

    // An unreadable one is an error, so a typo in a path is not swallowed.
    let (done, errs) = apply_touch(&[d.join("not_here.jpg")], &spec);
    assert_eq!(done, 0);
    expect_that!(errs, len(eq(1)), "{errs:?}");
    expect_that!(errs[0], contains_substring("unreadable"), "{errs:?}");
}

// ───────────────────────── preview / result export (export.rs)

/// Splits CSV text into records the way an RFC 4180 reader does: a line
/// break ends a record only outside quotes. A bare CR outside quotes is an
/// error, since readers disagree on whether it ends the row.
fn csv_records(text: &str) -> std::result::Result<Vec<String>, String> {
    let mut records = Vec::new();
    let (mut cur, mut in_q) = (String::new(), false);
    for c in text.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            '\n' if !in_q => records.push(std::mem::take(&mut cur)),
            '\r' if !in_q => return Err(format!("bare CR in {text:?}")),
            c => cur.push(c),
        }
    }
    if !cur.is_empty() || in_q {
        return Err(format!("unterminated last record in {text:?}"));
    }
    Ok(records)
}

/// Every JSON string literal in `text`, decoded, in order.
fn json_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        loop {
            match chars.next().expect("unterminated string") {
                '"' => break,
                '\\' => match chars.next().expect("dangling escape") {
                    'n' => s.push('\n'),
                    'r' => s.push('\r'),
                    't' => s.push('\t'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        let code = u32::from_str_radix(&hex, 16).unwrap();
                        s.push(char::from_u32(code).unwrap());
                    }
                    other => s.push(other),
                },
                c => {
                    assert!(c as u32 >= 0x20, "raw control character {c:?} in {text:?}");
                    s.push(c);
                }
            }
        }
        out.push(s);
    }
    out
}

fn export_text(file: &str, headers: &[&str], rows: Vec<(Vec<String>, String)>) -> String {
    let d = std::env::temp_dir().join(format!(
        "iron_renamer_test_export_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::create_dir_all(&d).unwrap();
    let path = d.join(file);
    export_rows(&path, headers, rows).unwrap();
    let text = read(&path);
    let _ = fs::remove_file(&path);
    text
}

fn one_row() -> Vec<(Vec<String>, String)> {
    vec![(vec!["a".into(), "b".into()], "a -> b".into())]
}

#[rstest]
#[case::upper_case_csv("out.CSV", "from,to\na,b\n")]
#[case::mixed_case_json("out.Json", "[\n  {\"from\": \"a\", \"to\": \"b\"}\n]\n")]
#[case::last_extension_counts("out.tar.json", "[\n  {\"from\": \"a\", \"to\": \"b\"}\n]\n")]
#[case::csv_then_txt_is_text("out.csv.txt", "a -> b\n")]
#[case::no_extension_is_text("out", "a -> b\n")]
#[case::unknown_extension_is_text("out.log", "a -> b\n")]
#[gtest]
fn the_extension_picks_the_export_format(#[case] file: &str, #[case] expected: &str) {
    expect_that!(export_text(file, &["from", "to"], one_row()), eq(expected));
}

#[rstest]
#[case::csv_keeps_the_header("e.csv", "from,to\n")]
#[case::json_is_an_empty_array("e.json", "[\n\n]\n")]
#[case::text_is_empty("e.txt", "")]
#[gtest]
fn exporting_no_rows(#[case] file: &str, #[case] expected: &str) {
    expect_that!(export_text(file, &["from", "to"], vec![]), eq(expected));
}

#[gtest]
fn a_carriage_return_in_a_name_is_quoted_in_csv() {
    let rows = vec![(vec!["a\rb".into(), "c".into()], String::new())];
    expect_that!(
        export_text("cr.csv", &["from", "to"], rows),
        eq("from,to\n\"a\rb\",c\n")
    );
}

#[gtest]
fn export_into_a_missing_folder_reports_the_io_error() {
    let path = tmpdir("export_missing").join("no_such_dir").join("out.csv");
    expect_that!(
        export_rows(&path, &["from"], vec![]).map_err(|e| e.kind()),
        err(eq(io::ErrorKind::NotFound))
    );
}

fn preview_items(d: &Path) -> Vec<PlanItem> {
    vec![
        PlanItem {
            from: d.join("a.txt"),
            new_name: "b.txt".into(),
            target: d.join("b.txt"),
            changed: true,
            issue: None,
        },
        PlanItem {
            from: d.join("same.txt"),
            new_name: "same.txt".into(),
            target: d.join("same.txt"),
            changed: false,
            issue: None,
        },
        PlanItem {
            from: d.join("x,y.txt"),
            new_name: "c.txt".into(),
            target: d.join("c.txt"),
            changed: true,
            issue: Some("conflict: c.txt".into()),
        },
    ]
}

#[gtest]
fn preview_csv_has_old_new_target_and_status() {
    let d = tmpdir("export_preview_csv");
    let out = d.join("p.csv");
    export_preview(&preview_items(&d), &out).unwrap();
    let records = csv_records(&read(&out)).unwrap();
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| crate::presets::csv_split(r))
        .collect();
    let t = |n: &str| d.join(n).display().to_string();
    expect_that!(
        rows,
        elements_are![
            elements_are![eq("old"), eq("new"), eq("target"), eq("status")],
            elements_are![eq("a.txt"), eq("b.txt"), eq(&t("b.txt")), eq("ok")],
            elements_are![
                eq("same.txt"),
                eq("same.txt"),
                eq(&t("same.txt")),
                eq("unchanged")
            ],
            // An issue wins over the changed flag.
            elements_are![
                eq("x,y.txt"),
                eq("c.txt"),
                eq(&t("c.txt")),
                eq("conflict: c.txt")
            ],
        ]
    );
}

#[gtest]
fn preview_text_shows_full_paths_and_status() {
    let d = tmpdir("export_preview_txt");
    let out = d.join("p.txt");
    export_preview(&preview_items(&d), &out).unwrap();
    let line = |from: &str, to: &str, status: &str| {
        format!(
            "{}  ->  {}   [{status}]",
            d.join(from).display(),
            d.join(to).display()
        )
    };
    expect_that!(
        read(&out),
        eq(&format!(
            "{}\n{}\n{}\n",
            line("a.txt", "b.txt", "ok"),
            line("same.txt", "same.txt", "unchanged"),
            line("x,y.txt", "c.txt", "conflict: c.txt"),
        ))
    );
}

#[gtest]
fn preview_json_keys_every_value_by_its_header() {
    let d = tmpdir("export_preview_json");
    let out = d.join("p.json");
    export_preview(&preview_items(&d)[..1], &out).unwrap();
    let target = d.join("b.txt").display().to_string();
    expect_that!(
        json_strings(&read(&out)),
        elements_are![
            eq("old"),
            eq("a.txt"),
            eq("new"),
            eq("b.txt"),
            eq("target"),
            eq(&target),
            eq("status"),
            eq("ok"),
        ]
    );
}

#[gtest]
fn results_list_successes_then_failures() {
    let d = tmpdir("export_results_order");
    let op = |a: &str, b: &str| Op {
        from: d.join(a),
        to: d.join(b),
    };
    let res = ExecResult {
        renamed: vec![op("a", "b"), op("c", "d")],
        failed: vec![(op("e", "f"), "denied".into())],
    };
    let out = d.join("r.json");
    export_results(&res, &out).unwrap();
    let p = |n: &str| d.join(n).display().to_string();
    expect_that!(
        json_strings(&read(&out)),
        elements_are![
            eq("from"),
            eq(&p("a")),
            eq("to"),
            eq(&p("b")),
            eq("result"),
            eq("done"),
            eq("from"),
            eq(&p("c")),
            eq("to"),
            eq(&p("d")),
            eq("result"),
            eq("done"),
            eq("from"),
            eq(&p("e")),
            eq("to"),
            eq(&p("f")),
            eq("result"),
            eq("failed: denied"),
        ]
    );

    let txt = d.join("r.txt");
    export_results(&res, &txt).unwrap();
    expect_that!(
        read(&txt).lines().last(),
        some(eq(&format!(
            "{}  ->  {}   [failed: denied]",
            p("e"),
            p("f")
        )))
    );
}

fn export_rows_strategy() -> impl Strategy<Value = Vec<(Vec<String>, String)>> {
    prop::collection::vec(
        (prop::collection::vec(any::<String>(), 3), "[^\n]{0,12}"),
        0..5,
    )
}

proptest! {
    // Any file name, including commas, quotes and line breaks, survives a
    // CSV export and comes back field for field.
    #[test]
    fn csv_export_reads_back_field_for_field(rows in export_rows_strategy()) {
        let text = export_text("p.csv", &["a", "b", "c"], rows.clone());
        let records = csv_records(&text).map_err(TestCaseError::fail)?;
        prop_assert_eq!(records.len(), rows.len() + 1);
        prop_assert_eq!(&records[0], "a,b,c");
        for (record, (values, _)) in records[1..].iter().zip(&rows) {
            prop_assert_eq!(&crate::presets::csv_split(record), values);
        }
    }

    // JSON export: one object per line, every value decodes to the original.
    #[test]
    fn json_export_reads_back_value_for_value(rows in export_rows_strategy()) {
        let text = export_text("p.json", &["a", "b", "c"], rows.clone());
        prop_assert!(text.starts_with("[\n") && text.ends_with("\n]\n"), "{text:?}");
        // An empty export still has the blank line between the brackets.
        prop_assert_eq!(text.lines().count(), rows.len().max(1) + 2);
        let expected: Vec<String> = rows
            .iter()
            .flat_map(|(values, _)| ["a", "b", "c"].iter().zip(values).flat_map(|(k, v)| [k.to_string(), v.clone()]))
            .collect();
        prop_assert_eq!(json_strings(&text), expected);
    }

    #[test]
    fn text_export_is_one_line_per_row(rows in export_rows_strategy()) {
        let text = export_text("p.txt", &["a", "b", "c"], rows.clone());
        // split_terminator, not lines(): lines() would also eat a trailing CR.
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        let expected: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
        prop_assert_eq!(lines, expected);
        prop_assert_eq!(text.ends_with('\n'), !rows.is_empty());
    }
}
