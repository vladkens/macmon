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
      "CPU Energy"
        | "GPU Energy"
        | "ANE"
        | "DRAM"
        | "DCS"
        | "Package Energy"
        | "GPU SRAM"
        | "AVE"
        | "VDEC"
        | "ISP"
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
    || is_clpc_energy_channel(group, subgroup, channel, unit)
  {
    return true;
  }

  if group == "Energy Model" {
    return channel == "GPU Energy"
      || channel.ends_with("CPU Energy")
      || channel.starts_with("ANE")
      || channel.starts_with("DRAM")
      || channel.starts_with("DCS");
  }

  if group == "CPU Stats" {
    return subgroup == "CPU Core Performance States";
  }

  group == "GPU Stats" && subgroup == "GPU Performance States"
}

#[cfg(test)]
mod tests {
  use super::ioreport_channels_filter;

  #[test]
  fn subscribes_only_to_known_clpc_energy_counters() {
    for channel in [
      "CPU Energy",
      "GPU Energy",
      "ANE",
      "DRAM",
      "DCS",
      "Package Energy",
      "GPU SRAM",
      "AVE",
      "VDEC",
      "ISP",
    ] {
      assert!(ioreport_channels_filter("CLPC", "Energy Counters", channel, "nJ"));
      assert!(!ioreport_channels_filter("CLPC", "Energy Counters", channel, "events"));
    }
    assert!(!ioreport_channels_filter("CLPC", "Energy Counters", "GPU + SRAM Energy", "nJ"));
    assert!(ioreport_channels_filter("CLPC", "Energy Counters", "ISP", "mJ"));
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

  #[test]
  fn subscribes_to_memory_and_selected_media_components() {
    for name in ["DRAM", "DCS0"] {
      assert!(ioreport_channels_filter("Energy Model", "", name, "mJ"));
    }
    for name in ["MSR", "AFR", "AMCC", "DISP", "GPUCS", "AVE", "VDEC", "ISP", "GPU SRAM"] {
      assert!(!ioreport_channels_filter("Energy Model", "", name, "mJ"));
    }
  }
}
