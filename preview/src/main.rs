use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::services::{ServeDir, ServeFile};
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

const DEFAULT_PORT: u16 = 8765;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;
    let config = PreviewConfig::discover()?;

    perform_full_build(&config)?;
    if args.build_only {
        let about = collect_about(
            &config,
            &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        )?;
        println!(
            "Built preview assets. Bundle: {}",
            config.bundle_path().display()
        );
        println!(
            "Freshness checks: {} total, {} stale",
            about.checks.len(),
            about.checks.iter().filter(|check| check.stale).count()
        );
        return Ok(());
    }

    let state = Arc::new(AppState {
        config: config.clone(),
        build_lock: Arc::new(Mutex::new(())),
        started_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    });

    let app = Router::new()
        .route("/", get(|| async { Redirect::temporary("/preview/") }))
        .route("/preview/", get(preview_redirect))
        .route("/preview/repo/playlist.json", get(preview_playlist))
        .route("/api/about", get(api_about))
        .route("/api/rebuild", post(api_rebuild))
        .route_service(
            "/bundle/affirmations.vzglyd",
            ServeFile::new(config.bundle_path()),
        )
        .route_service(
            "/preview/repo/affirmations.vzglyd",
            ServeFile::new(config.bundle_path()),
        )
        .nest_service(
            "/vendor/web-preview",
            ServeDir::new(config.runtime_web_preview_dir()),
        )
        .with_state(state);

    let address = SocketAddr::new(args.host, args.port);
    println!("Air quality preview available at http://{address}/preview/");

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind preview server at {address}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("preview server failed")?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn api_about(State(state): State<Arc<AppState>>) -> Result<Json<AboutResponse>, ApiError> {
    let config = state.config.clone();
    let started_at_utc = state.started_at_utc.clone();
    let build_lock = Arc::clone(&state.build_lock);
    let about = tokio::task::spawn_blocking(move || {
        let _guard = build_lock.lock().expect("preview build lock poisoned");
        collect_about(&config, &started_at_utc)
    })
    .await
    .map_err(|error| ApiError(anyhow!("about worker failed: {error}")))??;
    Ok(Json(about))
}

async fn api_rebuild(State(state): State<Arc<AppState>>) -> Result<Json<AboutResponse>, ApiError> {
    let config = state.config.clone();
    let started_at_utc = state.started_at_utc.clone();
    let build_lock = Arc::clone(&state.build_lock);
    let about = tokio::task::spawn_blocking(move || {
        let _guard = build_lock.lock().expect("preview build lock poisoned");
        perform_full_build(&config)?;
        collect_about(&config, &started_at_utc)
    })
    .await
    .map_err(|error| ApiError(anyhow!("rebuild worker failed: {error}")))??;
    Ok(Json(about))
}

async fn preview_redirect(raw_query: RawQuery) -> Redirect {
    let mut query = raw_query.0.unwrap_or_default();
    if query.is_empty() {
        query.push_str("repo=/preview/repo/");
    } else if !query.split('&').any(|pair| pair.starts_with("repo=")) {
        query.push_str("&repo=/preview/repo/");
    }
    Redirect::temporary(&format!("/vendor/web-preview/view.html?{query}"))
}

async fn preview_playlist() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": 1,
        "slides": [
            {
                "path": "affirmations.vzglyd",
                "enabled": true,
            }
        ]
    }))
}

#[derive(Clone)]
struct AppState {
    config: PreviewConfig,
    build_lock: Arc<Mutex<()>>,
    started_at_utc: String,
}

#[derive(Clone)]
struct PreviewConfig {
    repo_root: PathBuf,
    runtime_source_web_root: PathBuf,
    runtime_source_kernel_root: PathBuf,
    runtime_source_slide_root: PathBuf,
    runtime_build_root: PathBuf,
}

