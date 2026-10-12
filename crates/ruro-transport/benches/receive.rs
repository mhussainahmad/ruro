//! Cost of the full cross-process receive path for a typed input: publish an
//! encoded message into shared memory, take it out on the subscriber side,
//! decode it into the input queue and hand it to the node.
//!
//! Both ends live in this process, but the bytes travel through the same
//! iceoryx2 shared-memory service a separate host process would use.

use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ruro_msg::{Encoding, Image, Message, Scalar};
use ruro_node::{Builder, ErasedInput, Input};
use ruro_transport::Shm;

fn typed_input<T: Message>() -> (Input<T>, Arc<dyn ErasedInput>) {
    let params = Default::default();
    let mut b = Builder::new("bench", &params, Default::default());
    let input = b.input::<T>("in").unwrap();
    let erased = b.finish(Box::new(Noop)).inputs.remove(0).1;
    (input, erased)
}

fn shm_receive(c: &mut Criterion) {
    let shm = Shm::new("ruro-bench-rx").unwrap();
    let mut group = c.benchmark_group("shm_receive");

    let topic = format!("ruro-bench-rx/{}/scalar", std::process::id());
    let sub = shm.subscriber(&topic, 1).unwrap();
    let publ = shm.publisher(&topic).unwrap();
    let (input, erased) = typed_input::<Scalar>();
    let msg = Scalar(1.5);
    group.bench_function("scalar", |b| {
        b.iter(|| {
            publ.send(1, 0, msg.encoded_len(), &mut |buf| msg.encode_into(buf))
                .unwrap();
            sub.wait(Duration::from_millis(100)).unwrap();
            sub.drain(|seq, stamp, body| erased.push_encoded(seq, stamp, body).unwrap())
                .unwrap();
            input.recv().unwrap()
        })
    });

    for (name, w, h) in [("vga_image", 640, 480), ("1080p_image", 1920, 1080)] {
        let topic = format!("ruro-bench-rx/{}/{name}", std::process::id());
        let sub = shm.subscriber(&topic, 1).unwrap();
        let publ = shm.publisher(&topic).unwrap();
        let (input, erased) = typed_input::<Image>();
        let img = Image::new(w, h, Encoding::Rgb8);
        let len = img.encoded_len();
        group.throughput(Throughput::Bytes(len as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &len, |b, &len| {
            b.iter(|| {
                publ.send(1, 0, len, &mut |buf| img.encode_into(buf))
                    .unwrap();
                sub.wait(Duration::from_millis(100)).unwrap();
                sub.drain(|seq, stamp, body| erased.push_encoded(seq, stamp, body).unwrap())
                    .unwrap();
                // Dropping the sample here mirrors a node that is done with the frame.
                input.recv().unwrap().data.len()
            })
        });
    }
    group.finish();
}

struct Noop;
impl ruro_node::DynNode for Noop {
    fn start(&mut self, _: &mut ruro_node::Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn step(&mut self, _: &mut ruro_node::Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn stop(&mut self, _: &mut ruro_node::Ctx) {}
    fn set_param(&mut self, _: &str, _: &serde_json::Value) -> ruro_node::Result {
        Ok(())
    }
}

criterion_group!(benches, shm_receive);
criterion_main!(benches);
