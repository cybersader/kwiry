//! Stop only the requesting thread; watchers, publisher, fresh requests and the
//! runtime pool keep running. Two equivalent barriers drive one scenario: a
//! debugger breakpoint on the debug build (no GDB-specific modification; the
//! inert stall point is compiled in under `cargo test`), and the test-only stall
//! hook (`internal-stall-hook-test` feature, debug assertions, explicit env
//! var). No production pause switch, HTTP hook, elapsed-delay assumption, or
//! test signer bypass.
use super::*;

/// Holds exactly one search between opening its reader and searching it.
trait Barrier {
    fn pause(&mut self, lab: &Lab, q: &str) -> thread::JoinHandle<Value>;
    fn release(&mut self, call: thread::JoinHandle<Value>) -> Value;
}

fn pinned_search(lab: &Lab, q: &str) -> thread::JoinHandle<Value> {
    let addr = lab.address.clone();
    let body = page(q, 20);
    let headers = format!("{}{PROFILE}{MEDIA}", lab.auth(&[R], json!(20)));
    thread::spawn(move || request_at(&addr, "POST", SEARCH, &headers, &body))
}

fn until(root: &Path, mut predicate: impl FnMut() -> bool, diagnostics: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "synthetic reader barrier not reached in {}: {}",
        root.display(),
        diagnostics()
    );
}

struct Debugger {
    child: Child,
    root: PathBuf,
}
impl Debugger {
    fn start(lab: &mut Lab) -> Self {
        lab.stop();
        lab.config.vaults[0].snapshot_id = Some("c".repeat(64).parse().unwrap());
        save_config(&lab.path, &lab.config).unwrap();
        let root = lab._root.path();
        fs::write(root.join("barrier.gdb"),r#"set confirm off
set pagination off
set non-stop on
set disable-randomization off
break kwiry_core::runtime::search_opened_with_quality
disable 1
commands 1
silent
disable 1
shell touch pinned
end
run --config control/config.toml --data-dir index-data serve --bind 127.0.0.1:0 > daemon.out 2> daemon.err &
"#).unwrap();
        let child = Command::new("gdb")
            .args([
                "-nx",
                "-nh",
                "-quiet",
                "-iex",
                "set auto-load off",
                "-iex",
                "set debuginfod enabled off",
                "-x",
                "barrier.gdb",
                "--args",
            ])
            .arg(cargo_bin("kwiry"))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(fs::File::create(root.join("debug.out")).unwrap())
            .stderr(fs::File::create(root.join("debug.err")).unwrap())
            .spawn()
            .unwrap();
        let dbg = Self {
            child,
            root: root.to_owned(),
        };
        dbg.until(|| {
            fs::read_to_string(root.join("daemon.out"))
                .is_ok_and(|s| s.contains("listening on http://"))
        });
        lab.address = fs::read_to_string(root.join("daemon.out"))
            .unwrap()
            .split("http://")
            .nth(1)
            .unwrap()
            .trim()
            .into();
        lab.at = format_release_token_time(&lab.data);
        dbg
    }
    fn until(&self, predicate: impl FnMut() -> bool) {
        until(&self.root, predicate, || {
            format!(
                "{} {}",
                fs::read_to_string(self.root.join("debug.out")).unwrap_or_default(),
                fs::read_to_string(self.root.join("debug.err")).unwrap_or_default()
            )
        });
    }
    fn cmd(&mut self, cmd: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{cmd}").unwrap();
    }
}
impl Barrier for Debugger {
    fn pause(&mut self, lab: &Lab, q: &str) -> thread::JoinHandle<Value> {
        self.cmd("enable 1\nshell touch armed");
        self.until(|| self.root.join("armed").exists());
        fs::remove_file(self.root.join("armed")).unwrap();
        let call = pinned_search(lab, q);
        self.until(|| self.root.join("pinned").exists());
        fs::remove_file(self.root.join("pinned")).unwrap();
        call
    }
    fn release(&mut self, call: thread::JoinHandle<Value>) -> Value {
        self.cmd("continue -a&");
        call.join().unwrap()
    }
}
impl Drop for Debugger {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = writeln!(stdin, "quit");
        }
        let _ = self.child.wait();
    }
}

#[test]
fn governed_search_live_check_stalled_answering_reader() {
    if !Command::new("gdb")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        assert!(
            std::env::var("KWIRY_REQUIRE_LIVE_BARRIER").as_deref() != Ok("1"),
            "gdb required for pinned-reader acceptance"
        );
        eprintln!("SKIP pinned-reader process fixture: gdb unavailable");
        return;
    }
    let mut lab = Lab::new();
    let mut dbg = Debugger::start(&mut lab);
    stalled_answering_reader(&mut lab, &mut dbg);
}

