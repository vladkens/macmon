//! Shared library helpers.

pub(crate) fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  if b == zero { zero } else { a / b }
}

pub(crate) fn is_pmp_ane_channel(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
  group == "PMP"
    && subgroup == "Energy Counters"
    && channel.starts_with("ANE")
    && matches!(unit, "mJ" | "uJ" | "nJ")
}

pub(crate) fn is_pmp_cpu_channel(group: &str, subgroup: &str, channel: &str) -> bool {
  let numbered = |name: &str, prefix: &str| {
    name.strip_prefix(prefix).is_some_and(|rest| rest.bytes().all(|c| c.is_ascii_digit()))
  };
  numbered(group, "PMP")
    && subgroup == "Energy"
    && ["EACC", "PACC", "MACC"].iter().any(|prefix| numbered(channel, prefix))
}

pub(crate) fn is_clpc_energy_channel(
  group: &str,
  subgroup: &str,
  channel: &str,
  unit: &str,
) -> bool {
  group == "CLPC"
    && subgroup == "Energy Counters"
    && matches!(channel, "CPU Energy" | "GPU Energy" | "ANE")
    && unit == "nJ"
}

pub(crate) fn ioreport_channels_filter(
  group: &str,
  subgroup: &str,
  channel: &str,
  unit: &str,
) -> bool {
  if is_pmp_ane_channel(group, subgroup, channel, unit)
    || is_clpc_energy_channel(group, subgroup, channel, unit)
    || is_pmp_cpu_channel(group, subgroup, channel)
  {
    return true;
  }

  if group == "Energy Model" {
    return channel == "GPU Energy"
      || channel.ends_with("CPU Energy")
      || channel.starts_with("ANE")
      || channel.starts_with("DRAM")
      || channel.starts_with("GPU SRAM");
  }

  if group == "CPU Stats" {
    return subgroup == "CPU Core Performance States";
  }

  group == "GPU Stats" && subgroup == "GPU Performance States"
}

#[cfg(test)]
mod tests {
  use super::{ioreport_channels_filter, is_pmp_cpu_channel};

  #[test]
  fn recognizes_cpu_histograms_without_model_or_die_limits() {
    for group in ["PMP", "PMP0", "PMP1", "PMP12"] {
      for name in ["EACC", "EACC0", "PACC", "PACC12", "MACC", "MACC3"] {
        assert!(is_pmp_cpu_channel(group, "Energy", name));
        assert!(ioreport_channels_filter(group, "Energy", name, "events"));
      }
    }
    for (group, subgroup, name) in [
      ("PMPX", "Energy", "PACC0"),
      ("PMP-1", "Energy", "PACC0"),
      ("PMP", "Bandwidth", "PACC0"),
      ("PMP", "Energy", "PACC SRAM"),
      ("PMP", "Energy", "PACC0 SRAM"),
      ("PMP", "Energy", "AGX"),
      ("PMP", "Energy", "ANE"),
      ("PMP", "Energy", "PACC0 stall"),
      ("PMP", "Energy", "PACC١"),
      ("PMP", "Energy Counters", "PACC0"),
    ] {
      assert!(!is_pmp_cpu_channel(group, subgroup, name));
    }
  }

  #[test]
  fn subscribes_only_to_known_clpc_energy_counters() {
    for channel in ["CPU Energy", "GPU Energy", "ANE"] {
      assert!(ioreport_channels_filter("CLPC", "Energy Counters", channel, "nJ"));
      assert!(!ioreport_channels_filter("CLPC", "Energy Counters", channel, "events"));
    }
    assert!(!ioreport_channels_filter("CLPC", "Energy Counters", "DRAM", "nJ"));
    assert!(!ioreport_channels_filter("CLPC", "Performance States", "ANE", "nJ"));
  }

  #[test]
  fn subscribes_to_ane_energy_without_bandwidth_or_state_counters() {
    assert!(ioreport_channels_filter("Energy Model", "", "ANE", "mJ"));
    assert!(ioreport_channels_filter("PMP", "Energy Counters", "ANE", "mJ"));
    assert!(!ioreport_channels_filter("PMP", "Bandwidth", "ANE RD", "events"));
    assert!(!ioreport_channels_filter("PMP", "Energy Counters", "ANE", "events"));
    assert!(!ioreport_channels_filter("H11ANE", "H11ANE Power State", "ANE Power", ""));
    assert!(!ioreport_channels_filter("PMP", "Energy Counters", "DRAM", "mJ"));
  }
}
