//! File watching end to end: real file events, a counting syncer and a short debounce.

use std::io::PipeWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bazel_kotlin_lsp::bazel::Cancel;
use bazel_kotlin_lsp::proxy::{BazelMode, Mode, Syncer, serve};
use bazel_kotlin_lsp::sync::{Progress, SyncError, SyncOutcome, SyncSummary};

struct CountingSyncer {
    runs: Mutex<usize>,
    module_dirs: Vec<PathBuf>,
}

impl Syncer for CountingSyncer {
    fn sync(&self, _: &Cancel, _: &Progress) -> Result<SyncOutcome, SyncError> {
        *self.runs.lock().unwrap() += 1;
        Ok(SyncOutcome {
            summary: SyncSummary {
                status: "ok".into(),
                ..Default::default()
            },
            local_module_dirs: self.module_dirs.clone(),
            ..Default::default()
        })
    }
}

impl CountingSyncer {
    fn runs(&self) -> usize {
        *self.runs.lock().unwrap()
    }

    /// The run count once it has not moved for a second.
    fn settled(&self) -> usize {
        let mut last = self.runs();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = self.runs();
            if now == last {
                return now;
            }
            last = now;
        }
    }

    fn wait_for(&self, runs: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.runs() < runs {
            assert!(
                Instant::now() < deadline,
                "{} runs, expected {runs}",
                self.runs()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Keeps the proxy running until dropped, which closes its input.
struct Running {
    _to_proxy: PipeWriter,
}

fn start(root: &Path, syncer: Arc<CountingSyncer>) -> Running {
    let (client_r, to_proxy) = std::io::pipe().unwrap();
    let (proxy_r, client_w) = std::io::pipe().unwrap();
    let mode = Mode::Bazel(
        BazelMode::new(root.join("cache/model"), syncer)
            .with_watch(root.to_path_buf(), Duration::from_millis(200)),
    );
    let command: Vec<String> = ["sh", "-c", "cat > /dev/null"].map(String::from).into();
    std::thread::spawn(move || {
        let _keep_reader_open = proxy_r;
        serve(client_r, client_w, &command, mode)
    });
    Running {
        _to_proxy: to_proxy,
    }
}

fn repo() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("app")).unwrap();
    std::fs::write(t.path().join("MODULE.bazel"), "").unwrap();
    std::fs::write(t.path().join("app/BUILD.bazel"), "").unwrap();
    std::fs::write(t.path().join("app/App.kt"), "").unwrap();
    t
}

#[test]
fn build_edits_sync_once_and_kotlin_edits_never() {
    let t = repo();
    let syncer = Arc::new(CountingSyncer {
        runs: Mutex::new(0),
        module_dirs: vec![],
    });
    let _proxy = start(t.path(), syncer.clone());
    syncer.wait_for(1);
    let base = syncer.settled();

    std::fs::write(t.path().join("app/App.kt"), "package app\n").unwrap();
    assert_eq!(syncer.settled(), base, "a Kotlin edit synced");

    for text in ["# 1\n", "# 2\n", "# 3\n"] {
        std::fs::write(t.path().join("app/BUILD.bazel"), text).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    syncer.wait_for(base + 1);
    assert_eq!(syncer.settled(), base + 1, "a burst of BUILD edits");
}

#[test]
fn module_folders_outside_the_root_are_watched_after_a_sync() {
    let t = repo();
    let module = tempfile::tempdir().unwrap();
    std::fs::write(module.path().join("BUILD.bazel"), "").unwrap();
    let syncer = Arc::new(CountingSyncer {
        runs: Mutex::new(0),
        module_dirs: vec![module.path().to_path_buf(), t.path().join("app")],
    });
    let _proxy = start(t.path(), syncer.clone());
    syncer.wait_for(1);
    let base = syncer.settled();

    std::fs::write(module.path().join("BUILD.bazel"), "# edited\n").unwrap();
    syncer.wait_for(base + 1);
}
