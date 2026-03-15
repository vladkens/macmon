use super::*;
use macmon_lib::metrics::{CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, Metrics, UsageMetrics};
use macmon_lib::sources::{CpuDomainInfo, SocInfo};

#[test]
fn pipe_sample_flattens_metrics_and_optional_soc() {
  let metrics = Metrics {
    usage: UsageMetrics {
      cpu: vec![CpuUsageEntry {
        name: "PCPU".to_string(),
        freq_mhz: 3200,
        usage: 0.5,
        cores: vec![CoreUsageEntry { freq_mhz: 3100, usage: 0.4 }],
      }],
      gpu: vec![GpuUsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.2, units: 10 }],
    },
    ..Default::default()
  };
  let soc = SocInfo {
    mac_model: "Mac16,1".to_string(),
    chip_name: "Apple M4".to_string(),
    memory_gb: 24,
    cpu_cores_total: 10,
    cpu_domains: vec![CpuDomainInfo {
      name: "PCPU".to_string(),
      units: 10,
      freqs_mhz: vec![3000, 4000],
    }],
    gpu_cores: 10,
    gpu_freqs_mhz: vec![500, 1000],
  };

  let value = serde_json::to_value(PipeSample {
    timestamp: "2026-03-15T10:00:00Z",
    metrics: &metrics,
    soc: Some(&soc),
  })
  .unwrap();

  assert_eq!(value["timestamp"], serde_json::json!("2026-03-15T10:00:00Z"));
  assert_eq!(value["usage"]["PCPU"]["units"], serde_json::json!(1));
  assert_eq!(value["usage"]["PCPU"]["freq_mhz"], serde_json::json!(3200));
  assert!((value["usage"]["PCPU"]["usage"].as_f64().unwrap() - 0.5).abs() < 1e-6);
  assert_eq!(value["usage"]["PCPU"]["cores"][0][0], serde_json::json!(3100));
  assert!((value["usage"]["PCPU"]["cores"][0][1].as_f64().unwrap() - 0.4).abs() < 1e-6);
  assert_eq!(value["usage"]["gpu"]["GPU"]["units"], serde_json::json!(10));
  assert_eq!(value["usage"]["gpu"]["GPU"]["freq_mhz"], serde_json::json!(800));
  assert!((value["usage"]["gpu"]["GPU"]["usage"].as_f64().unwrap() - 0.2).abs() < 1e-6);
  assert_eq!(value["soc"]["cpu_domains"][0]["freqs_mhz"][1], serde_json::json!(4000));
  assert_eq!(value["soc"]["gpu_freqs_mhz"][1], serde_json::json!(1000));
}
