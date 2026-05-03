#[divan::bench(sample_count = 3, sample_size = 1)]
fn full_init() {
  divan::black_box_drop(macmon::bench::init_smc().unwrap());
}

#[divan::bench(sample_count = 3, sample_size = 1)]
fn read_all_keys(bencher: divan::Bencher) {
  let mut smc = macmon::sources::SMC::new().unwrap();
  bencher.bench_local(|| divan::black_box_drop(smc.read_all_keys().unwrap()));
}