impl PreviewConfig {
    fn discover() -> Result<Self> {
        let preview_crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = preview_crate_root
            .parent()
            .map(Path::to_path_buf)
            .context("preview crate must live inside the repo root")?;
        let workspace_root = repo_root
            .parent()
            .map(Path::to_path_buf)
            .context("repo root must have a workspace parent")?;
        let runtime_source_web_root = workspace_root.join("VRX-64-web");
        let runtime_source_kernel_root = workspace_root.join("VRX-64-kernel");
        let runtime_source_slide_root = workspace_root.join("VRX-64-slide");
        let runtime_build_root = std::env::temp_dir().join("lume-affirmations-preview");

        for required in [
            repo_root.join("Cargo.toml"),
            repo_root.join("sidecar/Cargo.toml"),
            runtime_source_web_root.join("Cargo.toml"),
            runtime_source_kernel_root.join("Cargo.toml"),
            runtime_source_slide_root.join("Cargo.toml"),
        ] {
            if !required.is_file() {
                bail!("required path is missing: {}", required.display());
            }
        }

        Ok(Self {
            repo_root,
            runtime_source_web_root,
            runtime_source_kernel_root,
            runtime_source_slide_root,
            runtime_build_root,
        })
    }

    fn bundle_path(&self) -> PathBuf {
        self.repo_root.join("affirmations.vzglyd")
    }

    fn slide_output_path(&self) -> PathBuf {
        self.repo_root.join("affirmations_slide.wasm")
    }

    fn sidecar_output_path(&self) -> PathBuf {
        self.repo_root.join("sidecar.wasm")
    }

    fn manifest_path(&self) -> PathBuf {
        self.repo_root.join("affirmations_slide.json")
    }

    fn assets_dir(&self) -> PathBuf {
        self.repo_root.join("assets")
    }

    fn runtime_live_root(&self) -> PathBuf {
        self.runtime_build_root.join("live")
    }

    fn runtime_web_preview_dir(&self) -> PathBuf {
        self.runtime_live_root()
            .join("VRX-64-web")
            .join("web-preview")
    }

    fn runtime_pkg_dir(&self) -> PathBuf {
        self.runtime_web_preview_dir().join("pkg")
    }
}

#[derive(Debug)]
struct Args {
    host: IpAddr,
    port: u16,
    build_only: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut host = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let mut port = DEFAULT_PORT;
        let mut build_only = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--host" => {
                    let value = args.next().context("--host requires an IP address")?;
                    host = value
                        .parse()
                        .with_context(|| format!("invalid host value '{value}'"))?;
                }
                "--port" => {
                    let value = args.next().context("--port requires a number")?;
                    port = value
                        .parse()
                        .with_context(|| format!("invalid port value '{value}'"))?;
                }
                "--build-only" => build_only = true,
                "--help" | "-h" => {
                    println!(
                        "Usage: cargo run -p affirmations-preview -- [--host 127.0.0.1] [--port 8765] [--build-only]"
                    );
                    std::process::exit(0);
                }
                other => bail!("unrecognized argument '{other}'"),
            }
        }

        Ok(Self {
            host,
            port,
            build_only,
        })
    }
}

fn perform_full_build(config: &PreviewConfig) -> Result<()> {
    build_slide_wasm(config)?;
    build_sidecar_wasm(config)?;
    package_bundle(config)?;
    ensure_runtime_preview(config)?;
    Ok(())
}

fn build_slide_wasm(config: &PreviewConfig) -> Result<()> {
    run_command(
        Command::new("cargo")
            .arg("build")
            .arg("-p")
            .arg("affirmations_slide")
            .arg("--target")
            .arg("wasm32-wasip1")
            .arg("--release")
            .current_dir(&config.repo_root),
        "build slide wasm",
    )?;

    let source = config
        .repo_root
        .join("target/wasm32-wasip1/release/affirmations_slide.wasm");
    copy_file(&source, &config.slide_output_path())?;
    Ok(())
}

