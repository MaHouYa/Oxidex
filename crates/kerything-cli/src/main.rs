use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use kerything_client::KerythingClient;
use kerything_core::daemon_model::SearchQueryParams;
use kerything_core::model::{SortDirection, SortKey};
use serde_json::Value;

fn main() {
    if let Err(err) = run() {
        eprintln!("kerything-cli: {err:#}");
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
    let mut client = connect_or_start_daemon()?;
    match command.as_str() {
        "search" => {
            let json_output = args.first().map(|arg| arg == "--json").unwrap_or(false);
            if json_output {
                args.remove(0);
            }
            let query = args.join(" ");
            let result = client.search(&SearchQueryParams {
                query,
                request: None,
                device_filter: None,
                sort_key: SortKey::Name,
                sort_direction: SortDirection::Asc,
                max_results: None,
            })?;
            if json_output {
                println!("{}", serde_json::to_string_pretty(&result.rows)?);
            } else {
                for row in result.rows {
                    println!("{}\t{}", row.name, row.display_path);
                }
            }
        }
        "rofi" => {
            let query = args.join(" ");
            let result = client.search(&SearchQueryParams {
                query,
                request: None,
                device_filter: None,
                sort_key: SortKey::Name,
                sort_direction: SortDirection::Asc,
                max_results: Some(500),
            })?;
            for row in result.rows {
                println!("{}\t{}", row.name, row.display_path);
            }
        }
        "indexes" => {
            for index in client.indexes()? {
                println!(
                    "{}\t{}\t{}\t{} entries",
                    index.device_id,
                    index.fs_type.as_str(),
                    label_or_dash(&index.label),
                    index.entry_count
                );
            }
        }
        "devices" => {
            for device in client.devices()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    device.device_id,
                    device.fs_type.as_str(),
                    label_or_dash(&device.label),
                    if device.mounted {
                        "mounted"
                    } else {
                        "not-mounted"
                    },
                    device.dev_node
                );
            }
        }
        "scan" => {
            let Some(device_id) = args.first() else {
                anyhow::bail!("scan requires a device id");
            };
            let result = client.start_scan(device_id)?;
            println!(
                "indexed {}\t{}\t{} entries",
                result.summary.device_id, result.summary.fs_type, result.summary.entry_count
            );
        }
        "config" => {
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
        }
        other => anyhow::bail!("unknown command: {other}"),
    }
    Ok(())
}

fn connect_or_start_daemon() -> anyhow::Result<KerythingClient> {
    match KerythingClient::connect_default() {
        Ok(client) => Ok(client),
        Err(first_err) => {
            start_daemon_once()?;
            for _ in 0..20 {
                if let Ok(client) = KerythingClient::connect_default() {
                    return Ok(client);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(first_err).context("failed to connect to kerythingd after attempting to start it")
        }
    }
}

fn start_daemon_once() -> anyhow::Result<()> {
    let daemon = sibling_binary("kerythingd");
    Command::new(daemon)
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
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

fn print_usage() {
    eprintln!(
        "Usage:\n  kerything-cli search [--json] <query>\n  kerything-cli rofi <query>\n  kerything-cli indexes\n  kerything-cli devices\n  kerything-cli scan <device-id>\n  kerything-cli config get\n  kerything-cli config set <path> <value>"
    );
}
