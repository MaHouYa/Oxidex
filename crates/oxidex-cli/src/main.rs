use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use oxidex_client::{OxidexClient, default_daemon_socket_path};
use oxidex_core::daemon_model::SearchQueryParams;
use oxidex_core::doctor::{
    DoctorCategory, DoctorOptions, DoctorReport, DoctorSeverity, run_local_doctor,
};
use oxidex_core::model::{SortDirection, SortKey};
use serde_json::Value;

fn main() {
    if let Err(err) = run() {
        eprintln!("oxidex-cli: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print_usage();
        return Ok(());
    }

    let command = args.remove(0);
    if command == "doctor" {
        return run_doctor(args);
    }

    let mut client = connect_or_start_daemon()?;
    match command.as_str() {
        "search" => command_search(&mut client, args),
        "explain" => {
            let query = args.join(" ");
            let explanation = client.explain(&query)?;
            println!("{}", explanation.summary);
            Ok(())
        }
        "rofi" => command_rofi(&mut client, args),
        "indexes" => {
            for index in client.indexes()? {
                println!(
                    "{}\t{}\t{}\t{} entries\t{}",
                    index.device_id,
                    index.fs_type.as_str(),
                    label_or_dash(&index.label),
                    index.entry_count,
                    index
                        .state
                        .and_then(|state| state.stale_reason)
                        .unwrap_or_else(|| "fresh".into())
                );
            }
            Ok(())
        }
        "devices" => {
            for device in client.devices()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    device.device_id,
                    device.fs_type_name,
                    label_or_dash(&device.label),
                    if device.mounted {
                        "mounted"
                    } else {
                        "not-mounted"
                    },
                    device.dev_node,
                    device
                        .scan_unavailable_reason
                        .unwrap_or_else(|| "scan-supported".into())
                );
            }
            Ok(())
        }
        "scan" => command_scan(&mut client, args),
        "jobs" => {
            for job in client.jobs()? {
                println!(
                    "{}\t{}\t{:?}\t{}%\t{}",
                    job.job_id, job.device_id, job.state, job.progress, job.message
                );
            }
            Ok(())
        }
        "cancel" => {
            let Some(job_id) = args.first() else {
                anyhow::bail!("cancel requires a job id");
            };
            let result = client.cancel_scan(Some(job_id.parse()?), None)?;
            println!("{}", result.message);
            Ok(())
        }
        "open" => command_open(&mut client, args, OpenMode::File),
        "open-folder" => command_open(&mut client, args, OpenMode::Folder),
        "copy-path" => command_copy_path(&mut client, args),
        "resolve" => command_resolve(&mut client, args),
        "config" => command_config(&mut client, args),
        other => anyhow::bail!("unknown command: {other}"),
    }
}

fn command_search(client: &mut OxidexClient, mut args: Vec<String>) -> anyhow::Result<()> {
    let mut json_output = false;
    let mut limit = None;
    let mut sort_key = SortKey::Relevance;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--json" => {
                json_output = true;
                args.remove(idx);
            }
            "--limit" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| anyhow::anyhow!("--limit requires a value"))?
                    .parse()?;
                args.drain(idx..=idx + 1);
                limit = Some(value);
            }
            "--sort" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| anyhow::anyhow!("--sort requires a value"))?;
                sort_key = parse_sort_key(value)?;
                args.drain(idx..=idx + 1);
            }
            _ => idx += 1,
        }
    }

    let query = args.join(" ");
    let result = client.search(&SearchQueryParams {
        query,
        request: None,
        device_filter: None,
        sort_key,
        sort_direction: SortDirection::Asc,
        max_results: limit,
    })?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result.rows)?);
    } else {
        for row in result.rows {
            println!("{}\t{}", row.name, row.display_path);
        }
    }
    Ok(())
}

fn command_rofi(client: &mut OxidexClient, mut args: Vec<String>) -> anyhow::Result<()> {
    let config = client.config_get().ok().map(|result| result.config);
    let mut limit = config
        .as_ref()
        .map(|config| config.rofi.max_results)
        .unwrap_or(200);
    let mut show_id = false;
    let mut full_path = config
        .as_ref()
        .map(|config| config.rofi.show_full_path)
        .unwrap_or(false);
    let show_device_label = config
        .as_ref()
        .map(|config| config.rofi.show_device_label)
        .unwrap_or(true);

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--limit" => {
                limit = args
                    .get(idx + 1)
                    .ok_or_else(|| anyhow::anyhow!("--limit requires a value"))?
                    .parse()?;
                args.drain(idx..=idx + 1);
            }
            "--show-id" => {
                show_id = true;
                args.remove(idx);
            }
            "--full-path" => {
                full_path = true;
                args.remove(idx);
            }
            _ => idx += 1,
        }
    }

    let query = args.join(" ");
    let result = client.search(&SearchQueryParams {
        query,
        request: None,
        device_filter: None,
        sort_key: SortKey::Relevance,
        sort_direction: SortDirection::Asc,
        max_results: Some(limit),
    })?;
    for row in result.rows {
        let mut label = if full_path {
            format!("{}\t{}", row.name, row.display_path)
        } else if show_device_label {
            format!("{}\t{}", row.name, row.device_label)
        } else {
            row.name.clone()
        };
        if show_id {
            label.push('\t');
            label.push_str(&format_hit_id(&row.hit.device_id, row.hit.record_idx));
        }
        println!("{label}");
    }
    Ok(())
}