fn build_sidecar_wasm(config: &PreviewConfig) -> Result<()> {
    run_command(
        Command::new("cargo")
            .arg("build")
            .arg("-p")
            .arg("affirmations-sidecar")
            .arg("--target")
            .arg("wasm32-wasip1")
            .arg("--release")
            .current_dir(&config.repo_root),
        "build sidecar wasm",
    )?;

    let source = config
        .repo_root
        .join("target/wasm32-wasip1/release/affirmations-sidecar.wasm");
    copy_file(&source, &config.sidecar_output_path())?;
    Ok(())
}

fn package_bundle(config: &PreviewConfig) -> Result<()> {
    let bundle_path = config.bundle_path();
    let file = File::create(&bundle_path)
        .with_context(|| format!("failed to create bundle '{}'", bundle_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    add_file_to_zip(&mut zip, &config.manifest_path(), "manifest.json", options)?;
    add_file_to_zip(&mut zip, &config.slide_output_path(), "slide.wasm", options)?;
    add_file_to_zip(
        &mut zip,
        &config.sidecar_output_path(),
        "sidecar.wasm",
        options,
    )?;

    for entry in WalkDir::new(config.assets_dir()) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&config.repo_root)
            .context("asset path should stay inside repo root")?;
        let internal = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        add_file_to_zip(&mut zip, entry.path(), &internal, options)?;
    }

    zip.finish().context("failed to finalize bundle archive")?;
    Ok(())
}

fn ensure_runtime_preview(config: &PreviewConfig) -> Result<()> {
    let dependency_files = runtime_dependency_files(config)?;
    let output_files = runtime_output_paths(config);
    let needs_rebuild = output_files.iter().any(|path| !path.is_file())
        || outputs_are_stale(&dependency_files, &output_files)?;

    if !needs_rebuild {
        return Ok(());
    }

    fs::create_dir_all(&config.runtime_build_root).with_context(|| {
        format!(
            "failed to create runtime build root '{}'",
            config.runtime_build_root.display()
        )
    })?;

    let staging_root = config.runtime_build_root.join("staging");
    remove_dir_if_exists(&staging_root)?;
    fs::create_dir_all(&staging_root)
        .with_context(|| format!("failed to create staging dir '{}'", staging_root.display()))?;

    let copied_web_root = staging_root.join("VRX-64-web");
    let copied_kernel_root = staging_root.join("VRX-64-kernel");
    let copied_slide_root = staging_root.join("VRX-64-slide");
    copy_tree(&config.runtime_source_web_root, &copied_web_root)?;
    copy_tree(&config.runtime_source_kernel_root, &copied_kernel_root)?;
    copy_tree(&config.runtime_source_slide_root, &copied_slide_root)?;

    remove_dir_if_exists(&copied_web_root.join("web-preview/pkg"))?;

    run_command(
        Command::new("wasm-pack")
            .arg("build")
            .arg("--dev")
            .arg("--mode")
            .arg("no-install")
            .arg("--target")
            .arg("web")
            .arg("--out-dir")
            .arg("web-preview/pkg")
            .env(
                "CARGO_TARGET_DIR",
                config.runtime_build_root.join("cargo-target"),
            )
            .current_dir(&copied_web_root),
        "build browser runtime",
    )?;

    patch_wasm_bindgen_bridge_import(&copied_web_root.join("web-preview/pkg"))?;

    let live_root = config.runtime_live_root();
    remove_dir_if_exists(&live_root)?;
    fs::rename(&staging_root, &live_root).with_context(|| {
        format!(
            "failed to promote runtime staging '{}' to '{}'",
            staging_root.display(),
            live_root.display()
        )
    })?;

    Ok(())
}

