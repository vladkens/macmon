use super::{
  Metrics, UsageEntry, UsageMetrics, calc_freq_from_residencies, cluster_key, cluster_usage_from_cores,
  cpu_domain_for_channel, distribute_units, sort_cluster_names, sort_indexed_samples_by_core,
};
use crate::sources::CpuDomainInfo;

fn usage_entry<'a>(items: &'a [UsageEntry], name: &str) -> Option<&'a UsageEntry> {
  items.iter().find(|entry| entry.name == name)
}

#[test]
fn calc_freq_with_matching_states() {
  let items = vec![("IDLE".to_string(), 50), ("F1".to_string(), 25), ("F2".to_string(), 25)];
  let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

  assert_eq!(freq, 1500);
  assert!((usage - 0.375f32).abs() < 1e-6f32);
}

#[test]
fn calc_freq_with_mismatched_states_uses_tail_activity() {
  let items = vec![
    ("IDLE".to_string(), 50),
    ("S1".to_string(), 0),
    ("S2".to_string(), 0),
    ("S3".to_string(), 0),
    ("S4".to_string(), 50),
  ];
  let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

  assert_eq!(freq, 2000);
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
fn cluster_keys_are_raw_and_stable() {
  assert_eq!(cluster_key("ECPU"), Some("ECPU".to_string()));
  assert_eq!(cluster_key("PCPU1"), Some("PCPU1".to_string()));
  assert_eq!(cluster_key("GPUPH"), Some("GPUPH".to_string()));
  assert_eq!(cluster_key("GPU-2"), Some("GPU2".to_string()));
}

#[test]
fn cpu_domain_matching_uses_domain_metadata() {
  let domains = vec![
    CpuDomainInfo {
      name: "ECPU".to_string(),
      units: 4,
      freqs: vec![1000, 2000],
      core_prefix: "ECPU".to_string(),
    },
    CpuDomainInfo {
      name: "PCPU".to_string(),
      units: 8,
      freqs: vec![2000, 3000],
      core_prefix: "PCPU".to_string(),
    },
  ];

  assert_eq!(
    cpu_domain_for_channel(&domains, "PCPU1").map(|domain| domain.name.as_str()),
    Some("PCPU")
  );
  assert_eq!(
    cpu_domain_for_channel(&domains, "ECPU").map(|domain| domain.name.as_str()),
    Some("ECPU")
  );
}

#[test]
fn metrics_preserve_missing_cpu_clusters() {
  let metrics = Metrics {
    usage: UsageMetrics {
      cpu: vec![UsageEntry { name: "PCPU".to_string(), freq_mhz: 3200, usage: 0.42, units: 4 }],
      gpu: vec![UsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.15, units: 10 }],
    },
    ..Default::default()
  };

  assert_eq!(
    usage_entry(&metrics.usage.cpu, "PCPU"),
    Some(&UsageEntry { name: "PCPU".to_string(), freq_mhz: 3200, usage: 0.42, units: 4 })
  );
  assert_eq!(
    usage_entry(&metrics.usage.gpu, "GPU"),
    Some(&UsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.15, units: 10 })
  );
  assert!(usage_entry(&metrics.usage.cpu, "ECPU").is_none());
}

#[test]
fn metrics_preserve_dynamic_cluster_names() {
  let metrics = Metrics {
    usage: UsageMetrics {
      cpu: vec![
        UsageEntry { name: "PCPU".to_string(), freq_mhz: 3030, usage: 0.31, units: 4 },
        UsageEntry { name: "PCPU1".to_string(), freq_mhz: 3220, usage: 0.44, units: 4 },
      ],
      gpu: vec![UsageEntry { name: "GPUPH".to_string(), freq_mhz: 1296, usage: 0.2, units: 16 }],
    },
    ..Default::default()
  };

  assert_eq!(metrics.usage.cpu.len(), 2);
  assert_eq!(
    usage_entry(&metrics.usage.cpu, "PCPU"),
    Some(&UsageEntry { name: "PCPU".to_string(), freq_mhz: 3030, usage: 0.31, units: 4 })
  );
  assert_eq!(
    usage_entry(&metrics.usage.cpu, "PCPU1"),
    Some(&UsageEntry { name: "PCPU1".to_string(), freq_mhz: 3220, usage: 0.44, units: 4 })
  );
  assert_eq!(
    usage_entry(&metrics.usage.gpu, "GPUPH"),
    Some(&UsageEntry { name: "GPUPH".to_string(), freq_mhz: 1296, usage: 0.2, units: 16 })
  );
}

