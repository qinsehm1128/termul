use std::fs;
use std::sync::Arc;

use serde_json::Value;

use super::{CrashInjector, CrashPoint, DurableFileSystem, NamespaceState, OwnedTempDisposition};

#[derive(Debug)]
struct InterruptAt(CrashPoint);

impl CrashInjector for InterruptAt {
    fn should_interrupt(&self, point: CrashPoint) -> bool {
        point == self.0
    }
}

fn injected(point: CrashPoint) -> DurableFileSystem {
    DurableFileSystem::with_crash_injector(Arc::new(InterruptAt(point)))
}

fn generation(path: &std::path::Path) -> String {
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    value["generation"].as_str().unwrap().to_string()
}

#[test]
fn native_replace_recovery_matrix() {
    let platform = std::env::consts::OS;
    assert!(matches!(platform, "linux" | "macos" | "windows"));

    for point in [
        CrashPoint::BeforeTempSync,
        CrashPoint::AfterTempSync,
        CrashPoint::AfterReplace,
        CrashPoint::AfterNamespaceSync,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let target = root.join("conversation-layout.json");
        fs::write(&target, br#"{"generation":"old"}"#).unwrap();
        let source_before = fs::read(&target).unwrap();
        let outcome = injected(point)
            .replace_bytes(&target, br#"{"generation":"new"}"#)
            .unwrap();

        match point {
            CrashPoint::BeforeTempSync | CrashPoint::AfterTempSync => {
                assert_eq!(outcome.namespace_state, NamespaceState::OldComplete);
                assert_eq!(generation(&target), "old");
                assert_eq!(fs::read(&target).unwrap(), source_before);
            }
            CrashPoint::AfterReplace | CrashPoint::AfterNamespaceSync => {
                assert_eq!(outcome.namespace_state, NamespaceState::NewComplete);
                assert_eq!(generation(&target), "new");
            }
            CrashPoint::AfterJsonlAppend => unreachable!(),
        }

        let restart = DurableFileSystem::new()
            .replace_bytes(&target, br#"{"generation":"recovered"}"#)
            .unwrap();
        assert_eq!(restart.namespace_state, NamespaceState::NewComplete);
        assert_eq!(restart.crash_point, CrashPoint::AfterNamespaceSync);
        assert_eq!(generation(&target), "recovered");
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name == "conversation-layout.json" || name.ends_with(".tmp")
        }));
    }
}

#[test]
fn native_workspace_revision_and_jsonl_restart_matrix() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace.json");
    fs::write(&workspace, br#"{"revision":1}"#).unwrap();

    let old = injected(CrashPoint::AfterTempSync)
        .replace_bytes(&workspace, br#"{"revision":2}"#)
        .unwrap();
    assert_eq!(old.namespace_state, NamespaceState::OldComplete);
    assert_eq!(
        old.owned_temp_disposition,
        OwnedTempDisposition::RetainedForRetry
    );
    let value: Value = serde_json::from_slice(&fs::read(&workspace).unwrap()).unwrap();
    assert_eq!(value["revision"], 1);

    let new = injected(CrashPoint::AfterReplace)
        .replace_bytes(&workspace, br#"{"revision":2}"#)
        .unwrap();
    assert_eq!(new.namespace_state, NamespaceState::NewComplete);
    let value: Value = serde_json::from_slice(&fs::read(&workspace).unwrap()).unwrap();
    assert_eq!(value["revision"], 2);

    let log = root.join("messages.jsonl");
    let appended = injected(CrashPoint::AfterJsonlAppend)
        .append_jsonl(&log, br#"{"schemaVersion":2,"seq":1}"#)
        .unwrap();
    assert_eq!(appended.crash_point, CrashPoint::AfterJsonlAppend);
    assert_eq!(
        fs::read(&log).unwrap(),
        b"{\"schemaVersion\":2,\"seq\":1}\n"
    );
    DurableFileSystem::new()
        .sync_file_and_namespace(&log)
        .unwrap();
}

#[test]
fn native_platform_claim_is_explicit_and_never_ignored() {
    #[cfg(target_os = "linux")]
    assert_eq!(std::env::consts::OS, "linux");
    #[cfg(target_os = "macos")]
    assert_eq!(std::env::consts::OS, "macos");
    #[cfg(target_os = "windows")]
    assert_eq!(std::env::consts::OS, "windows");

    let source = include_str!("durable_fs.rs");
    #[cfg(target_os = "linux")]
    {
        assert!(source.contains("File::open(parent)?.sync_all()"));
        assert!(source.contains("fs::rename(source, target)"));
    }
    #[cfg(target_os = "macos")]
    {
        assert!(source.contains("libc::F_FULLFSYNC"));
        assert!(source.contains("File::open(parent)?.sync_all()"));
    }
    #[cfg(target_os = "windows")]
    {
        assert!(source.contains("MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH"));
        assert!(source.contains("Windows has no portable parent-directory fsync"));
    }
}