fn patch_wasm_bindgen_bridge_import(pkg_dir: &Path) -> Result<()> {
    let candidates = [
        pkg_dir.join("vzglyd_web.js"),
        pkg_dir.join("vzglyd_web_bg.js"),
    ];
    let replacement = "import { JsEngineBridge } from '../js/engine_bridge.js';";

    for path in candidates {
        if !path.is_file() {
            continue;
        }

        let original = fs::read_to_string(&path)
            .with_context(|| format!("failed to read runtime bridge '{}'", path.display()))?;

        if original.contains(replacement) {
            return Ok(());
        }

        let mut changed = false;
        let mut patched = String::with_capacity(original.len());
        for line in original.lines() {
            if line.starts_with("import { JsEngineBridge } from './snippets/")
                && line.contains("/web-preview/js/engine_bridge.js';")
            {
                patched.push_str(replacement);
                patched.push('\n');
                changed = true;
            } else {
                patched.push_str(line);
                patched.push('\n');
            }
        }

        if changed {
            fs::write(&path, patched)
                .with_context(|| format!("failed to write patched bridge '{}'", path.display()))?;
            return Ok(());
        }
    }

    bail!(
        "runtime bridge import patch failed in '{}'",
        pkg_dir.display()
    )
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in WalkDir::new(source)
        .into_iter()
        .filter_entry(|entry| !should_skip_copy(source, entry.path()))
    {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(source)
            .context("copied path should stay inside source root")?;
        let target = destination.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create copied dir '{}'", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create copied parent dir '{}'", parent.display())
            })?;
        }
        fs::copy(entry.path(), &target).with_context(|| {
            format!(
                "failed to copy '{}' to '{}'",
                entry.path().display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

fn should_skip_copy(source_root: &Path, candidate: &Path) -> bool {
    let Ok(rel) = candidate.strip_prefix(source_root) else {
        return false;
    };
    if rel.as_os_str().is_empty() {
        return false;
    }
    if rel
        .components()
        .any(|component| component.as_os_str() == OsStr::new(".git"))
    {
        return true;
    }
    if rel
        .components()
        .any(|component| component.as_os_str() == OsStr::new("target"))
    {
        return true;
    }
    rel.starts_with(Path::new("web-preview/pkg"))
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create destination dir '{}'", parent.display()))?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy '{}' to '{}'",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn add_file_to_zip(
    zip: &mut zip::ZipWriter<File>,
    source: &Path,
    internal_path: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    let mut bytes = Vec::new();
    File::open(source)
        .with_context(|| format!("failed to open '{}' for zipping", source.display()))?
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read '{}' for zipping", source.display()))?;
    zip.start_file(internal_path, options)
        .with_context(|| format!("failed to add '{internal_path}' to bundle"))?;
    zip.write_all(&bytes)
        .with_context(|| format!("failed to write '{internal_path}' into bundle"))?;
    Ok(())
}

fn run_command(command: &mut Command, label: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to spawn command for {label}"))?;
    if !status.success() {
        bail!("{label} failed with status {status}");
    }
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory '{}'", path.display()))?;
    }
    Ok(())
}

fn outputs_are_stale(dependencies: &[PathBuf], outputs: &[PathBuf]) -> Result<bool> {
    let Some((_, newest_dependency_time)) = newest_file(dependencies)? else {
        return Ok(false);
    };

    for output in outputs {
        let metadata = match fs::metadata(output) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(true),
        };
        let modified = metadata.modified().with_context(|| {
            format!("failed to read output modified time '{}'", output.display())
        })?;
        if modified < newest_dependency_time {
            return Ok(true);
        }
    }

    Ok(false)
}

fn runtime_dependency_files(config: &PreviewConfig) -> Result<Vec<PathBuf>> {
    collect_files(&[
        config.runtime_source_web_root.join("Cargo.toml"),
        config.runtime_source_web_root.join("Cargo.lock"),
        config.runtime_source_web_root.join("src"),
        config.runtime_source_web_root.join("web-preview/js"),
        config.runtime_source_kernel_root.join("Cargo.toml"),
        config.runtime_source_kernel_root.join("Cargo.lock"),
        config.runtime_source_kernel_root.join("src"),
        config.runtime_source_slide_root.join("Cargo.toml"),
        config.runtime_source_slide_root.join("Cargo.lock"),
        config.runtime_source_slide_root.join("src"),
    ])
}

fn runtime_output_paths(config: &PreviewConfig) -> Vec<PathBuf> {
    let pkg_dir = config.runtime_pkg_dir();
    let mut paths = vec![
        pkg_dir.join("vzglyd_web.js"),
        pkg_dir.join("vzglyd_web_bg.wasm"),
    ];
    let bg_js = pkg_dir.join("vzglyd_web_bg.js");
    if bg_js.is_file() {
        paths.push(bg_js);
    }
    paths
}

fn collect_about(config: &PreviewConfig, started_at_utc: &str) -> Result<AboutResponse> {
    let manifest_version = read_manifest_version(&config.manifest_path())?;
    let slide_version = read_package_version(&config.repo_root.join("Cargo.toml"))?;
    let sidecar_version = read_package_version(&config.repo_root.join("sidecar/Cargo.toml"))?;
    let runtime_version = read_package_version(&config.runtime_source_web_root.join("Cargo.toml"))?;
    let kernel_version =
        read_package_version(&config.runtime_source_kernel_root.join("Cargo.toml"))?;

    let checks = vec![
        build_freshness_check(
            config,
            "Slide WASM",
            "Cargo.toml, Cargo.lock, and src/**/*",
            &collect_files(&[
                config.repo_root.join("Cargo.toml"),
                config.repo_root.join("Cargo.lock"),
                config.repo_root.join("src"),
            ])?,
            &[config.slide_output_path()],
        )?,
        build_freshness_check(
            config,
            "Sidecar WASM",
            "sidecar/Cargo.toml, sidecar/Cargo.lock, and sidecar/src/**/*",
            &collect_files(&[
                config.repo_root.join("sidecar/Cargo.toml"),
                config.repo_root.join("sidecar/Cargo.lock"),
                config.repo_root.join("sidecar/src"),
            ])?,
            &[config.sidecar_output_path()],
        )?,
        build_freshness_check(
            config,
            "Bundle Archive",
            "affirmations_slide.json, built wasm outputs, and assets/**/*",
            &collect_files(&[
                config.manifest_path(),
                config.slide_output_path(),
                config.sidecar_output_path(),
                config.assets_dir(),
            ])?,
            &[config.bundle_path()],
        )?,
        build_freshness_check(
            config,
            "Web Runtime Package",
            "../VRX-64-web/src/**/*, ../VRX-64-web/web-preview/js/**/*, and ../VRX-64-kernel/src/**/*",
            &runtime_dependency_files(config)?,
            &runtime_output_paths(config),
        )?,
    ];

    let tracked_files = vec![
        snapshot_file(config, "Manifest", config.manifest_path())?,
        snapshot_file(config, "World Asset", config.assets_dir().join("world.glb"))?,
        snapshot_file(
            config,
            "Viewer HTML",
            config.runtime_source_web_root.join("web-preview/view.html"),
        )?,
        snapshot_file(
            config,
            "Viewer Script",
            config.runtime_source_web_root.join("web-preview/view.js"),
        )?,
        snapshot_file(
            config,
            "Engine Bridge",
            config
                .runtime_source_web_root
                .join("web-preview/js/engine_bridge.js"),
        )?,
        snapshot_file(
            config,
            "Renderer Bridge",
            config
                .runtime_source_web_root
                .join("web-preview/js/renderer.js"),
        )?,
        snapshot_file(
            config,
            "Sidecar Worker",
            config
                .runtime_source_web_root
                .join("web-preview/js/sidecar-worker.js"),
        )?,
        snapshot_file(
            config,
            "WASM Host Bridge",
            config
                .runtime_source_web_root
                .join("web-preview/js/wasm-host.js"),
        )?,
    ]
    .into_iter()
    .map(FileSnapshotInternal::public)
    .collect();

    Ok(AboutResponse {
        server: ServerInfo {
            name: "affirmations-preview".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at_utc: started_at_utc.to_string(),
            preview_url: "/preview/".to_string(),
            bundle_url: "/preview/repo/affirmations.vzglyd".to_string(),
        },
        generated_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        versions: vec![
            NamedValue {
                label: "Manifest version".to_string(),
                value: manifest_version.version,
            },
            NamedValue {
                label: "Manifest ABI".to_string(),
                value: manifest_version.abi_version.to_string(),
            },
            NamedValue {
                label: "Slide crate".to_string(),
                value: slide_version,
            },
            NamedValue {
                label: "Sidecar crate".to_string(),
                value: sidecar_version,
            },
            NamedValue {
                label: "Web runtime crate".to_string(),
                value: runtime_version,
            },
            NamedValue {
                label: "Kernel crate".to_string(),
                value: kernel_version,
            },
        ],
        checks,
        tracked_files,
    })
}

fn build_freshness_check(
    config: &PreviewConfig,
    label: &str,
    dependency_hint: &str,
    dependencies: &[PathBuf],
    outputs: &[PathBuf],
) -> Result<FreshnessCheck> {
    let newest_dependency = newest_file(dependencies)?;
    let mut output_snapshots = Vec::new();
    for output in outputs {
        output_snapshots.push(snapshot_file(
            config,
            output_file_label(output),
            output.clone(),
        )?);
    }

    let stale_output = if let Some((_, newest_dependency_time)) = newest_dependency.as_ref() {
        output_snapshots.iter().find(|output| {
            !output.exists
                || output
                    .modified
                    .is_none_or(|modified| modified < *newest_dependency_time)
        })
    } else {
        output_snapshots.iter().find(|output| !output.exists)
    };

    let stale = stale_output.is_some();
    let summary = match (stale_output, newest_dependency.as_ref()) {
        (Some(output), Some((dependency_path, dependency_time))) if !output.exists => format!(
            "{} is missing; newest dependency is {} at {}",
            output.path,
            display_path(config, dependency_path),
            format_time(*dependency_time)
        ),
        (Some(output), Some((dependency_path, dependency_time))) => format!(
            "{} is older than {} ({} > {})",
            output.path,
            display_path(config, dependency_path),
            format_time(*dependency_time),
            output
                .modified_utc
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        ),
        (Some(output), None) if !output.exists => format!("{} is missing", output.path),
        (Some(output), None) => format!("{} looks older than expected", output.path),
        (None, Some((dependency_path, dependency_time))) => format!(
            "Fresh; newest dependency is {} at {}",
            display_path(config, dependency_path),
            format_time(*dependency_time)
        ),
        (None, None) => "Fresh; no dependencies were discovered for this check".to_string(),
    };

    Ok(FreshnessCheck {
        label: label.to_string(),
        dependency_hint: dependency_hint.to_string(),
        stale,
        summary,
        newest_dependency: newest_dependency.map(|(path, modified)| FileReference {
            path: display_path(config, &path),
            modified_utc: Some(format_time(modified)),
        }),
        outputs: output_snapshots
            .into_iter()
            .map(FileSnapshotInternal::public)
            .collect(),
    })
}

fn output_file_label(path: &Path) -> &str {
    path.file_name().and_then(OsStr::to_str).unwrap_or("output")
}

fn collect_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        for entry in WalkDir::new(path) {
            let entry = entry?;
            if entry.file_type().is_file() {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn newest_file(paths: &[PathBuf]) -> Result<Option<(PathBuf, SystemTime)>> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    for path in paths {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let modified = metadata
            .modified()
            .with_context(|| format!("failed to read modified time for '{}'", path.display()))?;
        let should_replace = newest
            .as_ref()
            .is_none_or(|(_, current_time)| modified > *current_time);
        if should_replace {
            newest = Some((path.clone(), modified));
        }
    }
    Ok(newest)
}

fn snapshot_file(
    config: &PreviewConfig,
    label: &str,
    path: PathBuf,
) -> Result<FileSnapshotInternal> {
    let display = display_path(config, &path);
    if !path.is_file() {
        return Ok(FileSnapshotInternal {
            label: label.to_string(),
            path: display,
            exists: false,
            size_bytes: None,
            modified: None,
            modified_utc: None,
            sha256: None,
        });
    }

    let metadata =
        fs::metadata(&path).with_context(|| format!("failed to stat '{}'", path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("failed to read modified time for '{}'", path.display()))?;
    let sha256 = hash_file(&path)?;

    Ok(FileSnapshotInternal {
        label: label.to_string(),
        path: display,
        exists: true,
        size_bytes: Some(metadata.len()),
        modified: Some(modified),
        modified_utc: Some(format_time(modified)),
        sha256: Some(sha256),
    })
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open '{}' for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read '{}' for hashing", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn display_path(config: &PreviewConfig, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(&config.repo_root) {
        return rel.display().to_string();
    }
    if let Ok(rel) = path.strip_prefix(&config.runtime_source_web_root) {
        return format!("../VRX-64-web/{}", rel.display());
    }
    if let Ok(rel) = path.strip_prefix(&config.runtime_source_kernel_root) {
        return format!("../VRX-64-kernel/{}", rel.display());
    }
    if let Ok(rel) = path.strip_prefix(config.runtime_live_root()) {
        return format!("$RUNTIME/{}", rel.display());
    }
    path.display().to_string()
}

fn format_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn read_package_version(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read cargo manifest '{}'", path.display()))?;
    let manifest: CargoManifest = toml::from_str(&text)
        .with_context(|| format!("failed to parse cargo manifest '{}'", path.display()))?;
    manifest
        .package
        .map(|package| package.version)
        .context("cargo manifest is missing package.version")
}

fn read_manifest_version(path: &Path) -> Result<ManifestVersionInfo> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read slide manifest '{}'", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse slide manifest '{}'", path.display()))
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    version: String,
}

