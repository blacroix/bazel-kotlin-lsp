use bazel_kotlin_lsp::sync::{Located, SyncOptions, locate};
use bkl_core::cache::workspace_key;

fn options(dir: &std::path::Path) -> SyncOptions {
    SyncOptions {
        start_dir: dir.to_path_buf(),
        cache_root: Some(dir.join("cache")),
        bazel: Some(dir.join("no-such-bazel")),
        ..Default::default()
    }
}

#[test]
fn locate_gives_the_cache_layout_without_running_bazel() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    let mut opts = options(dir);
    opts.start_dir = dir.join("sub");
    let Located::Bzlmod(paths) = locate(&opts).unwrap() else {
        panic!("not Bzlmod")
    };
    let key = workspace_key(&dir.canonicalize().unwrap());
    let out = dir.join("cache/bazel-kotlin-lsp").join(key);
    assert_eq!(paths.root, dir);
    assert_eq!(paths.start_dir, dir.join("sub"));
    assert_eq!(paths.layout.dir, out);
    assert_eq!(paths.out_dir, out);
    assert_eq!(paths.model_dir, out.join("model"));
    assert_eq!(paths.log, out.join("sync.log"));
    assert!(
        !dir.join("cache").exists(),
        "locate must not create folders"
    );
}

#[test]
fn locate_follows_an_explicit_out_folder() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    std::fs::write(dir.join("MODULE.bazel"), "").unwrap();
    let mut opts = options(dir);
    opts.out_dir = Some(dir.join("out"));
    let Located::Bzlmod(paths) = locate(&opts).unwrap() else {
        panic!("not Bzlmod")
    };
    assert_eq!(paths.out_dir, dir.join("out"));
    assert_eq!(paths.model_dir, dir.join("out/model"));
    assert_eq!(paths.log, dir.join("out/sync.log"));
}

#[test]
fn locate_outside_bzlmod_says_why() {
    let t = tempfile::tempdir().unwrap();
    let Located::NotBzlmod(why) = locate(&options(t.path())).unwrap() else {
        panic!("should not be Bzlmod")
    };
    assert!(
        why.to_string().contains("not inside a Bazel workspace"),
        "{why}"
    );

    std::fs::write(t.path().join("WORKSPACE"), "").unwrap();
    let Located::NotBzlmod(why) = locate(&options(t.path())).unwrap() else {
        panic!("should not be Bzlmod")
    };
    assert!(why.to_string().contains("WORKSPACE-only"), "{why}");
}
