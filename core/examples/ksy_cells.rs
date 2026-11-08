//! Time a protocol definition against native DLT, per message and on one
//! thread: each field, the whole line, splitting; and the allocations of
//! each. For work on the definition reader (doc/protocol-definitions.md).
//!
//!     cargo run --release -p nerka-core --example ksy_cells -- trace.dlt examples/protocols/dlt.ksy
use nerka_core::input::proto::ksy::KsyProtocol;
use nerka_core::input::proto::{Origin, Protocol};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let p = KsyProtocol::load(&a[2]).unwrap();
    let mut msgs = Vec::new();
    let mut pos = 0;
    while pos + 20 <= data.len() && msgs.len() < 20000 {
        let len = u16::from_be_bytes([data[pos + 18], data[pos + 19]]) as usize;
        msgs.push(data[pos + 16..pos + 16 + len].to_vec());
        pos += 16 + len;
    }
    let at = Origin { secs: 0, usecs: 0, channel: &[0; 4] };
    for (id, f) in p.schema().iter().enumerate() {
        let before = ALLOCS.load(Ordering::Relaxed);
        let t = Instant::now();
        for m in &msgs {
            std::hint::black_box(p.field(m, &at, id));
        }
        let allocs = (ALLOCS.load(Ordering::Relaxed) - before) as f64 / msgs.len() as f64;
        println!("{:8} {:6.0} ns {allocs:5.1} allocs", f.name, t.elapsed().as_nanos() as f64 / msgs.len() as f64);
    }
    let t = Instant::now();
    let mut out = Vec::new();
    for m in &msgs {
        out.clear();
        p.render(m, &at, &mut out);
    }
    println!("render   {:6.0} ns", t.elapsed().as_nanos() as f64 / msgs.len() as f64);
    let dgrams: Vec<Vec<u8>> = msgs.chunks(20).map(|c| c.concat()).collect();
    let native = nerka_core::input::dlt::Dlt::default();
    for (name, pr) in [("native", &native as &dyn Protocol), ("ksy", &p as &dyn Protocol)] {
        let mut found = Vec::new();
        let t = Instant::now();
        for d in &dgrams {
            found.clear();
            pr.split(d, &mut found);
        }
        println!("split {name:6} {:6.0} ns", t.elapsed().as_nanos() as f64 / msgs.len() as f64);
    }
}
