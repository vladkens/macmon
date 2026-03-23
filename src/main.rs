mod app;
mod api;
mod config;
mod debug;
mod metrics;
mod sources;

use api::collect_snapshot;
use app::App;
use clap::{CommandFactory, Parser, Subcommand, parser::ValueSource};
use config::Config;
use metrics::Sampler;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Subcommand)]
enum ServerCommands {
  /// Start the API server
  Up {
    /// Bind address for the API server
    #[arg(long = "bind-address", alias = "host", default_value = "127.0.0.1")]
    bind_address: String,

    /// TCP port to bind the API server to
    #[arg(long, default_value_t = 3939)]
    port: u16,

    /// Keep the server attached to the current terminal
    #[arg(long, default_value_t = false)]
    foreground: bool,
  },

  /// Stop the background API server
  Down,
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

  /// Print debug information
  Debug,

  #[command(
    about = "Start or stop the HTTP API server",
    after_help = "Startup options for `macmon server up`:\n      --bind-address <BIND_ADDRESS>  Bind address for the API server\n      --port <PORT>                  TCP port to bind the API server to\n      --foreground                   Keep the server attached to the current terminal"
  )]
  Server {
    #[command(subcommand)]
    action: ServerCommands,
  },
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

      loop {
        let doc = collect_snapshot(&mut sampler, args.interval, *soc_info)?;
        let doc = serde_json::to_string(&doc)?;

        println!("{}", doc);

        counter += 1;
        if *samples > 0 && counter >= *samples {
          break;
        }
      }
    }
    Some(Commands::Debug) => debug::print_debug()?,
    Some(Commands::Server { action }) => handle_server_command(&args, action)?,
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

fn handle_server_command(args: &Cli, action: &ServerCommands) -> Result<(), Box<dyn Error>> {
  match action {
    ServerCommands::Up { bind_address, port, foreground } => {
      if *foreground {
        api::run_server(bind_address, *port, args.interval)?;
      } else {
        let pid_path = get_server_pid_path()?;
        ensure_server_not_running(&pid_path)?;
        let child = spawn_background_server(args, bind_address, *port)?;
        write_server_pid(&pid_path, child.id())?;
        println!(
          "macmon server started in background on http://{}:{} (pid {})",
          bind_address,
          port,
          child.id()
        );
      }
    }
    ServerCommands::Down => {
      let pid_path = get_server_pid_path()?;
      stop_background_server(&pid_path)?;
      println!("macmon server stopped");
    }
  }

  Ok(())
}

fn spawn_background_server(
  args: &Cli,
  bind_address: &str,
  port: u16,
) -> Result<std::process::Child, Box<dyn Error>> {
  let exe = std::env::current_exe()?;
  let mut cmd = Command::new(exe);
  cmd
    .arg("--interval")
    .arg(args.interval.to_string())
    .arg("server")
    .arg("up")
    .arg("--bind-address")
    .arg(bind_address)
    .arg("--port")
    .arg(port.to_string())
    .arg("--foreground")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

  #[cfg(unix)]
  unsafe {
    cmd.pre_exec(|| {
      if libc::setsid() == -1 {
        return Err(std::io::Error::last_os_error());
      }
      Ok(())
    });
  }

  Ok(cmd.spawn()?)
}

fn get_server_pid_path() -> Result<PathBuf, Box<dyn Error>> {
  Config::get_server_pid_path().ok_or_else(|| "failed to resolve server pid path".into())
}

fn write_server_pid(path: &PathBuf, pid: u32) -> Result<(), Box<dyn Error>> {
  fs::write(path, pid.to_string())?;
  Ok(())
}

fn read_server_pid(path: &PathBuf) -> Result<i32, Box<dyn Error>> {
  let pid = fs::read_to_string(path)?.trim().parse::<i32>()?;
  Ok(pid)
}

fn ensure_server_not_running(path: &PathBuf) -> Result<(), Box<dyn Error>> {
  if !path.exists() {
    return Ok(());
  }

  let pid = match read_server_pid(path) {
    Ok(pid) => pid,
    Err(_) => {
      let _ = fs::remove_file(path);
      return Ok(());
    }
  };

  if process_exists(pid) {
    return Err(format!("macmon server is already running with pid {}", pid).into());
  }

  let _ = fs::remove_file(path);
  Ok(())
}

fn stop_background_server(path: &PathBuf) -> Result<(), Box<dyn Error>> {
  if !path.exists() {
    return Err("macmon server is not running".into());
  }

  let pid = read_server_pid(path)?;
  if !process_exists(pid) {
    let _ = fs::remove_file(path);
    return Err(format!("stale pid file found for pid {}", pid).into());
  }

  #[cfg(unix)]
  {
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    if rc != 0 {
      return Err(std::io::Error::last_os_error().into());
    }
  }

  let _ = fs::remove_file(path);
  Ok(())
}

fn process_exists(pid: i32) -> bool {
  #[cfg(unix)]
  {
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
  }

  #[cfg(not(unix))]
  {
    let _ = pid;
    false
  }
}
