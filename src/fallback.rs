//! Backup power sources, in case macOS breaks the usual ones: read only when those have no
//! reading. Ported from [mactop](https://github.com/metaspartan/mactop) (MIT).
//! Copyright (c) 2024-2026 Carsen Klock, MIT License

use crate::shared::zero_div;
use crate::sources::SMC;

/// PMP `Energy` histograms of the CPU clusters, also in the per-die groups `PMP0`, `PMP1`, …:
/// "EACC0", "PACC0", "MACC1", "PACC0 SRAM".
pub(crate) fn is_pmp_cpu_channel(group: &str, subgroup: &str, channel: &str) -> bool {
  let die = group.strip_prefix("PMP").is_some_and(|x| x.bytes().all(|x| x.is_ascii_digit()));
  let name = channel.strip_suffix(" SRAM").unwrap_or(channel).as_bytes();
  let cluster = name.len() >= 4
    && name[0].is_ascii_uppercase()
    && &name[1..4] == b"ACC"
    && name[4..].iter().all(u8::is_ascii_digit);
  die && subgroup == "Energy" && cluster
}

/// SMC DRAM power keys, one per die: "PZD1", "PZD2", …
pub(crate) fn is_ram_key(name: &str) -> bool {
  name.len() == 4 && name.starts_with("PZD") && matches!(name.as_bytes()[3], b'1'..=b'9')
}

/// The first of `watts`, then `smc`, above 0 and not a catch-up spike, else the first reading.
/// `smc` is read only when needed.
pub(crate) fn first_valid(
  watts: [Option<f32>; 2],
  smc: impl FnOnce() -> Option<f32>,
  sys: Option<f32>,
) -> Option<f32> {
  let watts = watts.into_iter().chain(std::iter::once_with(smc)).flatten();
  first_positive(watts.map(|x| if is_spike(x, sys) { 0.0 } else { x }))
}

/// The first of `watts` above 0, else the first one.
fn first_positive(watts: impl IntoIterator<Item = f32>) -> Option<f32> {
  let mut first = None;
  for x in watts {
    if x > 0.0 {
      return Some(x);
    }

    first = first.or(Some(x));
  }

  first
}

/// Whether `watts` is a catch-up spike of a frozen `Energy Model`, far above the SMC system total
/// `sys`. The margin is wide, as `sys` lags behind a load step. A total of 0 is no reading.
fn is_spike(watts: f32, sys: Option<f32>) -> bool {
  sys.is_some_and(|sys| sys > 0.0 && watts > 10.0 * sys + 100.0)
}

/// Watts of a PMP power histogram: the residency-weighted average of its bins ("0.250W", "1W",
/// …), the lowest bin, where an idle cluster sits, counted as 0. None when the states aren't bins.
pub(crate) fn histogram_watts(residencies: &[(String, i64)]) -> Option<f32> {
  if residencies.len() < 2 {
    return None;
  }

  let mut total = 0.0;
  let mut watts = 0.0;
  for (i, (name, residency)) in residencies.iter().enumerate() {
    let bin: f64 = name.trim().strip_suffix('W')?.parse().ok()?;
    if !bin.is_finite() || bin < 0.0 || *residency < 0 {
      return None;
    }

    total += *residency as f64;
    if i > 0 {
      watts += bin * *residency as f64;
    }
  }

  Some(zero_div(watts, total) as f32)
}

/// CPU power from the histogram watts of the `PMP` group's channels, else of the per-die groups'
/// (multi-die chips leave `PMP` empty), as `(in_pmp, watts)` pairs. See `total_power`.
pub(crate) fn pmp_cpu_watts(channels: &[(bool, Option<f32>)]) -> Option<f32> {
  let bare = channels.iter().any(|&(in_pmp, _)| in_pmp);
  total_power(channels.iter().filter(|&&(in_pmp, _)| in_pmp == bare).map(|&(_, watts)| watts))
}

/// The sum of `watts`, None without any or when one is unreadable: part of the total is no total.
fn total_power(watts: impl IntoIterator<Item = Option<f32>>) -> Option<f32> {
  let mut watts = watts.into_iter().peekable();
  watts.peek()?;
  watts.map(|x| x.filter(|x| x.is_finite() && *x >= 0.0)).sum()
}

