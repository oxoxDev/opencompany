use super::*;

#[test]
fn a_second_instance_is_refused_while_the_first_holds_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let first = acquire(dir.path()).expect("the first instance takes the root");

    let refused = acquire(dir.path()).expect_err("the second must be refused");
    let message = refused.to_string();
    // The message has to name the directory: an operator seeing this needs
    // to know *which* root is contended, not merely that one is.
    assert!(
        message.contains(&dir.path().display().to_string()),
        "the refusal must name the data root: {message}"
    );
    assert!(
        message.contains("OPENCOMPANY_DATA_DIR"),
        "the refusal must say how to run a second instance anyway: {message}"
    );

    drop(first);
}

#[test]
fn the_root_is_available_again_once_released() {
    let dir = tempfile::tempdir().unwrap();
    let first = acquire(dir.path()).unwrap();
    drop(first);

    // Not merely "does not error": a lock that could not be retaken after a
    // clean shutdown would make every restart of a desktop app fail.
    let _retaken = acquire(dir.path()).expect("a released root is takeable");
}

#[test]
fn two_roots_do_not_contend() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let _first = acquire(a.path()).unwrap();
    let _second = acquire(b.path()).expect("a different root is unaffected");
}

#[test]
fn the_root_is_created_when_it_does_not_exist_yet() {
    // First launch of a desktop app: the platform data directory has never
    // been written to.
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("does/not/exist/yet");
    let _lock = acquire(&nested).expect("a fresh root is created and locked");
    assert!(nested.join(LOCK_FILE).is_file());
}

#[test]
fn an_existing_lock_file_is_reused_rather_than_truncated() {
    // The file is never deleted on release, so the next run opens one that
    // already exists. Truncating would be harmless today and a data loss the
    // moment anything is ever written into it.
    let dir = tempfile::tempdir().unwrap();
    drop(acquire(dir.path()).unwrap());
    std::fs::write(dir.path().join(LOCK_FILE), b"marker").unwrap();

    let lock = acquire(dir.path()).unwrap();
    // Released before reading. On Windows a byte-range lock is *mandatory*
    // rather than advisory, so reading the file while the lock is held fails
    // with a sharing violation (os error 33) — the read this used to do was
    // asking the platform for something it forbids, not observing anything
    // about the lock. Dropping first checks the same property: the content was
    // still there after the acquire, so the acquire did not truncate it.
    drop(lock);
    assert_eq!(
        std::fs::read(dir.path().join(LOCK_FILE)).unwrap(),
        b"marker"
    );
}