#[test]
fn metrics_serialize_with_expected_shape() {
  let metrics = Metrics {
    usage: UsageMetrics {
      cpu: vec![UsageEntry { name: "ECPU".to_string(), freq_mhz: 1181, usage: 0.33, units: 4 }],
      gpu: vec![UsageEntry { name: "GPU".to_string(), freq_mhz: 461, usage: 0.21, units: 10 }],
    },
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

  assert_eq!(value["usage"]["cpu"][0]["name"], serde_json::json!("ECPU"));
  assert_eq!(value["usage"]["cpu"][0]["freq_mhz"], serde_json::json!(1181));
  assert!((value["usage"]["cpu"][0]["usage"].as_f64().unwrap() - 0.33).abs() < 1e-6);
  assert_eq!(value["usage"]["cpu"][0]["units"], serde_json::json!(4));
  assert_eq!(value["usage"]["gpu"][0]["name"], serde_json::json!("GPU"));
  assert_eq!(value["usage"]["gpu"][0]["freq_mhz"], serde_json::json!(461));
  assert!((value["usage"]["gpu"][0]["usage"].as_f64().unwrap() - 0.21).abs() < 1e-6);
  assert_eq!(value["usage"]["gpu"][0]["units"], serde_json::json!(10));
  assert!((value["power"]["cpu"].as_f64().unwrap() - 0.2).abs() < 1e-6);
  assert!((value["power"]["package"].as_f64().unwrap() - 0.321).abs() < 1e-6);
  assert!((value["power"]["board"].as_f64().unwrap() - 5.8).abs() < 1e-6);
  assert!((value["power"]["battery"].as_f64().unwrap() - 0.7).abs() < 1e-6);
  assert!((value["power"]["dc_in"].as_f64().unwrap() - 0.8).abs() < 1e-6);
  assert_eq!(value["memory"]["swap_usage"], serde_json::json!(4));
  assert!((value["temp"]["cpu_avg"].as_f64().unwrap() - 42.0).abs() < 1e-6);
}

#[test]
fn cluster_usage_is_averaged_across_cluster_units() {
  let cores = vec![
    (0, (4512, 1.0)),
    (1, (1260, 0.0)),
    (2, (1260, 0.0)),
    (3, (1260, 0.0)),
    (4, (1260, 0.0)),
    (5, (1260, 0.0)),
    (6, (1260, 0.0)),
    (7, (1260, 0.0)),
    (8, (1260, 0.0)),
    (9, (1260, 0.0)),
  ];
  let cluster_units = vec![("PCPU".to_string(), 5), ("PCPU1".to_string(), 5)];
  let cluster_names = vec!["PCPU".to_string(), "PCPU1".to_string()];

  let usage = cluster_usage_from_cores(&cores, &cluster_names, &cluster_units);

  assert_eq!(usage.iter().find(|(name, _)| name == "PCPU").map(|(_, usage)| *usage), Some(0.2));
  assert_eq!(usage.iter().find(|(name, _)| name == "PCPU1").map(|(_, usage)| *usage), Some(0.0));
}

#[test]
fn cluster_names_are_sorted_naturally() {
  let mut clusters = vec!["PCPU10".to_string(), "PCPU2".to_string(), "PCPU".to_string()];

  sort_cluster_names(&mut clusters);

  assert_eq!(clusters, vec!["PCPU".to_string(), "PCPU2".to_string(), "PCPU10".to_string()]);
}

#[test]
fn distribute_units_preserves_input_order() {
  let units = distribute_units(&["ECPU".to_string(), "PCPU".to_string(), "PCPU1".to_string()], 10);

  assert_eq!(
    units,
    vec![
      ("ECPU".to_string(), 4),
      ("PCPU".to_string(), 3),
      ("PCPU1".to_string(), 3),
    ]
  );
}

#[test]
fn core_samples_are_sorted_before_cluster_aggregation() {
  let mut cores = vec![(5, (1260, 0.0)), (0, (4512, 1.0)), (1, (1260, 0.0))];
  sort_indexed_samples_by_core(&mut cores);

  assert_eq!(cores.iter().map(|(idx, _)| *idx).collect::<Vec<_>>(), vec![0, 1, 5]);
}
