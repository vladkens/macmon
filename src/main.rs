use clap::{CommandFactory, Parser, Subcommand, parser::ValueSource};
use macmon::{App, Sampler, debug};
use std::error::Error;
use std::net::ToSocketAddrs;
use std::sync::{Arc, RwLock};
use std::thread;

mod serve;

// Validate by asking the platform resolver for at least one socket address.
// The original host string is preserved so launchd and serve use the same value
// the user provided.
fn validate_host(host: &str) -> Result<String, String> {
  match (host, 0).to_socket_addrs() {
    Ok(mut addrs) => {
      if addrs.next().is_some() {
        Ok(host.to_string())
      } else {
        Err("host must resolve to a socket address".to_string())
      }
    }
    _ => Err("host must resolve to a socket address".to_string()),
  }
}

#[derive(Debug, Subcommand)]
enum Commands {
  /// Output metrics in JSON format (suitable for piping)
  #[command(alias = "raw")]
  Pipe {
    /// Number of samples to run for. Set to 0 to run indefinitely
    #[arg(short, long, default_value_t = 0)]
    samples: u32,

    /// Include SoC information in the output
    #[arg(long, default_value_t = false)]
    soc_info: bool,
  },

  /// Serve metrics over HTTP (JSON at /json, Prometheus at /metrics)
  Serve {
    /// Host address to listen on
    #[arg(long, default_value = "0.0.0.0", value_parser = validate_host)]
    host: String,

    /// Port to listen on
    #[arg(short, long, default_value_t = 9090)]
    port: u16,

    /// Install as a launchd service (auto-start on login)
    #[arg(long, default_value_t = false)]
    install: bool,

    /// Uninstall the launchd service
    #[arg(long, default_value_t = false)]
    uninstall: bool,
  },

  /// Print debug information
  Debug,
}

/// Sudoless performance monitoring CLI tool for Apple Silicon processors
/// https://github.com/vladkens/macmon
#[derive(Debug, Parser)]
#[command(version, verbatim_doc_comment)]
struct Cli {
  #[command(subcommand)]
  command: Option<Commands>,

  /// Update interval in milliseconds
  #[arg(short, long, global = true, default_value_t = 1000)]
  interval: u32,
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();

  match &args.command {
    Some(Commands::Pipe { samples, soc_info }) => {
      let mut sampler = Sampler::new()?;
      let mut counter = 0u32;

      let soc_info_val = if *soc_info { Some(sampler.get_soc_info().clone()) } else { None };

      loop {
        let doc = sampler.get_metrics(args.interval.max(100))?;

        let mut doc = serde_json::to_value(&doc)?;
        if let Some(ref soc) = soc_info_val {
          doc["soc"] = serde_json::to_value(soc)?;
        }
        doc["timestamp"] = serde_json::to_value(chrono::Utc::now().to_rfc3339())?;
        let doc = serde_json::to_string(&doc)?;

        println!("{}", doc);

        counter += 1;
        if *samples > 0 && counter >= *samples {
          break;
        }
      }
    }
    Some(Commands::Serve { host, port, install, uninstall }) => {
      if *install || *uninstall {
        serve::launchd(host, *port, *install)?;
        return Ok(());
      }
      let mut sampler = Sampler::new()?;
      let soc = Arc::new(sampler.get_soc_info().clone());
      let shared: serve::SharedMetrics = Arc::new(RwLock::new(None));

      let shared_http = Arc::clone(&shared);
      let soc_http = Arc::clone(&soc);
      let host = host.clone();
      let port = *port;
      thread::spawn(move || {
        if let Err(e) = serve::run(&host, port, shared_http, soc_http) {
          eprintln!("server error: {e}");
        }
      });

      loop {
        match sampler.get_metrics(args.interval.max(100)) {
          Ok(m) => *shared.write().unwrap() = Some(m),
          Err(e) => eprintln!("sampling error: {e}"),
        }
      }
    }
    Some(Commands::Debug) => debug::print_debug()?,
    _ => {
      let mut app = App::new()?;

      let matches = Cli::command().get_matches();
      let msec = match matches.value_source("interval") {
        Some(ValueSource::CommandLine) => Some(args.interval),
        _ => None,
      };

      app.run_loop(msec)?;
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::validate_host;

  #[test]
  fn accepts_resolvable_hosts() {
    assert!(validate_host("127.0.0.1").is_ok());
    assert!(validate_host("::1").is_ok());
    assert!(validate_host("0.0.0.0").is_ok());
    assert!(validate_host("localhost").is_ok());
    assert!(validate_host("0").is_ok());
    assert!(validate_host("127.1").is_ok());
  }

  #[test]
  fn rejects_invalid_hostnames() {
    assert!(validate_host("").is_err());
    assert!(validate_host("example..com").is_err());
    assert!(validate_host("example com").is_err());
    assert!(validate_host("example.com:9090").is_err());
  }

  #[test]
  fn rejects_unresolvable_dns_hostnames() {
    assert!(validate_host("example.invalid").is_err());
  }

  #[test]
  fn rejects_xml_metacharacters() {
    assert!(validate_host("<example>").is_err());
    assert!(validate_host("example>host").is_err());
    assert!(validate_host("example&host").is_err());
    assert!(validate_host(r#""example""#).is_err());
    assert!(validate_host("example'host").is_err());
  }
}
