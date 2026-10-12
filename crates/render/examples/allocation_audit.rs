//! What a frame costs in memory, counted the way this workspace is allowed to count it.
//!
//! `cargo run --release --example allocation_audit -p render`
//!
//! AGENTS.md §3 bans `malloc`/`free` churn during gameplay frames and doc 09 promises the runtime "reads
//! bytes and issues no allocations" (AS4). Until now neither claim had a number attached, and the honest
//! reason is a workspace rule: counting allocations means a `#[global_allocator]`, implementing
//! `GlobalAlloc` is an `unsafe impl`, and `unsafe_code = "forbid"` in `Cargo.toml` - the same ban that
//! already decided how `assetc` writes FlatBuffer bytes and where WASM exports may come from - refuses it.
//! So this audit measures what is observable without unsafe and says which of its numbers are measured and
//! which are read off the code.
//!
//! What is measured: the two buffers a frame owns. `Vec::clear` plus pushes that fit the existing capacity
//! cannot allocate, which is std's documented behaviour rather than a hope, so **capacity that does not
//! move across repeated renders is the proof** and it is printed here per path. What is read off the code:
//! the bound table, which is two vectors sized by the node count and is built per call unless the caller
//! hoists it - visible in the API, not counted at runtime.
//!
//! Counts, not timings, so this output would repeat across runners if CI ran it; it is not in CI, because
//! it asserts nothing about hashed state and adds a minute to every job for a number that cannot drift.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::scene_bounds;
use render::raster::{
    render, render_bnb, render_bnb_into, render_bnb_with, render_culled_into, render_into, Camera,
    Frame,
};
use render::Tree;

fn sphere(params_first: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(params_first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(4),
    ])
}

fn union(subtree_len: u32, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        subtree_len,
        pack_first_and_flags(0, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(0),
    ])
}

/// Four spheres under two unions under one root: enough structure that the bound table has a node count
/// and a depth to report, and nothing the audit needs a compiler for.
fn scene() -> Tree {
    let params = vec![
        0.4, -2.0, 0.0, 0.0, //
        0.4, -0.5, 0.0, 0.0, //
        0.4, 1.0, 0.0, 0.0, //
        0.4, 2.5, 0.0, 0.0,
    ];
    let tree = Tree {
        nodes: vec![
            union(7, true),
            union(3, false),
            sphere(0, false),
            sphere(4, false),
            union(3, false),
            sphere(8, false),
            sphere(12, false),
        ],
        params,
    };
    // Hand-assembled trees are validated before use: an invalid one evaluates to a picture nobody
    // renders, and the audit would then be measuring a scene that does not exist.
    render::validate(&tree).expect("the audit scene is a valid A-07 tree");
    tree
}

fn camera() -> Camera {
    Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 96,
        height: 64,
        max_t: 20.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn capacities(frame: &Frame) -> (usize, usize) {
    (frame.pixels.capacity(), frame.depth.capacity())
}

fn main() {
    let tree = scene();
    let camera = camera();
    let rays = (camera.width * camera.height) as usize;
    println!("// AS4 allocation audit - the memory a frame costs, measured without unsafe");
    println!(
        "// scene: {} nodes; frame: {}x{} = {} rays",
        tree.nodes.len(),
        camera.width,
        camera.height,
        rays
    );
    println!(
        "// why no allocator counter: #[global_allocator] needs an unsafe impl GlobalAlloc, and \
         unsafe_code = \"forbid\" is a workspace rule"
    );
    println!();

    // 1. The allocating path, for reference: a fresh frame per render.
    let (fresh, _) = render(&tree, &camera);
    println!(
        " render              : frame allocated per call, capacities {:?} for {} pixels",
        capacities(&fresh),
        rays
    );

    // 2. Reusing a frame. Capacity that does not move across renders is the observable form of "this
    //    path allocated nothing in the buffers".
    let mut reused = Frame::new(0, 0);
    let mut seen = Vec::new();
    for _ in 0..5 {
        render_into(&tree, &camera, &mut reused);
        seen.push(capacities(&reused));
    }
    let stable = seen.windows(2).all(|pair| pair[0] == pair[1]);
    println!(
        " render_into         : capacities {:?} over 5 renders, unchanged every time: {}",
        seen[0], stable
    );
    assert!(
        stable,
        "a reused frame grew its buffers after the first render: {:?}",
        seen
    );
    assert_eq!(reused.pixels.len(), rays * 3);
    assert_eq!(reused.depth.len(), rays);

    // 3. The culled path with a hoisted frame but a per-call table.
    let mut culled = Frame::new(camera.width, camera.height);
    let before = capacities(&culled);
    render_culled_into(&tree, &camera, &mut culled);
    render_culled_into(&tree, &camera, &mut culled);
    println!(
        " render_culled_into  : capacities {:?} then {:?}; the bound table (2 Vecs x {} nodes) is built \
         per call",
        before,
        capacities(&culled),
        tree.nodes.len()
    );

    // 4. The form the hot path should use: reused frame, hoisted table, nothing per frame.
    let bounds = scene_bounds(&tree);
    let mut bnb = Frame::new(camera.width, camera.height);
    let before = capacities(&bnb);
    for _ in 0..5 {
        render_bnb_with(&tree, &camera, &mut bnb, &bounds);
    }
    println!(
        " render_bnb_with     : capacities {:?} then {:?} over 5 renders; table hoisted, so nothing \
         allocates per frame",
        before,
        capacities(&bnb)
    );
    assert_eq!(before, capacities(&bnb));

    // 5. Same picture from both entry points - the reuse path must not be a different renderer.
    let (owning, owning_stats) = render_bnb(&tree, &camera);
    let (hoisted_frame, hoisted_stats) = {
        let mut f = Frame::new(camera.width, camera.height);
        let s = render_bnb_into(&tree, &camera, &mut f);
        (f, s)
    };
    assert_eq!(owning.pixels, hoisted_frame.pixels);
    assert_eq!(owning.depth, hoisted_frame.depth);
    assert_eq!(owning_stats, hoisted_stats);
    println!(" render_bnb vs render_bnb_into : pixels, depth and stats identical");
    println!();
    println!("// What this does and does not claim:");
    println!("// - measured: the two buffers a frame owns stop allocating once a frame is reused, at any");
    println!(
        "//   stable resolution. That is the part AGENTS.md §3 is about, and it now has a number."
    );
    println!("// - read off the code: the bound table is two vectors per call. Hoisting it is why");
    println!("//   render_bnb_with exists; a hot path that calls the per-frame form still churns.");
    println!("// - NOT a proof about the whole renderer: no allocator hook is available under the ban, so");
    println!("//   an allocation inside std's iterator adapters or a temporary in a future pass would not");
    println!(
        "//   show up here. If a real frame budget needs that certainty, the decision to reopen is"
    );
    println!(
        "//   a scoped unsafe allow on an allocator-wrapping crate, not a silent #[allow] here."
    );
    println!(
        "// - nothing about gate R1: this is bytes, not nanoseconds, and there is still no GPU."
    );
}
