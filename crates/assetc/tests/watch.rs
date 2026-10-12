//! The reload loop's behaviour, tested where it can be asserted: the library state machine and the
//! `--once` command form.
//!
//! A watch loop is hard to test honestly and easy to test *badly* - sleep, touch a file, hope the poll
//! fired - and a test like that fails on a loaded runner while passing for the wrong reason somewhere
//! else. So nothing here waits on a timer. The change detection is a fingerprint over the authoring bytes,
//! so the tests rewrite the file and observe again, which is exactly what a human editing the file makes
//! the loop see. What *is* worth pinning hard is the failure path: a broken edit must leave the previous
//! asset on disk and must not mark itself as compiled.

use assetc::reader::from_bytes;
use assetc::watch::{fingerprint, temp_for, Outcome, Watch};
use std::path::{Path, PathBuf};
use std::process::Command;

fn sphere(r: f32) -> String {
    format!("Sphere(\n    r: {r},\n    centre: (0.0, 0.0, 0.0),\n)\n")
}

fn dir(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "assetc-watch-{tag}-{}-{}",
        std::process::id(),
        // Distinct per test even when two run in the same process.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&path).expect("a temp working directory");
    path
}

fn write(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, text).expect("writable");
    path
}

fn asset_radius(bin: &Path) -> f32 {
    let bytes = std::fs::read(bin).expect("an asset was written");
    let loaded = from_bytes(&bytes).expect("the written asset loads");
    *loaded.params.first().expect("a sphere carries its radius")
}

