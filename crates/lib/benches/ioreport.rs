#[divan::bench(sample_count = 10, sample_size = 1)]
fn subscription() {
  divan::black_box_drop(
    macmon::bench::IOReport::new(Some(macmon::bench::ioreport_channels_filter)).unwrap(),
  );
}

#[divan::bench(sample_count = 100, sample_size = 1)]
fn get_sample(bencher: divan::Bencher) {
  let mut ior =
    macmon::bench::IOReport::new(Some(macmon::bench::ioreport_channels_filter)).unwrap();
  bencher.bench_local(|| divan::black_box_drop(ior.next_sample()));
}
