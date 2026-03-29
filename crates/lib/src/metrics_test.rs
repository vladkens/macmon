use super::{
  CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, Metrics, calc_core_freq_avg,
  calc_freq_from_residencies, distribute_units,
};

fn cpu_usage_entry<'a>(items: &'a [CpuUsageEntry], name: &str) -> Option<&'a CpuUsageEntry> {
  items.iter().find(|entry| entry.name == name)
}

fn gpu_usage_entry<'a>(items: &'a [GpuUsageEntry], name: &str) -> Option<&'a GpuUsageEntry> {
  items.iter().find(|entry| entry.name == name)
}

#[test]
fn calc_freq_with_matching_states() {
  let items = vec![("IDLE".to_string(), 50), ("F1".to_string(), 25), ("F2".to_string(), 15)];
  let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

  assert_eq!(freq, 1375);
  assert!((usage - 0.44444).abs() < 1e-4f32);
}

#[test]
fn calc_freq_with_mismatched_states_matches_legacy_mapping() {
  let items = vec![
    ("IDLE".to_string(), 50),
    ("S1".to_string(), 0),
    ("S2".to_string(), 0),
    ("S3".to_string(), 0),
    ("S4".to_string(), 50),
  ];
  let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

  assert_eq!(freq, 1000);
  assert!((usage - 0.5f32).abs() < 1e-6f32);
}

#[test]
fn calc_freq_with_only_idle_returns_zero_usage() {
  let items = vec![("IDLE".to_string(), 100)];
  let (freq, usage) = calc_freq_from_residencies(&items, &[1200, 3000]);

  assert_eq!(freq, 1200);
  assert_eq!(usage, 0.0);
}

#[test]
fn metrics_preserve_domain_and_core_cpu_usage() {
  let metrics = Metrics {
    cpu_usage: vec![CpuUsageEntry {
      name: "PCPU".to_string(),
      freq_mhz: 3200,
      usage: 0.42,
      cores: vec![
        CoreUsageEntry { freq_mhz: 3100, usage: 0.4 },
        CoreUsageEntry { freq_mhz: 3300, usage: 0.44 },
      ],
    }],
    gpu_usage: vec![GpuUsageEntry {
      name: "GPU".to_string(),
      freq_mhz: 800,
      usage: 0.15,
      units: 10,
    }],
    ..Default::default()
  };

  assert_eq!(
    cpu_usage_entry(&metrics.cpu_usage, "PCPU"),
    Some(&CpuUsageEntry {
      name: "PCPU".to_string(),
      freq_mhz: 3200,
      usage: 0.42,
      cores: vec![
        CoreUsageEntry { freq_mhz: 3100, usage: 0.4 },
        CoreUsageEntry { freq_mhz: 3300, usage: 0.44 },
      ],
    })
  );
  assert_eq!(
    gpu_usage_entry(&metrics.gpu_usage, "GPU"),
    Some(&GpuUsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.15, units: 10 })
  );
  assert!(cpu_usage_entry(&metrics.cpu_usage, "ECPU").is_none());
  assert_eq!(metrics.cpu_usage[0].cores.len(), 2);
  assert_eq!(metrics.cpu_usage[0].cores[0].freq_mhz, 3100);
}

#[test]
fn core_usage_average_returns_domain_average() {
  let cores = vec![
    CoreUsageEntry { freq_mhz: 3000, usage: 0.25 },
    CoreUsageEntry { freq_mhz: 3600, usage: 0.75 },
  ];

  let (freq, usage) = calc_core_freq_avg(&cores, 2400);

  assert_eq!(freq, 3300);
  assert!((usage - 0.5).abs() < 1e-6);
}

#[test]
fn metrics_serialize_with_cli_shape() {
  let metrics = Metrics {
    cpu_usage: vec![CpuUsageEntry {
      name: "ECPU".to_string(),
      freq_mhz: 1181,
      usage: 0.33,
      cores: vec![
        CoreUsageEntry { freq_mhz: 1100, usage: 0.2 },
        CoreUsageEntry { freq_mhz: 1262, usage: 0.46 },
      ],
    }],
    gpu_usage: vec![GpuUsageEntry {
      name: "GPU".to_string(),
      freq_mhz: 461,
      usage: 0.21,
      units: 10,
    }],
    power: super::PowerMetrics {
      package: 0.321,
      cpu: 0.2,
      gpu: 0.01,
      ram: 0.11,
      gpu_ram: 0.001,
      ane: 0.0,
      board: 5.8,
      battery: 0.7,
      dc_in: 0.8,
    },
    memory: super::MemMetrics { ram_total: 1, ram_usage: 2, swap_total: 3, swap_usage: 4 },
    temp: super::TempMetrics { cpu_avg: 42.0, gpu_avg: 36.0 },
  };

  let value = serde_json::to_value(&metrics).unwrap();

  assert_eq!(value["cpu_usage"]["ECPU"]["units"], serde_json::json!(2));
  assert_eq!(value["cpu_usage"]["ECPU"]["freq_mhz"], serde_json::json!(1181));
  assert!((value["cpu_usage"]["ECPU"]["usage"].as_f64().unwrap() - 0.33).abs() < 1e-6);
  assert_eq!(value["cpu_usage"]["ECPU"]["cores"][0][0], serde_json::json!(1100));
  assert!((value["cpu_usage"]["ECPU"]["cores"][0][1].as_f64().unwrap() - 0.2).abs() < 1e-6);
  assert_eq!(value["gpu_usage"]["GPU"]["freq_mhz"], serde_json::json!(461));
  assert_eq!(value["gpu_usage"]["GPU"]["units"], serde_json::json!(10));
  assert!((value["power"]["package"].as_f64().unwrap() - 0.321).abs() < 1e-6);
  assert_eq!(value["memory"]["swap_usage"], serde_json::json!(4));
  assert!((value["temp"]["cpu_avg"].as_f64().unwrap() - 42.0).abs() < 1e-6);
}

#[test]
fn distribute_units_preserves_input_order() {
  let units = distribute_units(&["ECPU".to_string(), "PCPU".to_string(), "PCPU1".to_string()], 10);

  assert_eq!(
    units,
    vec![("ECPU".to_string(), 4), ("PCPU".to_string(), 3), ("PCPU1".to_string(), 3),]
  );
}
