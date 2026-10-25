//! Saturated churn benchmark for the semaphore-backed pool capacity gate
//! (design-semaphore-pool-refactor.md S4).
//!
//! Measures steady-state acquire/release throughput of `PoolCapacity` under
//! oversubscription (16 concurrent tasks on a 4-permit gate), the pattern
//! that used to surface permit-collapse wake-up loss in the hand-rolled
//! Notify scheme. A permit-based gate cannot lose wake-ups by construction,
//! so the numbers here are a regression guard: a meaningful throughput drop
//! across versions flags a fairness/latency change in the gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use oxidns::infra::clock::AppClock;
use oxidns::infra::network::upstream::PoolCapacity;

fn churn(c: &mut Criterion) {
    AppClock::start();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("bench runtime should build");
    // Cheap Arc-based handle: cloned per benchmark invocation so the FnMut
    // closure never has to move or borrow the Runtime itself, and the
    // `to_async` executor relies only on the owned-Handle impl.
    let handle = runtime.handle().clone();

    let mut group = c.benchmark_group("pool_capacity_churn");
    group.throughput(criterion::Throughput::Elements(
        16 * 512, // tasks * iterations, counted as capacity transactions
    ));

    for permits in [2usize, 4, 8] {
        group.bench_with_input(
            criterion::BenchmarkId::from_parameter(permits),
            &permits,
            |b, &permits| {
                b.to_async(handle.clone()).iter(|| async move {
                    let capacity = Arc::new(PoolCapacity::new(permits));
                    let done = Arc::new(AtomicU64::new(0));
                    const ITERATIONS: u64 = 512;

                    let mut handles = Vec::new();
                    for _ in 0..16 {
                        let capacity = capacity.clone();
                        let done = done.clone();
                        handles.push(tokio::spawn(async move {
                            while done.load(Ordering::Relaxed) < ITERATIONS {
                                let permit = capacity.acquire_owned().await;
                                done.fetch_add(1, Ordering::Relaxed);
                                drop(permit);
                            }
                        }));
                    }
                    for handle in handles {
                        handle.await.expect("churn task should join");
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, churn);
criterion_main!(benches);
