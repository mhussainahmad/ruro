//! Cost of moving one message across processes through shared memory
//! (publish, notify, receive), for small and image-sized payloads.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ruro_transport::Shm;

fn shm_hop(c: &mut Criterion) {
    let shm = Shm::new("ruro-bench").unwrap();
    let mut group = c.benchmark_group("shm_hop");
    for size in [8usize, 4 * 1024, 640 * 480 * 3, 1920 * 1080 * 3] {
        let topic = format!("ruro-bench/{}/{size}", std::process::id());
        let sub = shm.subscriber(&topic, 1).unwrap();
        let publ = shm.publisher(&topic).unwrap();
        let payload = vec![7u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                publ.send(1, 0, size, &mut |buf| buf.copy_from_slice(&payload))
                    .unwrap();
                sub.wait(Duration::from_millis(100)).unwrap();
                let mut got = 0;
                sub.drain(|_, _, body| got = body.len()).unwrap();
                assert_eq!(got, size);
            })
        });
    }
    group.finish();
}

criterion_group!(benches, shm_hop);
criterion_main!(benches);
