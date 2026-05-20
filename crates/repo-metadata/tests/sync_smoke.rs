//! End-to-end smoke: serve the synthetic fixture over a local HTTP
//! server, run the rpm-md backend through HttpCache against it,
//! verify revision + package count + snapshot persistence.
//!
//! Does NOT touch the actual `repo sync` CLI command — that's tested
//! via `crates/cli/tests/cli.rs`. This test exercises the
//! `repo-metadata` crate directly so failures are attributed
//! correctly.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rpm_spec_repo_metadata::backend::{detect_backend, RepoBackend};
use rpm_spec_repo_metadata::cache::{self, CacheDirs};
use rpm_spec_repo_metadata::http::{HttpCache, NetMode};
use rpm_spec_profile::repos::{RepoConfig, RepoKind};

/// Project-root-relative path to the fixture directory.
fn fixture_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

/// Spawn a single-threaded HTTP server that serves files from
/// `fixture_dir()/tests/fixtures/repos/rpm-md/tiny-fedora/`. Returns
/// (port, shutdown_sender). The thread exits when the receiver
/// returns Err (sender dropped).
fn spawn_http_server() -> (u16, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener
        .set_nonblocking(true)
        .expect("set_nonblocking");
    let port = listener.local_addr().unwrap().port();
    let root = fixture_dir().to_path_buf();
    let (tx, rx) = mpsc::channel::<()>();

    thread::spawn(move || {
        let serving_root = root.join("tests/fixtures/repos/rpm-md/tiny-fedora");
        loop {
            if rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut sock, _)) => {
                    sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    let mut buf = [0u8; 4096];
                    let n = match sock.read(&mut buf) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .trim_start_matches('/');
                    let full = serving_root.join(path);
                    match std::fs::read(&full) {
                        Ok(body) => {
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(header.as_bytes());
                            let _ = sock.write_all(&body);
                        }
                        Err(_) => {
                            let _ = sock
                                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    (port, tx)
}

#[test]
fn sync_fixture_through_http_cache() {
    let (port, _shutdown) = spawn_http_server();
    let baseurl = format!("http://127.0.0.1:{port}/");

    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = CacheDirs::ensure(tmp.path().to_path_buf()).expect("ensure dirs");
    let http = HttpCache::new(tmp.path().to_path_buf(), NetMode::Online).expect("http");

    let mut cfg = RepoConfig::default();
    cfg.baseurl = Some(baseurl.clone());
    cfg.kind = RepoKind::RpmMd;
    cfg.enabled = true;

    let backend: Box<dyn RepoBackend> = detect_backend(&cfg).expect("backend");
    let rev = backend
        .fetch_revision(&http, &baseurl)
        .expect("fetch_revision");
    assert!(!rev.id.is_empty(), "revision id should be set");

    // Test rig has no profile-level TOML key, but production paths
    // always pass a `[a-z0-9_-]{1,64}` slug — use one here so the
    // resulting cache opens successfully on the asserted-shape side
    // (see `RepoDb::open`'s `validate_repo_id` check).
    let repo_id = rpm_spec_repo_core::RepoId::from("smoke");
    let index = backend
        .fetch_index(&http, &baseurl, &rev, &repo_id)
        .expect("fetch_index");
    assert_eq!(index.packages.len(), 3, "tiny-fedora has 3 packages");

    let snap = cache::write_snapshot(
        &dirs,
        &baseurl,
        backend.kind(),
        &index,
        rev.raw_bytes.len() as u64,
    )
    .expect("write_snapshot");
    assert!(snap.join("manifest.json").exists());
    assert!(
        snap.join(rpm_spec_repo_core::db::RepoDb::file_name()).exists(),
        "repo.db should be present after write_snapshot"
    );

    // Reload from disk and verify the SQLite mirror carries the same
    // package count as the in-memory index.
    let db_path = snap.join(rpm_spec_repo_core::db::RepoDb::file_name());
    let reloaded = rpm_spec_repo_core::db::RepoDb::open(&db_path).expect("reopen repo.db");
    assert_eq!(reloaded.package_count().expect("count"), 3);
}