fn command_scan(client: &mut OxidexClient, args: Vec<String>) -> anyhow::Result<()> {
    let wait = args.iter().any(|arg| arg == "--wait");
    let device_id = args
        .iter()
        .find(|arg| arg.as_str() != "--wait")
        .ok_or_else(|| anyhow::anyhow!("scan requires a device id"))?;
    let result = client.start_scan(device_id)?;
    println!(
        "queued scan {}\t{}\t{:?}",
        result.job_id, result.device_id, result.state
    );
    if wait {
        loop {
            let job = client.job_status(result.job_id)?;
            println!(
                "{}\t{}\t{:?}\t{}%",
                job.job_id, job.device_id, job.state, job.progress
            );
            match job.state {
                oxidex_core::daemon_model::ScanState::Finished => break,
                oxidex_core::daemon_model::ScanState::Failed
                | oxidex_core::daemon_model::ScanState::Cancelled => {
                    anyhow::bail!("{}", job.error.unwrap_or(job.message))
                }
                _ => thread::sleep(Duration::from_millis(500)),
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum OpenMode {
    File,
    Folder,
}

fn command_open(
    client: &mut OxidexClient,
    args: Vec<String>,
    mode: OpenMode,
) -> anyhow::Result<()> {
    let (device_id, record_idx) = parse_hit_arg(&args)?;
    let resolved = client.resolve_path(&device_id, record_idx)?;
    anyhow::ensure!(resolved.mounted, "indexed device is not mounted");
    let target = match mode {
        OpenMode::File => PathBuf::from(&resolved.path),
        OpenMode::Folder => {
            let path = PathBuf::from(&resolved.path);
            if path.is_dir() {
                path
            } else {
                path.parent().map(PathBuf::from).unwrap_or(path)
            }
        }
    };
    open::that(&target)?;
    Ok(())
}

fn command_copy_path(client: &mut OxidexClient, args: Vec<String>) -> anyhow::Result<()> {
    let (device_id, record_idx) = parse_hit_arg(&args)?;
    let resolved = client.resolve_path(&device_id, record_idx)?;
    copy_text(&resolved.path)?;
    println!("{}", resolved.path);
    Ok(())
}

fn command_resolve(client: &mut OxidexClient, mut args: Vec<String>) -> anyhow::Result<()> {
    let json_output = args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    let (device_id, record_idx) = parse_hit_arg(&args)?;
    let resolved = client.resolve_path(&device_id, record_idx)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&resolved)?);
    } else {
        println!("{}", resolved.path);
    }
    Ok(())
}

fn command_config(client: &mut OxidexClient, args: Vec<String>) -> anyhow::Result<()> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        anyhow::bail!("config requires get or set");
    };
    match subcommand {
        "get" => {
            let result = client.config_get()?;
            println!("# {}", result.path);
            println!("{}", toml::to_string_pretty(&result.config)?);
        }
        "set" => {
            if args.len() < 3 {
                anyhow::bail!("config set requires a path and value");
            }
            let value = parse_value(&args[2]);
            let result = client.config_set(&args[1], value)?;
            println!("updated {}", result.path);
        }
        other => anyhow::bail!("unknown config command: {other}"),
    }
    Ok(())
}

fn run_doctor(args: Vec<String>) -> anyhow::Result<()> {
    let json_output = args.iter().any(|arg| arg == "--json");
    let scanner_only = args.iter().any(|arg| arg == "--scanner");
    let security_only = args.iter().any(|arg| arg == "--security");
    let mut report = run_local_doctor(&DoctorOptions {
        daemon_socket: default_daemon_socket_path().ok(),
        scanner_socket: None,
        scanner_only,
        security_only,
    });

    match OxidexClient::connect_default() {
        Ok(mut client) => match client.doctor() {
            Ok(result) => report.merge(result.report),
            Err(err) => report.add_detail(
                DoctorCategory::Daemon,
                DoctorSeverity::Warn,
                "daemon_doctor",
                "Daemon is reachable but daemon.doctor failed",
                format!("{err:#}"),
            ),
        },
        Err(err) => report.add_detail(
            DoctorCategory::Daemon,
            DoctorSeverity::Warn,
            "daemon_connection",
            "oxidexd is not reachable",
            format!("{err:#}"),
        ),
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_doctor_report(&report);
    }
    Ok(())
}

