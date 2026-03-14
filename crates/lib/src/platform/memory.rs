use super::WithError;

pub fn libc_ram() -> WithError<(u64, u64)> {
  let (mut usage, mut total) = (0u64, 0u64);

  unsafe {
    let mut name = [libc::CTL_HW, libc::HW_MEMSIZE];
    let mut size = std::mem::size_of::<u64>();
    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut total as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get total memory".into());
    }
  }

  unsafe {
    let mut count: u32 = libc::HOST_VM_INFO64_COUNT as _;
    let mut stats = std::mem::zeroed::<libc::vm_statistics64>();

    #[allow(deprecated)]
    let ret_code = libc::host_statistics64(
      libc::mach_host_self(),
      libc::HOST_VM_INFO64,
      &mut stats as *mut _ as *mut _,
      &mut count,
    );

    if ret_code != 0 {
      return Err("Failed to get memory stats".into());
    }

    let page_size_kb = libc::sysconf(libc::_SC_PAGESIZE) as u64;
    usage = (stats.active_count as u64
      + stats.inactive_count as u64
      + stats.wire_count as u64
      + stats.speculative_count as u64
      + stats.compressor_page_count as u64
      - stats.purgeable_count as u64
      - stats.external_page_count as u64)
      * page_size_kb;
  }

  Ok((usage, total))
}

pub fn libc_swap() -> WithError<(u64, u64)> {
  let (mut usage, mut total) = (0u64, 0u64);

  unsafe {
    let mut name = [libc::CTL_VM, libc::VM_SWAPUSAGE];
    let mut size = std::mem::size_of::<libc::xsw_usage>();
    let mut xsw: libc::xsw_usage = std::mem::zeroed::<libc::xsw_usage>();

    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut xsw as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get swap usage".into());
    }

    usage = xsw.xsu_used;
    total = xsw.xsu_total;
  }

  Ok((usage, total))
}