#[derive(Deserialize)]
struct ManifestVersionInfo {
    version: String,
    abi_version: u32,
}

#[derive(Serialize)]
struct AboutResponse {
    server: ServerInfo,
    generated_at_utc: String,
    versions: Vec<NamedValue>,
    checks: Vec<FreshnessCheck>,
    tracked_files: Vec<FileSnapshot>,
}

#[derive(Serialize)]
struct ServerInfo {
    name: String,
    version: String,
    started_at_utc: String,
    preview_url: String,
    bundle_url: String,
}

#[derive(Serialize)]
struct NamedValue {
    label: String,
    value: String,
}

#[derive(Serialize)]
struct FreshnessCheck {
    label: String,
    dependency_hint: String,
    stale: bool,
    summary: String,
    newest_dependency: Option<FileReference>,
    outputs: Vec<FileSnapshot>,
}

#[derive(Serialize)]
struct FileReference {
    path: String,
    modified_utc: Option<String>,
}

#[derive(Serialize)]
struct FileSnapshot {
    label: String,
    path: String,
    exists: bool,
    size_bytes: Option<u64>,
    modified_utc: Option<String>,
    sha256: Option<String>,
}

struct FileSnapshotInternal {
    label: String,
    path: String,
    exists: bool,
    size_bytes: Option<u64>,
    modified: Option<SystemTime>,
    modified_utc: Option<String>,
    sha256: Option<String>,
}

impl FileSnapshotInternal {
    fn public(self) -> FileSnapshot {
        FileSnapshot {
            label: self.label,
            path: self.path,
            exists: self.exists,
            size_bytes: self.size_bytes,
            modified_utc: self.modified_utc,
            sha256: self.sha256,
        }
    }
}

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self(value.into())
    }
}
