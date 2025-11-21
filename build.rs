use std::process::Command;

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();

    // Check if web-frontend feature is enabled
    let has_web_frontend = std::env::var("CARGO_FEATURE_WEB_FRONTEND").is_ok();

    // Only build frontend for release builds with web-frontend feature
    if profile == "release" && has_web_frontend {
        println!("cargo:warning=Building frontend assets for release...");

        // Check if deno is available
        let deno_check = Command::new("deno")
            .arg("--version")
            .output();

        if deno_check.is_err() {
            panic!("deno not found. Install Deno to build frontend assets: https://deno.land");
        }

        // Run deno task build
        let build_status = Command::new("deno")
            .current_dir("app")
            .arg("task")
            .arg("build")
            .status();

        match build_status {
            Ok(status) if status.success() => {
                println!("cargo:warning=Frontend build completed successfully");
            }
            Ok(status) => {
                println!("cargo:warning=Frontend build failed with status: {}", status);
                panic!("Frontend build failed");
            }
            Err(e) => {
                println!("cargo:warning=Failed to run deno build: {}", e);
                panic!("Frontend build failed: {}", e);
            }
        }
    }

    // Tell cargo to rerun if app files change
    println!("cargo:rerun-if-changed=app/");
    println!("cargo:rerun-if-changed=app/src/");
    println!("cargo:rerun-if-changed=app/index.html");
    println!("cargo:rerun-if-changed=app/deno.json");
    println!("cargo:rerun-if-changed=app/build.ts");
    println!("cargo:rerun-if-changed=app/deps.ts");
}
