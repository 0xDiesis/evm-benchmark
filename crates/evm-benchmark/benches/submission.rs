use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use evm_benchmark::submission::LatencyTracker;

/// Benchmark latency tracker statistics computation
fn bench_latency_stats_computation(c: &mut Criterion) {
    c.bench_function("latency_stats_100_txs", |b| {
        b.iter(|| {
            let tracker = LatencyTracker::new();

            // Record 100 transactions
            for i in 0..100 {
                let hash = alloy_primitives::B256::repeat_byte(i as u8);
                let sender = alloy_primitives::Address::with_last_byte(i as u8);

                tracker.record_submit(
                    black_box(hash),
                    black_box(i as u64),
                    black_box(sender),
                    black_box(21_000u64),
                    black_box(evm_benchmark::types::TransactionType::SimpleTransfer),
                );
            }

            // Compute statistics
            let _ = tracker.statistics();
        })
    });
}

/// Benchmark latency tracker with various transaction counts
fn bench_latency_stats_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_stats_sizes");
    group.sample_size(10);

    for size in [10, 100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let tracker = LatencyTracker::new();

                // Record transactions
                for i in 0..size {
                    let hash = alloy_primitives::B256::repeat_byte((i % 256) as u8);
                    let sender = alloy_primitives::Address::with_last_byte((i % 256) as u8);

                    tracker.record_submit(
                        black_box(hash),
                        black_box(i as u64),
                        black_box(sender),
                        black_box(21_000u64),
                        black_box(evm_benchmark::types::TransactionType::SimpleTransfer),
                    );
                }

                // Compute statistics
                let _ = tracker.statistics();
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_latency_stats_computation,
    bench_latency_stats_sizes
);
criterion_main!(benches);
