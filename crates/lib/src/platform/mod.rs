#![allow(non_upper_case_globals)]
#![allow(dead_code)]

mod cf;
mod io_report;
mod io_service;
mod memory;
pub(crate) mod smc;

pub type WithError<T> = Result<T, Box<dyn std::error::Error>>;

pub use cf::{cfdict_get_val, cfstr, from_cfstr};
pub use io_report::{IOReport, cfio_collect_residencies};
pub use io_service::{IOServiceIterator, cfio_get_props};
pub use memory::{libc_ram, libc_swap};
pub use smc::SMC;
