//! Emit the probe asset through the writer so `flatc` can be pointed at the result.
//! The acceptance bar is an outside parser reading these bytes, not our own reader agreeing.
use assetc::lower;
use assetc::writer::to_bytes;
use assetc::Shape;

fn main() {
    let authored = Shape::Sphere {
        r: 1.0,
        centre: [0.0, 0.0, 0.0],
    };
    let asset = lower(&authored).expect("a sphere lowers");
    let bytes = to_bytes(&asset, "probe: a unit sphere at the origin");
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "probe_written.bin".into());
    std::fs::write(&path, &bytes).expect("writable");
    println!("wrote {} bytes to {path}", bytes.len());
}