/// Shared by both barriers so each must produce the same recorded wire family.
fn stalled_answering_reader(lab: &mut Lab, dbg: &mut impl Barrier) {
    let mut cap = Capture::new();
    for (case, old_text, q, new_text, same_root) in [
        ("byte-identical-hits", true, "moonflower", true, false),
        ("byte-identical-zero", true, "zzzzzzzz", true, false),
        ("empty-root", false, "moonflower", false, false),
        ("label-only", true, "moonflower", true, true),
    ] {
        let old = lab._root.path().join(format!("{case}-a"));
        fs::create_dir(&old).unwrap();
        if old_text {
            sealed_note(&old);
        }
        lab.config.vaults[0].path = old;
        lab.config.vaults[0].snapshot_id = Some(LABEL_A.parse().unwrap());
        save_config(&lab.path, &lab.config).unwrap();
        wait_for(|| lab.query(q)["body"]["resource_snapshots"][0]["snapshot_id"] == LABEL_A);
        let call = dbg.pause(lab, q);
        if !same_root {
            let new = lab._root.path().join(format!("{case}-b"));
            fs::create_dir(&new).unwrap();
            if new_text {
                sealed_note(&new);
            }
            lab.config.vaults[0].path = new;
        }
        lab.config.vaults[0].snapshot_id = Some(LABEL_B.parse().unwrap());
        save_config(&lab.path, &lab.config).unwrap();
        wait_for(|| lab.query(q)["body"]["resource_snapshots"][0]["snapshot_id"] == LABEL_B);
        let old = dbg.release(call);
        assert_eq!(old["body"]["resource_snapshots"][0]["snapshot_id"], LABEL_A);
        assert_eq!(
            old["body"]["hits"].as_array().unwrap().is_empty(),
            !old_text || q == "zzzzzzzz"
        );
        cap.check(case, old, 200, "", true);
    }
    let call = dbg.pause(lab, "moonflower");
    // Publish the same tuple with a generic (not live-check) label. A fresh
    // indexed refusal distinguishes publication from the closed reload gate.
    lab.config.vaults[0].snapshot_id = Some("generic-synthetic".parse().unwrap());
    save_config(&lab.path, &lab.config).unwrap();
    wait_for(|| lab.query("moonflower")["body"]["error"]["resource_indexes"] == json!([0]));
    let reply = dbg.release(call);
    assert_eq!(reply["body"]["error"]["resource_indexes"], json!([0]));
    cap.check(
        "final-ineligible-binding",
        reply,
        503,
        "resource_unavailable",
        true,
    );
    lab.config.vaults[0].snapshot_id = Some(LABEL_B.parse().unwrap());
    save_config(&lab.path, &lab.config).unwrap();
    wait_for(|| lab.query("moonflower")["body"]["resource_snapshots"][0]["snapshot_id"] == LABEL_B);
    let call = dbg.pause(lab, "moonflower");
    let removed = lab.config.vaults.pop().unwrap();
    save_config(&lab.path, &lab.config).unwrap();
    wait_for(|| lab.query("moonflower")["body"]["error"]["resource_indexes"] == json!([0]));
    let reply = dbg.release(call);
    assert_eq!(reply["body"]["error"]["resource_indexes"], json!([0]));
    cap.check(
        "final-resource-refusal",
        reply,
        503,
        "resource_unavailable",
        true,
    );
    lab.config.vaults.push(removed);
    save_config(&lab.path, &lab.config).unwrap();
    wait_for(|| lab.query("moonflower")["status"] == 200);
    let call = dbg.pause(lab, "moonflower");
    fs::write(&lab.path, "[invalid").unwrap();
    wait_for(|| lab.post(&[R], PROFILE, MEDIA, b"{")["status"] == 503);
    let reply = dbg.release(call);
    assert!(reply["body"]["error"].get("resource_indexes").is_none());
    cap.check(
        "final-global-refusal",
        reply,
        503,
        "resource_unavailable",
        true,
    );
    cap.save("stalled-reader");
}

/// The same barrier with no debugger: the reader path's test-only stall point.
/// The hook exists only with the feature and debug assertions; the variable
/// below is inert in any other build.
#[cfg(debug_assertions)]
struct StallHook {
    dir: PathBuf,
}
#[cfg(debug_assertions)]
impl StallHook {
    fn start(lab: &mut Lab) -> Self {
        lab.stop();
        lab.config.vaults[0].snapshot_id = Some("c".repeat(64).parse().unwrap());
        let dir = lab._root.path().join("stall");
        fs::create_dir(&dir).unwrap();
        let env = dir.clone();
        lab.start_with(move |cmd| {
            cmd.env(kwiry_core::stall_hook::STALL_DIR_ENV, env);
        });
        Self { dir }
    }
}
#[cfg(debug_assertions)]
impl Barrier for StallHook {
    fn pause(&mut self, lab: &Lab, q: &str) -> thread::JoinHandle<Value> {
        fs::write(self.dir.join("arm"), b"").unwrap();
        let call = pinned_search(lab, q);
        let pinned = self.dir.join("pinned");
        until(&self.dir, || pinned.exists(), String::new);
        fs::remove_file(pinned).unwrap();
        call
    }
    fn release(&mut self, call: thread::JoinHandle<Value>) -> Value {
        fs::write(self.dir.join("release"), b"").unwrap();
        call.join().unwrap()
    }
}

#[cfg(debug_assertions)]
#[test]
fn governed_search_live_check_stalled_answering_reader_via_stall_hook() {
    let mut lab = Lab::new();
    let mut hook = StallHook::start(&mut lab);
    stalled_answering_reader(&mut lab, &mut hook);
}

/// Run by CI's release-binary step: the stall point is absent from a release
/// build, so an armed directory never pauses a search.
#[cfg(not(debug_assertions))]
#[test]
fn format_release_binary_has_no_reader_stall_point() {
    let mut lab = Lab::new();
    lab.stop();
    let dir = lab._root.path().join("stall");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("arm"), b"").unwrap();
    let env = dir.clone();
    lab.start_with(move |cmd| {
        cmd.env("KWIRY_TEST_STALL_READER_DIR", env);
    });
    assert_eq!(lab.query("moonflower")["status"], 200);
    assert!(dir.join("arm").exists());
    assert!(!dir.join("pinned").exists());
    lab.stop();
}
