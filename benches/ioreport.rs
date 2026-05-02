#[divan::bench(sample_count = 10, sample_size = 1)]
fn subscription() {
  divan::black_box_drop(macmon::bench::init_ioreport().unwrap());
}

#[divan::bench(sample_count = 100, sample_size = 1)]
fn get_samples_0_1(bencher: divan::Bencher) {
  let mut ior = macmon::bench::init_ioreport().unwrap();
  bencher.bench_local(|| divan::black_box_drop(ior.get_samples(0, 1)));
}

#[divan::bench(sample_count = 100, sample_size = 1)]
fn get_samples_0_4(bencher: divan::Bencher) {
  let mut ior = macmon::bench::init_ioreport().unwrap();
  bencher.bench_local(|| divan::black_box_drop(ior.get_samples(0, 4)));
}
