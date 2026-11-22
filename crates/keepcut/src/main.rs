//! Keepcut: mark the parts of your clips to keep, then cut and join them.
//!
//! Usage: keepcut [--script FILE] [FILES…]   (see script.rs for the test automation)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clips;
mod config;
mod editor;
mod export;
mod media;
mod project;
mod script;
mod settings;
mod timeline;

use std::path::PathBuf;

use eframe::egui;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    // <data dir>/logs/keepcut.YYYY-MM-DD, 7 days kept (§9)
    let file = config::dirs().and_then(|d| {
        let dir = d.data_dir().join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("keepcut")
            .filename_suffix("log")
            .max_log_files(7)
            .build(dir)
            .ok()
    });
    match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer);
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr)
                .with(file_layer)
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr)
                .init();
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    let _log_guard = init_logging();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut script = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        if a == "--script" {
            let p = PathBuf::from(args.next().unwrap_or_default());
            script = Some(
                script::Script::load(&p)
                    .map_err(|e| anyhow::anyhow!("script {}: {e}", p.display()))?,
            );
        } else {
            files.push(PathBuf::from(a));
        }
    }
    tracing::info!("Keepcut {} starting", env!("CARGO_PKG_VERSION"));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Keepcut")
            .with_inner_size([1500.0, 950.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "Keepcut",
        options,
        Box::new(move |cc| {
            let mut app = app::App::new(cc, files)?;
            app.script = script;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
