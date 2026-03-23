use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;

use crate::metrics::{Metrics, Sampler};
use crate::sources::SocInfo;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Serialize)]
pub struct StatsSnapshot {
  pub timestamp: String,
  pub soc: SocInfo,
  #[serde(flatten)]
  pub metrics: Metrics,
}

#[derive(Debug, Clone, Serialize)]
struct HealthResponse {
  status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorResponse {
  error: String,
}

#[derive(Debug, Clone, Serialize)]
struct RootResponse {
  name: &'static str,
  endpoints: [&'static str; 3],
}

#[derive(Debug, Clone)]
enum SharedState {
  Ready(StatsSnapshot),
  Error(String),
}

pub fn collect_snapshot(sampler: &mut Sampler, interval: u32, include_soc: bool) -> WithError<serde_json::Value> {
  let metrics = sampler.get_metrics(interval.max(100))?;
  let mut doc = serde_json::to_value(&metrics)?;

  if include_soc {
    doc["soc"] = serde_json::to_value(sampler.get_soc_info())?;
  }

  doc["timestamp"] = serde_json::to_value(Utc::now().to_rfc3339())?;
  Ok(doc)
}

pub fn run_server(host: &str, port: u16, interval: u32) -> WithError<()> {
  let mut sampler = Sampler::new()?;
  let interval = interval.max(100);
  let state = Arc::new(RwLock::new(SharedState::Ready(build_snapshot(&mut sampler, interval)?)));

  let listener = TcpListener::bind((host, port))?;
  listener.set_nonblocking(true)?;

  loop {
    loop {
      match listener.accept() {
        Ok((stream, _)) => {
          let state = state.clone();
          thread::spawn(move || {
            if let Err(err) = handle_connection(stream, &state) {
              eprintln!("api connection error: {}", err);
            }
          });
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
        Err(err) => {
          eprintln!("api accept error: {}", err);
          break;
        }
      }
    }

    let next = match build_snapshot(&mut sampler, interval) {
      Ok(snapshot) => SharedState::Ready(snapshot),
      Err(err) => SharedState::Error(err.to_string()),
    };
    *state.write().unwrap() = next;

    thread::sleep(Duration::from_millis(20));
  }
}

fn build_snapshot(sampler: &mut Sampler, interval: u32) -> WithError<StatsSnapshot> {
  Ok(StatsSnapshot {
    timestamp: Utc::now().to_rfc3339(),
    soc: sampler.get_soc_info().clone(),
    metrics: sampler.get_metrics(interval.max(100))?,
  })
}

fn handle_connection(mut stream: TcpStream, state: &Arc<RwLock<SharedState>>) -> WithError<()> {
  let mut reader = BufReader::new(stream.try_clone()?);
  let mut request_line = String::new();
  reader.read_line(&mut request_line)?;

  let path = request_line.split_whitespace().nth(1).unwrap_or("/");

  let (status, body) = match path {
    "/" => (
      "200 OK",
      serde_json::to_vec(&RootResponse {
        name: "macmon",
        endpoints: ["/health", "/stats", "/api/v1/stats"],
      })?,
    ),
    "/health" => ("200 OK", serde_json::to_vec(&HealthResponse { status: "ok" })?),
    "/stats" | "/api/v1/stats" => match &*state.read().unwrap() {
      SharedState::Ready(snapshot) => ("200 OK", serde_json::to_vec(snapshot)?),
      SharedState::Error(err) => (
        "503 Service Unavailable",
        serde_json::to_vec(&ErrorResponse { error: err.clone() })?,
      ),
    },
    _ => (
      "404 Not Found",
      serde_json::to_vec(&ErrorResponse { error: "not found".to_string() })?,
    ),
  };

  write!(
    stream,
    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
    body.len()
  )?;
  stream.write_all(&body)?;
  stream.flush()?;

  Ok(())
}
