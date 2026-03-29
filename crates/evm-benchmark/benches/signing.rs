use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use evm_benchmark::signing::BatchSigner;
use std::str::FromStr;

/// Benchmark signing a single transaction synchronously
fn bench_sign_single_tx(c: &mut Criterion) {
    c.bench_function("sign_single_tx", |b| {
        b.iter(|| {
            let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
            let private_signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
            let batch_signer = BatchSigner::new(private_signer, 0, 1);

            let recipient = black_box(Address::with_last_byte(0x42));
            let value = black_box(U256::from(1u32));

            batch_signer
                .sign_batch_parallel(vec![(recipient, value)])
                .expect("failed to sign")
        })
    });
}

/// Benchmark signing 100 transactions sequentially
fn bench_sign_batch_100_sequential(c: &mut Criterion) {
    c.bench_function("sign_batch_100_sequential", |b| {
        b.iter(|| {
            let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
            let private_signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
            let batch_signer = BatchSigner::new(private_signer, 0, 1);

            let recipient = black_box(Address::with_last_byte(0x42));
            let value = black_box(U256::from(1u32));

            // Create 100 transactions and sign them with rayon
            let txs: Vec<(Address, U256)> = (0..100).map(|_| (recipient, value)).collect();

            batch_signer
                .sign_batch_parallel(txs)
                .expect("failed to sign batch")
        })
    });
}

/// Benchmark signing 100 transactions in parallel with rayon (same as sequential since rayon is always parallel)
fn bench_sign_batch_100_rayon(c: &mut Criterion) {
    c.bench_function("sign_batch_100_rayon", |b| {
        b.iter(|| {
            let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
            let private_signer = PrivateKeySigner::from_str(key).expect("failed to parse signer");
            let batch_signer = BatchSigner::new(private_signer, 0, 1);

            let recipient = black_box(Address::with_last_byte(0x42));
            let value = black_box(U256::from(1u32));

            let txs: Vec<(Address, U256)> = (0..100).map(|_| (recipient, value)).collect();

            batch_signer
                .sign_batch_parallel(txs)
                .expect("failed to sign batch")
        })
    });
}

/// Benchmark signing various batch sizes with rayon
fn bench_sign_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("sign_batch_rayon_sizes");
    group.sample_size(10); // Reduce sample size for large batches

    for size in [10, 50, 100, 500, 1000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
                let private_signer =
                    PrivateKeySigner::from_str(key).expect("failed to parse signer");
                let batch_signer = BatchSigner::new(private_signer, 0, 1);

                let recipient = black_box(Address::with_last_byte(0x42));
                let value = black_box(U256::from(1u32));

                let txs: Vec<(Address, U256)> = (0..size).map(|_| (recipient, value)).collect();

                batch_signer
                    .sign_batch_parallel(txs)
                    .expect("failed to sign batch")
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sign_single_tx,
    bench_sign_batch_100_sequential,
    bench_sign_batch_100_rayon,
    bench_sign_batch_sizes
);
criterion_main!(benches);
