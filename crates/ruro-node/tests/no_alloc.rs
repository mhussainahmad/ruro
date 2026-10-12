//! Proves that the pooled paths do not allocate in steady state: publishing
//! with `Output::loan`, and receiving from another process via `push_encoded`.
//!
//! A counting global allocator counts allocations per thread while
//! measuring, so the harness and tests running in parallel on other threads
//! do not interfere.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ruro_msg::{Encoding, Image, Message, Scalar};
use ruro_node::{Builder, BuiltNode, Ctx, DynNode, ErasedInput, Input, Output};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    if COUNTING.with(Cell::get) {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f` on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

struct Noop;
impl DynNode for Noop {
    fn start(&mut self, _: &mut Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn step(&mut self, _: &mut Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn stop(&mut self, _: &mut Ctx) {}
    fn set_param(&mut self, _: &str, _: &serde_json::Value) -> ruro_node::Result {
        Ok(())
    }
}

fn wired<T: Message>(depth: usize) -> (Output<T>, Input<T>, BuiltNode) {
    let params = Default::default();
    let mut b = Builder::new("t", &params, [("in".to_owned(), depth)].into());
    let out = b.output::<T>("out").unwrap();
    let inp = b.input::<T>("in").unwrap();
    let built = b.finish(Box::new(Noop));
    built.outputs[0]
        .1
        .connect_local(built.inputs[0].1.clone())
        .unwrap();
    (out, inp, built)
}

fn publish_frame(out: &Output<Image>, i: u8) {
    let mut frame = out.loan();
    frame.reshape(320, 240, Encoding::Rgb8);
    frame.data.fill(i);
    frame.send().unwrap();
}

#[test]
fn loaned_publishing_does_not_allocate() {
    let (out, inp, _built) = wired::<Image>(2);
    // Warm up: fill the pool with frames and the queue to its depth.
    for i in 0..8 {
        publish_frame(&out, i);
        if i % 2 == 0 {
            drop(inp.recv());
        }
    }
    let n = allocations(|| {
        for i in 0..1000u32 {
            publish_frame(&out, i as u8);
            // A consumer that holds on to the latest frame for a while.
            let frame = inp.latest().unwrap();
            assert_eq!(frame.data[0], i as u8);
        }
    });
    assert_eq!(n, 0, "steady-state loan/send allocated {n} times");

    // Sanity check that the counter works: plain `send` allocates its sample.
    let (scalar_out, scalar_in, _b) = wired::<Scalar>(1);
    let n = allocations(|| {
        scalar_out.send(Scalar(1.0)).unwrap();
        drop(scalar_in.recv());
    });
    assert!(n > 0);
}

#[test]
fn decoding_from_other_processes_does_not_allocate() {
    let (_out, inp, built) = wired::<Image>(1);
    let erased: std::sync::Arc<dyn ErasedInput> = built.inputs[0].1.clone();
    let frames: Vec<Vec<u8>> = (0..4u8)
        .map(|i| {
            let mut img = Image::new(320, 240, Encoding::Rgb8);
            img.data.fill(i);
            img.to_bytes()
        })
        .collect();
    for (i, f) in frames.iter().enumerate() {
        erased.push_encoded(i as u64, 0, f).unwrap();
        drop(inp.recv());
    }
    let n = allocations(|| {
        for i in 0..1000usize {
            erased.push_encoded(i as u64, 0, &frames[i % 4]).unwrap();
            let frame = inp.recv().unwrap();
            assert_eq!(frame.data[0], (i % 4) as u8);
        }
    });
    assert_eq!(n, 0, "steady-state receive allocated {n} times");
}
