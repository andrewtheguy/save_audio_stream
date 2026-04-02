use std::fs;
use std::process::Command;

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();

    // Only build frontend for release builds
    if profile == "release" {
        println!("cargo:warning=Building frontend assets for release...");

        // Check if bun is available
        let bun_check = Command::new("bun").arg("--version").output();

        if bun_check.is_err() {
            panic!("bun not found. Install Bun to build frontend assets: https://bun.sh");
        }

        // Run bun run build
        let build_status = Command::new("bun")
            .current_dir("frontend")
            .arg("run")
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
                println!("cargo:warning=Failed to run bun build: {}", e);
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