/// System power: `PSTR`, on a Mac without a battery then `PDTR` and `PD0R` (Mac Studio reads 0
/// from the first two), the first above 0, else the first reading.
pub(crate) fn sys_power(smc: &mut SMC, battery: bool) -> Option<f32> {
  let keys = if battery { &["PSTR"][..] } else { &["PSTR", "PDTR", "PD0R"] };
  first_positive(keys.iter().filter_map(|x| smc.read_float_val(x).ok()))
}

/// DRAM power: the sum of the per-die `keys`, see `total_power`.
pub(crate) fn smc_ram_power(smc: &mut SMC, keys: &[String]) -> Option<f32> {
  total_power(keys.iter().map(|x| smc.read_float_val(x).ok()))
}

#[cfg(test)]
mod tests {
  use super::{histogram_watts, is_pmp_cpu_channel, is_ram_key, pmp_cpu_watts, total_power};

  #[test]
  fn pmp_cpu_channels_are_cluster_power_histograms() {
    for channel in ["EACC0", "PACC0", "MACC1", "PACC", "PACC0 SRAM", "EACC0 SRAM"] {
      assert!(is_pmp_cpu_channel("PMP", "Energy", channel), "{channel}");
      assert!(is_pmp_cpu_channel("PMP1", "Energy", channel), "{channel}");
    }
    for channel in ["AGX", "AGX SRAM", "pACC0", "PACC0 DTL", "PACC0X", "ACC"] {
      assert!(!is_pmp_cpu_channel("PMP", "Energy", channel), "{channel}");
    }
    assert!(!is_pmp_cpu_channel("PMPX", "Energy", "PACC0"));
    assert!(!is_pmp_cpu_channel("PMP", "Energy Counters", "PACC0"));
  }

  #[test]
  fn ram_keys_are_per_die() {
    assert!(is_ram_key("PZD1") && is_ram_key("PZD9"));
    assert!(
      !is_ram_key("PZD0") && !is_ram_key("PZDT") && !is_ram_key("PZD") && !is_ram_key("PZC1")
    );
  }

  #[test]
  fn histogram_watts_counts_the_lowest_bin_as_zero() {
    let bins = |x: &[(&str, i64)]| x.iter().map(|(n, r)| (n.to_string(), *r)).collect::<Vec<_>>();
    // PACC0 of an M5: 1 W at rest, then 4 and 6 W
    assert_eq!(histogram_watts(&bins(&[("   1W", 2), ("4W", 1), ("6W", 1)])), Some(2.5));
    assert_eq!(histogram_watts(&bins(&[("0.250W", 3), ("0.500W", 1)])), Some(0.125));
    assert_eq!(histogram_watts(&bins(&[("0.250W", 0), ("0.500W", 0)])), Some(0.0));
    assert_eq!(histogram_watts(&bins(&[("1W", 1)])), None);
    assert_eq!(histogram_watts(&bins(&[("IDLE", 1), ("V0P5", 1)])), None);
    assert_eq!(histogram_watts(&bins(&[("1W", 1), ("NaNW", 1)])), None);
    assert_eq!(histogram_watts(&bins(&[("1W", 1), ("2W", -1)])), None);
  }

  #[test]
  fn pmp_cpu_uses_the_pmp_group_else_the_dies_and_every_channel() {
    assert_eq!(pmp_cpu_watts(&[]), None);
    assert_eq!(pmp_cpu_watts(&[(true, Some(1.0)), (true, Some(2.0))]), Some(3.0));
    assert_eq!(pmp_cpu_watts(&[(true, Some(1.0)), (false, Some(5.0))]), Some(1.0));
    assert_eq!(pmp_cpu_watts(&[(false, Some(1.0)), (false, Some(5.0))]), Some(6.0));
    assert_eq!(pmp_cpu_watts(&[(true, Some(1.0)), (true, None)]), None);
  }

  #[test]
  fn total_power_needs_every_reading() {
    assert_eq!(total_power([Some(0.4), Some(0.6)]), Some(1.0));
    assert_eq!(total_power([Some(0.0)]), Some(0.0));
    for watts in [vec![], vec![Some(0.4), None], vec![Some(0.4), Some(-1.0)], vec![Some(f32::NAN)]]
    {
      assert_eq!(total_power(watts), None);
    }
  }
}
