use std::fs;
use std::process::Command;

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();

    // Check if web-frontend feature is enabled
    let has_web_frontend = std::env::var("CARGO_FEATURE_WEB_FRONTEND").is_ok();

    // Only build frontend for release builds with web-frontend feature
    if profile == "release" && has_web_frontend {
        println!("cargo:warning=Building frontend assets for release...");

        // Check if deno is available
        let deno_check = Command::new("deno").arg("--version").output();

        if deno_check.is_err() {
            panic!("deno not found. Install Deno to build frontend assets: https://deno.land");
        }

        // Run deno task build
        let build_status = Command::new("deno")
            .current_dir("frontend")
            .arg("task")
            .arg("build")
            .status();

        match build_status {
            Ok(status) if status.success() => {
                println!("cargo:warning=Frontend build completed successfully");
            }
            Ok(status) => {
                println!(
                    "cargo:warning=Frontend build failed with status: {}",
                    status
                );
                panic!("Frontend build failed");
            }
            Err(e) => {
                println!("cargo:warning=Failed to run deno build: {}", e);
                panic!("Frontend build failed: {}", e);
            }
        }
        // Tell cargo to rerun if app files change (excluding node_modules and dist)
        let exclude = ["node_modules", "dist"];
        //let mut debug_output = String::new();
        if let Ok(entries) = fs::read_dir("frontend") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !exclude.contains(&name.to_string_lossy().as_ref()) {
                    let path = entry.path();
                    println!("cargo:rerun-if-changed={}", path.display());
                    //debug_output.push_str(&format!("{}\n", path.display()));
                }
            }
        }
        //fs::write("build_debug.txt", debug_output).ok();
    }

}
