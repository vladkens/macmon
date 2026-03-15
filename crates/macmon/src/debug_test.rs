use super::*;
use macmon_lib::metrics::{CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, Metrics, UsageMetrics};
use macmon_lib::sources::{CpuDomainInfo, SocInfo};

#[test]
fn debug_output_nests_soc_and_metrics() {
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

  let value = serde_json::to_value(DebugOutput { soc: &soc, metrics: &metrics }).unwrap();

  assert_eq!(value["soc"]["chip_name"], serde_json::json!("Apple M4"));
  assert_eq!(value["metrics"]["usage"]["PCPU"]["freq_mhz"], serde_json::json!(3200));
  assert_eq!(value["metrics"]["usage"]["gpu"]["GPU"]["units"], serde_json::json!(10));
}
