use std::process::Command;

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();

    // Check if web-frontend feature is enabled
    let has_web_frontend = std::env::var("CARGO_FEATURE_WEB_FRONTEND").is_ok();

    // Only build frontend for release builds with web-frontend feature
    if profile == "release" && has_web_frontend {
        println!("cargo:warning=Building frontend assets for release...");

        // Check if npm is available
        let npm_check = Command::new("npm")
            .arg("--version")
            .output();

        if npm_check.is_err() {
            println!("cargo:warning=npm not found. Skipping frontend build.");
            println!("cargo:warning=Install Node.js and npm to build frontend assets.");
            return;
        }

        // Run npm install
        let install_status = Command::new("npm")
            .current_dir("app")
            .arg("install")
            .status();

        match install_status {
            Ok(status) if status.success() => {
                println!("cargo:warning=npm install completed");
            }
            _ => {
                println!("cargo:warning=npm install failed. Frontend may not build correctly.");
            }
        }

        // Run npm run build
        let build_status = Command::new("npm")
            .current_dir("app")
            .arg("run")
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
                println!("cargo:warning=Failed to run npm build: {}", e);
                panic!("Frontend build failed: {}", e);
            }
        }
    }

    // Tell cargo to rerun if app files change
    println!("cargo:rerun-if-changed=app/");
    println!("cargo:rerun-if-changed=app/src/");
    println!("cargo:rerun-if-changed=app/index.html");
    println!("cargo:rerun-if-changed=app/vite.config.ts");
    println!("cargo:rerun-if-changed=app/package.json");
}
