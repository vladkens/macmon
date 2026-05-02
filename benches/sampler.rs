#[divan::bench(sample_count = 10, sample_size = 1)]
fn get_metrics_0(bencher: divan::Bencher) {
  let mut sampler = macmon::Sampler::new().unwrap();
  bencher.bench_local(|| divan::black_box_drop(sampler.get_metrics(0).unwrap()));
}
