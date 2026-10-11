//! Cost of an in-process hop: send on an output, receive on a connected input.

use criterion::{Criterion, criterion_group, criterion_main};
use ruro_msg::{Encoding, Image, Scalar};
use ruro_node::Builder;

fn local_hop(c: &mut Criterion) {
    let params = Default::default();
    let mut b = Builder::new("bench", &params, Default::default());
    let out = b.output::<Scalar>("out").unwrap();
    let inp = b.input::<Scalar>("in").unwrap();
    let img_out = b.output::<Image>("img_out").unwrap();
    let img_in = b.input::<Image>("img_in").unwrap();
    let built = b.finish(Box::new(Noop));
    let port = |name: &str| {
        built
            .outputs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .clone()
    };
    let input = |name: &str| {
        built
            .inputs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .clone()
    };
    port("out").connect_local(input("in")).unwrap();
    port("img_out").connect_local(input("img_in")).unwrap();

    c.bench_function("local_hop/scalar", |b| {
        b.iter(|| {
            out.send(Scalar(1.0)).unwrap();
            inp.recv().unwrap()
        })
    });
    let img = Image::new(1920, 1080, Encoding::Rgb8);
    c.bench_function("local_hop/1080p_image_shared", |b| {
        b.iter_batched(
            || img.clone(),
            |frame| {
                img_out.send(frame).unwrap();
                img_in.recv().unwrap()
            },
            criterion::BatchSize::LargeInput,
        )
    });
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

criterion_group!(benches, local_hop);
criterion_main!(benches);
