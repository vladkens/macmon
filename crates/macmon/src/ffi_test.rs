use super::*;

#[test]
fn usage_metrics_serialize_with_cpu_cores_map() {
  let usage = UsageMetrics {
    cpu: vec![CpuUsageEntry {
      name: "PCPU".to_string(),
      freq_mhz: 3200,
      usage: 0.5,
      units: 8,
      cores: vec![CoreUsageEntry { freq_mhz: 3100, usage: 0.4 }],
    }],
    gpu: vec![GpuUsageEntry {
      name: "GPU".to_string(),
      freq_mhz: 800,
      usage: 0.2,
      units: 10,
    }],
  };

  let value = serde_json::to_value(&usage).unwrap();

  assert_eq!(value["PCPU"]["units"], serde_json::json!(8));
  assert_eq!(value["PCPU"]["freq_mhz"], serde_json::json!(3200));
  assert!((value["PCPU"]["usage"].as_f64().unwrap() - 0.5).abs() < 1e-6);
  assert_eq!(value["PCPU"]["cores"][0][0], serde_json::json!(3100));
  assert!((value["PCPU"]["cores"][0][1].as_f64().unwrap() - 0.4).abs() < 1e-6);
  assert_eq!(value["gpu"]["GPU"]["units"], serde_json::json!(10));
  assert_eq!(value["gpu"]["GPU"]["freq_mhz"], serde_json::json!(800));
  assert!((value["gpu"]["GPU"]["usage"].as_f64().unwrap() - 0.2).abs() < 1e-6);
}
