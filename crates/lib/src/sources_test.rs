use super::{
  CpuDomainInfo, finalize_cpu_freq_domains, init_cpu_freq_domains, parse_cpu_domain_units,
};
use crate::platform::smc::KeyInfo;

fn cached_key_info(cache: &[(u32, KeyInfo)], key: u32) -> Option<KeyInfo> {
  cache.iter().find(|(cached_key, _)| *cached_key == key).map(|(_, info)| *info)
}

#[test]
fn parse_cpu_domain_units_returns_generic_domain_order() {
  assert_eq!(parse_cpu_domain_units(Some("proc 0:8:4")), vec![4, 8]);
  assert_eq!(parse_cpu_domain_units(Some("invalid")), Vec::<u32>::new());
}

#[test]
fn init_cpu_freq_domains_uses_binding_slots() {
  let domains = init_cpu_freq_domains(vec![4, 8]);

  assert_eq!(domains.len(), 3);
  assert_eq!(domains[0].name, "ECPU");
  assert_eq!(domains[0].units, 4);
  assert_eq!(domains[1].name, "PCPU");
  assert_eq!(domains[1].units, 8);
  assert_eq!(domains[2].name, "MCPU");
  assert_eq!(domains[2].units, 0);
}

#[test]
fn finalize_cpu_freq_domains_preserves_public_names_after_filtering() {
  let mut domains = vec![
    CpuDomainInfo { units: 4, freqs: vec![1000, 2000], name: "CPUCL0".to_string() },
    CpuDomainInfo { units: 8, freqs: vec![2000, 3000], name: "CPUCL1".to_string() },
    CpuDomainInfo { units: 0, freqs: vec![], name: "CPUCL2".to_string() },
  ];

  finalize_cpu_freq_domains(&mut domains);

  assert_eq!(domains.len(), 2);
  assert_eq!(domains[0].name, "CPUCL0");
  assert_eq!(domains[0].units, 4);
  assert_eq!(domains[1].name, "CPUCL1");
  assert_eq!(domains[1].units, 8);
}

#[test]
fn finalize_cpu_freq_domains_moves_single_freq_table_without_renaming_domain() {
  let mut domains = vec![
    CpuDomainInfo { units: 0, freqs: vec![1000, 2000], name: "CPUCL0".to_string() },
    CpuDomainInfo { units: 10, freqs: vec![], name: "CPUCL1".to_string() },
  ];

  finalize_cpu_freq_domains(&mut domains);

  assert_eq!(domains.len(), 1);
  assert_eq!(domains[0].name, "CPUCL1");
  assert_eq!(domains[0].units, 10);
  assert_eq!(domains[0].freqs, vec![1000, 2000]);
}

#[test]
fn finalize_cpu_freq_domains_drops_freq_only_domains() {
  let mut domains = vec![
    CpuDomainInfo { units: 4, freqs: vec![1000, 2000], name: "ECPU".to_string() },
    CpuDomainInfo { units: 0, freqs: vec![3000, 4000], name: "MCPU".to_string() },
  ];

  finalize_cpu_freq_domains(&mut domains);

  assert_eq!(domains.len(), 1);
  assert_eq!(domains[0].name, "ECPU");
  assert_eq!(domains[0].units, 4);
  assert_eq!(domains[0].freqs, vec![1000, 2000]);
}

#[test]
fn smc_vec_cache_returns_inserted_key_info() {
  let key = u32::from_be_bytes(*b"TEST");
  let info = KeyInfo { data_size: 4, data_type: 0x666c7420, data_attributes: 0 };
  let mut cache = Vec::new();

  assert_eq!(cached_key_info(&cache, key), None);

  cache.push((key, info));

  assert_eq!(cached_key_info(&cache, key), Some(info));
  assert_eq!(cached_key_info(&cache, key), Some(info));
}
