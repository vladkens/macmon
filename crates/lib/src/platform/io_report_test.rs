use super::scale_energy_to_watts;

#[test]
fn scale_energy_to_watts_uses_sample_duration() {
  let watts = scale_energy_to_watts(5000.0, "mJ", 500).unwrap();
  assert!((watts - 10.0).abs() < 1e-6);
}

#[test]
fn scale_energy_to_watts_rejects_unknown_units() {
  let err = scale_energy_to_watts(1.0, "watts", 1000).unwrap_err();
  assert!(err.to_string().contains("Invalid energy unit"));
}
