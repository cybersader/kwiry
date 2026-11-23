//! Embed the build identity that `engine` reports. Never fails: without git or
//! a commit in the environment, the commit is reported as unknown.
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=KWIRY_GIT_COMMIT");
    let commit = ["KWIRY_GIT_COMMIT", "GIT_COMMIT"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .or_else(git_head)
        .map(|commit| commit.trim().to_ascii_lowercase())
        .filter(|commit| commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default();
    println!("cargo:rustc-env=KWIRY_BUILD_COMMIT={commit}");
    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    println!("cargo:rustc-env=KWIRY_BUILD_ID={target}/{profile}");
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_owned())
}

fn git_head() -> Option<String> {
    // Rebuild when this checkout moves to another commit.
    let git_dir = git(&["rev-parse", "--absolute-git-dir"])?;
    for file in ["HEAD", "logs/HEAD"] {
        let path = Path::new(&git_dir).join(file);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    git(&["rev-parse", "HEAD"])
}
