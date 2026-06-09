pub(crate) fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  if b == zero { zero } else { a / b }
}

pub(crate) fn ioreport_channels_filter(
  group: &str,
  subgroup: &str,
  channel: &str,
  _unit: &str,
) -> bool {
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
