//! topmanagerd — samples the system, evaluates health and alerts, keeps
//! history, and serves it all on the session bus for the top-bar HUD and GUI.

mod backup;
mod notify;
mod paths;
mod service;
mod state;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tm_collect::{Collector, Host};
use tm_core::bus;
use tm_core::format::{format_bytes, format_bytes_per_second};
use tm_core::health;

use crate::paths::Paths;
use crate::service::Service;
use crate::state::State;

const USAGE: &str = "\
topmanagerd — TopManager background service

USAGE:
    topmanagerd [run]                     Serve on the session bus (default)
    topmanagerd dump [--light]            Print one full snapshot as JSON
    topmanagerd summary                   Print a short human-readable summary
    topmanagerd export FILE [--with-history]
    topmanagerd import FILE [--with-history]
    topmanagerd paths                     Show where settings and state live
    topmanagerd --version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let cmd = args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or("run");

    if flag("--version") || flag("-V") {
        println!("topmanagerd {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if flag("--help") || flag("-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let file_arg = || args.iter().filter(|a| !a.starts_with('-')).nth(1).map(PathBuf::from);
    let result = match cmd {
        "run" => run(),
        "dump" => dump(!flag("--light")),
        "summary" => summary(),
        "paths" => {
            let p = Paths::from_env();
            println!("config:  {}", p.config_file().display());
            println!("history: {}", p.history_file().display());
            println!("alerts:  {}", p.alerts_file().display());
            Ok(())
        }
        "export" | "import" => match file_arg() {
            None => Err(format!("{cmd} needs a FILE argument")),
            Some(file) => {
                let paths = Paths::from_env();
                let r = if cmd == "export" {
                    backup::export(&paths, &file, flag("--with-history"))
                } else {
                    backup::import(&paths, &file, flag("--with-history"))
                };
                r.map(|msg| {
                    println!("{msg}");
                    if cmd == "import" {
                        ask_running_daemon_to_reload();
                    }
                })
            }
        },
        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("topmanagerd: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Two samples one second apart, so rates and CPU percentages are real.
fn primed_snapshot(full: bool) -> tm_core::model::Snapshot {
    let mut c = Collector::new(Host::default());
    c.sample(full);
    std::thread::sleep(Duration::from_secs(1));
    c.sample(full)
}

fn dump(full: bool) -> Result<(), String> {
    let snap = primed_snapshot(full);
    println!("{}", serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?);
    Ok(())
}

fn summary() -> Result<(), String> {
    let s = primed_snapshot(false);
    let mut engine = tm_core::alert::AlertEngine::new(Default::default());
    let top = s.top_by_cpu(1).first().copied().cloned();
    engine.evaluate(
        &tm_core::alert::AlertInput {
            cpu_usage: s.cpu.global_usage,
            memory: Some(&s.memory),
            disk: Some(&s.disk),
            thermal: s.thermal.level,
            top_process: top.as_ref(),
            power: Some(&s.power),
        },
        s.timestamp,
    );
    println!("{} — {} — kernel {}", s.system.hostname, s.system.os_name, s.system.kernel);
    println!("CPU      {:5.1}%  ({} cores, {})", s.cpu.global_usage, s.cpu.cores.len(), s.system.cpu_model);
    println!(
        "Memory   {:5.1}%  {} / {} (available {}, swap {} / {}, pressure {:?})",
        s.memory.usage_percentage(),
        format_bytes(s.memory.used),
        format_bytes(s.memory.total),
        format_bytes(s.memory.available),
        format_bytes(s.memory.swap_used),
        format_bytes(s.memory.swap_total),
        s.memory.pressure
    );
    println!(
        "Network  ↓ {}  ↑ {}",
        format_bytes_per_second(s.network.total_rx_rate),
        format_bytes_per_second(s.network.total_tx_rate)
    );
    for v in &s.disk.volumes {
        println!(
            "Disk     {:5.1}%  {} ({}, {}) {} free",
            v.usage_percentage(),
            v.mount_point,
            v.file_system,
            v.device,
            format_bytes(v.free)
        );
    }
    for g in &s.gpus {
        println!("GPU      {} util={:?} vram={:?}", g.name, g.utilization, g.vram_used.map(format_bytes));
    }
    if s.power.has_battery {
        println!(
            "Battery  {}% charging={} plugged={} health={:?}",
            s.power.charge_percent,
            s.power.is_charging,
            s.power.is_plugged_in,
            s.power.health_percent()
        );
    } else {
        println!("Battery  none");
    }
    println!("Thermal  {:?} max={:?}°C ({:?})", s.thermal.level, s.thermal.max_temp, s.thermal.sensor);
    println!("Health   {}/100 {}  {:?}", engine.health_score, health::rating(engine.health_score), engine.diagnosis);
    println!("Top CPU:");
    for p in s.top_by_cpu(5) {
        println!(
            "  {:>7} {:>6.1}%  {:>9}  {}{}",
            p.pid,
            p.cpu,
            format_bytes(p.memory),
            p.name,
            if p.protected { " [protected]" } else { "" }
        );
    }
    Ok(())
}

fn ask_running_daemon_to_reload() {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return,
    };
    rt.block_on(async {
        if let Ok(conn) = zbus::Connection::session().await {
            let r = conn.call_method(Some(bus::NAME), bus::PATH, Some(bus::INTERFACE), "Reload", &()).await;
            if r.is_ok() {
                println!("running topmanagerd reloaded");
            }
        }
    });
}

fn run() -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(serve())
}

async fn serve() -> Result<(), String> {
    let paths = Paths::from_env();
    let state = Arc::new(Mutex::new(State::load(paths, tm_collect::host::now_unix())));
    let wake = Arc::new(tokio::sync::Notify::new());
    let service = Service { state: state.clone(), wake: wake.clone() };

    let conn = zbus::connection::Builder::session()
        .map_err(|e| e.to_string())?
        .name(bus::NAME)
        .map_err(|e| e.to_string())?
        .serve_at(bus::PATH, service)
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| format!("cannot own {} on the session bus (already running?): {e}", bus::NAME))?;
    eprintln!("topmanagerd {} serving {} on the session bus", env!("CARGO_PKG_VERSION"), bus::NAME);

    let iface = conn.object_server().interface::<_, Service>(bus::PATH).await.map_err(|e| e.to_string())?;

    let collector = Arc::new(Mutex::new(Collector::new(Host::default())));
    let mut term =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;

    loop {
        let (full, interval, notifications) = {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            if st.settings_store.changed_on_disk() {
                let s = st.settings_store.load();
                let _ = st.apply_settings(s);
            }
            (st.wants_full(), st.settings.general.refresh_interval, st.settings.general.notifications)
        };

        let c = collector.clone();
        let snap = tokio::task::spawn_blocking(move || c.lock().unwrap_or_else(|e| e.into_inner()).sample(full))
            .await
            .map_err(|e| e.to_string())?;

        let (raised, summary) = {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            let raised = st.ingest(snap);
            (raised, st.summary().to_string())
        };
        let emitter = iface.signal_emitter();
        let _ = Service::tick(emitter, &summary).await;
        for alert in &raised {
            let json = serde_json::to_string(alert).unwrap_or_default();
            let _ = Service::alert_raised(emitter, &json).await;
            if notifications {
                if let Err(e) = notify::send(&conn, alert).await {
                    eprintln!("topmanagerd: notification failed: {e}");
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs_f64(interval)) => {}
            _ = wake.notified() => {
                // Coalesce bursts (e.g. a slider dragged in Settings).
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
    }
    eprintln!("topmanagerd: shutting down");
    Ok(())
}