fn print_doctor_report(report: &DoctorReport) {
    println!("Oxidex Doctor");
    for check in &report.checks {
        println!(
            "{:<5} {:<9} {:<28} {}",
            severity_label(check.severity),
            category_label(check.category),
            check.name,
            check.message
        );
        if let Some(detail) = &check.detail
            && !detail.trim().is_empty()
        {
            println!("      {detail}");
        }
    }
}

fn connect_or_start_daemon() -> anyhow::Result<OxidexClient> {
    match OxidexClient::connect_default() {
        Ok(client) => Ok(client),
        Err(first_err) => {
            start_daemon_once()?;
            for _ in 0..20 {
                if let Ok(client) = OxidexClient::connect_default() {
                    return Ok(client);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(first_err).context("failed to connect to oxidexd after attempting to start it")
        }
    }
}

fn start_daemon_once() -> anyhow::Result<()> {
    let daemon = sibling_binary("oxidexd");
    Command::new(daemon)
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn parse_hit_arg(args: &[String]) -> anyhow::Result<(String, u32)> {
    let value = args
        .windows(2)
        .find(|pair| pair[0] == "--hit")
        .map(|pair| pair[1].as_str())
        .or_else(|| args.first().map(String::as_str))
        .ok_or_else(|| anyhow::anyhow!("command requires --hit <device-id>:<record-idx>"))?;
    let Some((device_id, record_idx)) = value.rsplit_once(':') else {
        anyhow::bail!("hit must be formatted as <device-id>:<record-idx>");
    };
    Ok((device_id.to_owned(), record_idx.parse()?))
}

fn format_hit_id(device_id: &str, record_idx: u32) -> String {
    format!("{device_id}:{record_idx}")
}

fn parse_sort_key(value: &str) -> anyhow::Result<SortKey> {
    match value {
        "relevance" | "rank" => Ok(SortKey::Relevance),
        "name" => Ok(SortKey::Name),
        "path" => Ok(SortKey::Path),
        "size" => Ok(SortKey::Size),
        "mtime" | "modified" | "date" => Ok(SortKey::Mtime),
        other => anyhow::bail!("unsupported sort key '{other}'"),
    }
}

fn copy_text(text: &str) -> anyhow::Result<()> {
    for command in [
        ("wl-copy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["-ib"]),
    ] {
        if run_clipboard_command(command.0, &command.1, text).is_ok() {
            return Ok(());
        }
    }
    Ok(())
}

fn run_clipboard_command(command: &str, args: &[&str], text: &str) -> anyhow::Result<()> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "{command} failed");
    Ok(())
}

fn sibling_binary(name: &str) -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(name);
    };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join(name);
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

fn parse_value(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()))
}

fn label_or_dash(label: &str) -> &str {
    if label.trim().is_empty() {
        "-"
    } else {
        label.trim()
    }
}

fn severity_label(severity: DoctorSeverity) -> &'static str {
    match severity {
        DoctorSeverity::Ok => "OK",
        DoctorSeverity::Info => "INFO",
        DoctorSeverity::Warn => "WARN",
        DoctorSeverity::Fail => "FAIL",
    }
}

fn category_label(category: DoctorCategory) -> &'static str {
    match category {
        DoctorCategory::Daemon => "daemon",
        DoctorCategory::Scanner => "scanner",
        DoctorCategory::Systemd => "systemd",
        DoctorCategory::Config => "config",
        DoctorCategory::Indexes => "indexes",
        DoctorCategory::Security => "security",
        DoctorCategory::Packaging => "package",
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  oxidex-cli search [--json] [--limit N] [--sort relevance|name|path|size|mtime] <query>\n  oxidex-cli explain <query>\n  oxidex-cli rofi [--limit N] [--show-id] [--full-path] <query>\n  oxidex-cli indexes\n  oxidex-cli devices\n  oxidex-cli scan <device-id> [--wait]\n  oxidex-cli jobs\n  oxidex-cli cancel <job-id>\n  oxidex-cli open --hit <device-id>:<record-idx>\n  oxidex-cli open-folder --hit <device-id>:<record-idx>\n  oxidex-cli copy-path --hit <device-id>:<record-idx>\n  oxidex-cli resolve --hit <device-id>:<record-idx> [--json]\n  oxidex-cli config get\n  oxidex-cli config set <path> <value>\n  oxidex-cli doctor [--json] [--scanner] [--security]"
    );
}