#[test]
fn the_first_observation_compiles_and_the_second_reports_no_change() {
    let root = dir("first");
    let source = write(&root, "sphere.ron", &sphere(1.0));
    let output = root.join("sphere.bin");
    let mut watcher = Watch::default();

    let first = watcher.observe(source.to_str().unwrap(), output.to_str().unwrap());
    assert!(
        matches!(first, Outcome::Rebuilt { nodes: 1, depth: 1 }),
        "the first observation must compile: {first:?}"
    );
    assert!(output.exists(), "the asset should be on disk");
    assert_eq!(asset_radius(&output), 1.0);
    assert!(
        !temp_for(output.to_str().unwrap()).exists(),
        "the temporary file should be renamed into place, not left beside the asset"
    );

    let second = watcher.observe(source.to_str().unwrap(), output.to_str().unwrap());
    assert_eq!(
        second,
        Outcome::Unchanged,
        "identical bytes must not rebuild"
    );
    assert!(watcher.seen().is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_edit_that_changes_the_bytes_rebuilds_and_the_asset_says_so() {
    let root = dir("edit");
    let source = write(&root, "sphere.ron", &sphere(1.0));
    let output = root.join("sphere.bin");
    let mut watcher = Watch::default();
    assert!(matches!(
        watcher.observe(source.to_str().unwrap(), output.to_str().unwrap()),
        Outcome::Rebuilt { .. }
    ));

    std::fs::write(&source, sphere(2.5)).expect("writable");
    let rebuilt = watcher.observe(source.to_str().unwrap(), output.to_str().unwrap());
    assert!(
        matches!(rebuilt, Outcome::Rebuilt { .. }),
        "a different authoring file must rebuild: {rebuilt:?}"
    );
    assert_eq!(asset_radius(&output), 2.5);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_broken_edit_fails_without_touching_the_asset_the_engine_is_reading() {
    // The whole reason this module exists. A loop that "succeeds" over a stale asset is how an author ends
    // up debugging a shape they already deleted.
    let root = dir("broken");
    let source = write(&root, "sphere.ron", &sphere(1.75));
    let output = root.join("sphere.bin");
    let mut watcher = Watch::default();
    assert!(matches!(
        watcher.observe(source.to_str().unwrap(), output.to_str().unwrap()),
        Outcome::Rebuilt { .. }
    ));
    let good = std::fs::read(&output).expect("the first asset");
    let stamp_before = watcher.seen();

    std::fs::write(&source, "this is not a shape at all").expect("writable");
    let failed = watcher.observe(source.to_str().unwrap(), output.to_str().unwrap());
    let reason = match failed {
        Outcome::Failed(reason) => reason,
        other => panic!("a malformed authoring file must be a failure, got {other:?}"),
    };
    assert!(
        reason.contains("not a valid authoring file"),
        "the refusal should name the reason: {reason}"
    );
    assert_eq!(
        std::fs::read(&output).expect("still there"),
        good,
        "a failed compile must leave the last good asset byte-for-byte intact"
    );
    assert_eq!(
        watcher.seen(),
        stamp_before,
        "a failed compile must not mark the broken bytes as compiled, or fixing them would not rebuild"
    );
    assert!(
        !temp_for(output.to_str().unwrap()).exists(),
        "a failed compile must not leave a temporary file behind"
    );

    // And the repair path: same broken bytes never re-compilable, fixed bytes rebuild from the same state.
    std::fs::write(&source, sphere(0.5)).expect("writable");
    assert!(matches!(
        watcher.observe(source.to_str().unwrap(), output.to_str().unwrap()),
        Outcome::Rebuilt { .. }
    ));
    assert_eq!(asset_radius(&output), 0.5);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_source_is_a_readable_failure_that_leaves_the_asset_alone() {
    // A watch loop has to survive a source that is not there: editors delete-and-recreate, and some swap
    // through a temporary name on every save, which is the same shape as this test. The point of it is that
    // *any* failure leaves the asset alone.
    let root = dir("refused");
    let source = write(
        &root,
        "sphere.ron",
        "Sphere(r: 1.0, centre: (0.0, 0.0, 0.0))",
    );
    let output = root.join("sphere.bin");
    let mut watcher = Watch::default();
    assert!(matches!(
        watcher.observe(source.to_str().unwrap(), output.to_str().unwrap()),
        Outcome::Rebuilt { .. }
    ));
    let good = std::fs::read(&output).expect("asset present");

    // A missing input is the other half: watch must survive a file that is not there yet, because editors
    // delete-then-recreate and an editor that swaps files through a temp name hits this every save.
    let missing = watcher.observe(
        root.join("not-yet-written.ron").to_str().unwrap(),
        output.to_str().unwrap(),
    );
    assert!(
        matches!(&missing, Outcome::Failed(reason) if reason.contains("cannot read")),
        "a missing source should be a readable refusal, got {missing:?}"
    );
    assert_eq!(std::fs::read(&output).expect("still there"), good);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_fingerprint_tracks_the_bytes_and_nothing_else() {
    assert_eq!(fingerprint(&sphere(1.0)), fingerprint(&sphere(1.0)));
    assert_ne!(fingerprint(&sphere(1.0)), fingerprint(&sphere(1.1)));
    // A trailing newline is a byte: an editor that adds one must not leave the asset looking current while
    // the compiler saw different text, and equally a save that changes nothing must not rebuild.
    assert_ne!(fingerprint("a"), fingerprint("a\n"));
}

#[test]
fn the_watch_command_acts_once_when_asked_and_exits_non_zero_on_a_broken_edit() {
    let root = dir("cli");
    let source = write(&root, "sphere.ron", &sphere(1.0));
    let output = root.join("sphere.bin");

    let ok = run(&[
        "watch",
        "--once",
        source.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    assert!(
        ok.status.success(),
        "a first watch pass on a good file must succeed: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let printed = String::from_utf8_lossy(&ok.stdout).into_owned();
    assert!(
        printed.contains("watch:") && printed.contains("node(s)"),
        "the pass should report what it did: {printed}"
    );
    assert!(output.exists());

    std::fs::write(&source, "nonsense").expect("writable");
    let bad = run(&[
        "watch",
        "--once",
        source.to_str().unwrap(),
        output.to_str().unwrap(),
    ]);
    assert!(
        !bad.status.success(),
        "--once must report a broken edit by exiting non-zero"
    );
    let stderr = String::from_utf8_lossy(&bad.stderr).into_owned();
    assert!(
        stderr.contains("the previous asset is unchanged"),
        "the refusal should say what it did not do: {stderr}"
    );
    assert!(output.exists(), "and the asset must still be there");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unresolved_argument_set_is_a_usage_error_not_a_miscompile() {
    let bare = run(&["watch"]);
    assert!(
        !bare.status.success(),
        "`watch` with no files cannot do anything useful"
    );
    let stderr = String::from_utf8_lossy(&bare.stderr).into_owned();
    assert!(
        stderr.contains("usage:"),
        "it should print the usage rather than guess: {stderr}"
    );

    let bad_interval = run(&["watch", "--interval", "soon", "a.ron", "b.bin"]);
    assert!(
        !bad_interval.status.success(),
        "an unparsable --interval must not silently take a default"
    );
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_assetc"))
        .args(args)
        .output()
        .expect("assetc is built for its own integration test")
}
