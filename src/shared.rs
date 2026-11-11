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

pub(crate) fn is_pmp_mem_channel(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
  group == "PMP"
    && subgroup == "Energy Counters"
    && (channel.starts_with("DRAM") || channel.starts_with("DCS"))
    && matches!(unit, "mJ" | "uJ" | "nJ")
}

/// CPU cluster power histograms of PMP, also in the per-die groups `PMP0`, `PMP1`, … of M5 Max
/// and Ultra: "EACC0", "PACC0", "MACC1", "PACC0 SRAM" (after metaspartan/mactop, MIT).
pub(crate) fn is_pmp_cpu_channel(group: &str, subgroup: &str, channel: &str) -> bool {
  let die = group.strip_prefix("PMP").is_some_and(|x| x.bytes().all(|x| x.is_ascii_digit()));
  let name = channel.strip_suffix(" SRAM").unwrap_or(channel).as_bytes();
  let cluster = name.len() >= 4
    && name[0].is_ascii_uppercase()
    && &name[1..4] == b"ACC"
    && name[4..].iter().all(u8::is_ascii_digit);
  die && subgroup == "Energy" && cluster
}

pub(crate) fn is_clpc_energy_channel(
  group: &str,
  subgroup: &str,
  channel: &str,
  unit: &str,
) -> bool {
  group == "CLPC"
    && subgroup == "Energy Counters"
    && matches!(
      channel,
      "CPU Energy" | "GPU Energy" | "ANE" | "DRAM" | "DCS" | "Package Energy" | "AVE" | "ISP"
    )
    && matches!(unit, "nJ" | "mJ")
}

pub(crate) fn ioreport_channels_filter(
  group: &str,
  subgroup: &str,
  channel: &str,
  unit: &str,
) -> bool {
  if is_pmp_ane_channel(group, subgroup, channel, unit)
    || is_pmp_mem_channel(group, subgroup, channel, unit)
    || is_clpc_energy_channel(group, subgroup, channel, unit)
  {
    return true;
  }

  if group == "Energy Model" {
    return channel == "GPU Energy"
      || channel.ends_with("CPU Energy")
      || channel.starts_with("ANE")
      || channel.starts_with("DRAM")
      || channel.starts_with("DCS")
      || channel.starts_with("GPU SRAM");
  }

  if group == "CPU Stats" {
    return subgroup == "CPU Core Performance States";
  }

  group == "GPU Stats" && subgroup == "GPU Performance States"
}

/// `ioreport_channels_filter` with the PMP CPU histograms, for CPU power without CLPC counters.
pub(crate) fn ioreport_channels_filter_pmp_cpu(
  group: &str,
  subgroup: &str,
  channel: &str,
  unit: &str,
) -> bool {
  ioreport_channels_filter(group, subgroup, channel, unit)
    || is_pmp_cpu_channel(group, subgroup, channel)
}

#[cfg(test)]
mod tests {
  use super::{ioreport_channels_filter, ioreport_channels_filter_pmp_cpu, is_pmp_cpu_channel};

  #[test]
  fn pmp_cpu_channels_are_cluster_power_histograms() {
    for channel in ["EACC0", "PACC0", "MACC1", "PACC", "PACC0 SRAM", "EACC0 SRAM"] {
      assert!(is_pmp_cpu_channel("PMP", "Energy", channel), "{channel}");
      assert!(is_pmp_cpu_channel("PMP1", "Energy", channel), "{channel}");
      assert!(ioreport_channels_filter_pmp_cpu("PMP", "Energy", channel, ""));
      assert!(!ioreport_channels_filter("PMP", "Energy", channel, ""));
    }
    for channel in ["AGX", "AGX SRAM", "pACC0", "PACC0 DTL", "PACC0X", "ACC"] {
      assert!(!is_pmp_cpu_channel("PMP", "Energy", channel), "{channel}");
    }
    assert!(!is_pmp_cpu_channel("PMPX", "Energy", "PACC0"));
    assert!(!is_pmp_cpu_channel("PMP", "Energy Counters", "PACC0"));
  }

  #[test]
  fn subscribes_only_to_known_clpc_energy_counters() {
    for channel in ["CPU Energy", "GPU Energy", "ANE", "DRAM", "DCS", "Package Energy", "ISP"] {
      assert!(ioreport_channels_filter("CLPC", "Energy Counters", channel, "nJ"));
      assert!(!ioreport_channels_filter("CLPC", "Energy Counters", channel, "events"));
    }
    assert!(ioreport_channels_filter("CLPC", "Energy Counters", "AVE", "mJ"));
    assert!(!ioreport_channels_filter("CLPC", "Energy Counters", "VDEC", "nJ"));
    assert!(!ioreport_channels_filter("CLPC", "Performance States", "ANE", "nJ"));
  }

  #[test]
  fn subscribes_to_ane_energy_without_bandwidth_or_state_counters() {
    assert!(ioreport_channels_filter("Energy Model", "", "ANE", "mJ"));
    assert!(ioreport_channels_filter("PMP", "Energy Counters", "ANE", "mJ"));
    assert!(!ioreport_channels_filter("PMP", "Bandwidth", "ANE RD", "events"));
    assert!(!ioreport_channels_filter("PMP", "Energy Counters", "ANE", "events"));
    assert!(!ioreport_channels_filter("H11ANE", "H11ANE Power State", "ANE Power", ""));
  }

  #[test]
  fn subscribes_to_pmp_dram_total_without_its_frequency_split() {
    for channel in ["DRAM", "DCS"] {
      assert!(ioreport_channels_filter("Energy Model", "", channel, "mJ"));
      assert!(ioreport_channels_filter("PMP", "Energy Counters", channel, "mJ"));
    }
    assert!(!ioreport_channels_filter("PMP", "Energy Counters", "DRAM", "events"));
    assert!(!ioreport_channels_filter("PMP", "Bandwidth", "DRAM", "mJ"));
    for channel in ["F0", "F1", "F2", "DRAM"] {
      assert!(!ioreport_channels_filter("PMP", "DRAM Energy", channel, "mJ"));
    }
  }
}
