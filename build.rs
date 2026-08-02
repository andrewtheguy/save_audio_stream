use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    // Declared before the CI early-return, so the opt-out changes what is built
    // and not what Cargo watches — a CI build and a local build agree on when
    // this script is stale.
    println!("cargo:rerun-if-env-changed=CI");

    // Explicit inputs rather than a read_dir sweep of frontend/: Cargo re-stats
    // these paths whether or not they exist, a listing only notices files that
    // are there now, and neither frontend/dist nor node_modules can creep in and
    // make the script retrigger on its own output. frontend/src is a directory,
    // which Cargo walks recursively.
    for path in [
        "frontend/src",
        "frontend/index.html",
        "frontend/package.json",
        "frontend/bun.lock",
        "frontend/tsconfig.json",
        "frontend/vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    // CI builds the bundle once, in its own job, and hands every target the same
    // artifact (.github/workflows/release.yml) — so there is nothing to build
    // here, and no reason for bun to be installed on the runner.
    if env::var("CI").is_ok_and(|value| value == "true") {
        return;
    }

    // Absolute, from CARGO_MANIFEST_DIR: a build script's working directory is
    // Cargo's business, not something to hang a relative path on.
    let frontend_dir = Path::new(&env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("frontend");
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status()
        .expect("failed to run `bun run build` for the frontend — install Bun: https://bun.sh");

    assert!(status.success(), "`bun run build` for the frontend failed");
}
