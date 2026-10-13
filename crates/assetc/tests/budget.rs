//! AS5's mechanism: what a compiled shape costs, and what happens when it costs more than the caller's
//! ceiling. The **number** is deliberately not chosen here - doc 09 says the per-tree budget is still
//! undefined - so these tests pin the shape of the gate (measured cost, a bound that bounds, refusal
//! without writing) and leave the threshold to whoever owns the frame-time claim.

use assetc::budget::{cost_of, violations, Budget, Cost, Violation};
use assetc::{lower, Shape};

fn sphere() -> Shape {
    Shape::Sphere {
        r: 1.0,
        centre: [0.0, 0.0, 0.0],
    }
}

/// A chain of `count` spheres joined by plain unions: the widest, shallowest way to make a big tree, so
/// the node clause bites long before the grammar's depth cap could.
fn chain(count: usize) -> Shape {
    let mut shape = sphere();
    for index in 1..count {
        shape = Shape::Union {
            a: Box::new(shape),
            b: Box::new(Shape::Sphere {
                r: 0.2,
                centre: [index as f32, 0.0, 0.0],
            }),
        };
    }
    shape
}

fn cost_of_shape(shape: &Shape) -> Cost {
    cost_of(&lower(shape).expect("the test shape must lower"))
}

#[test]
fn a_simple_shape_reports_its_cost_without_tripping_a_generous_ceiling() {
    let asset = lower(&sphere()).expect("a sphere lowers");
    let cost = cost_of(&asset);
    assert_eq!(cost.nodes, 1, "one record");
    assert_eq!(
        cost.depth, 1,
        "a lone leaf is one level deep - the compiler counts the level, not the edges below it"
    );
    assert_eq!(cost.params, 4, "radius and centre");
    assert_eq!(cost.unbounded, 0, "a sphere bounds itself");
    let budget = Budget {
        max_nodes: 32,
        max_depth: 8,
        max_evaluations_per_ray: 4_096,
    };
    assert!(
        violations(&cost, &budget, 96).is_empty(),
        "a single record must not be refused by any sane ceiling"
    );
}

#[test]
fn an_over_budget_tree_names_every_clause_it_trips() {
    // All three at once: an author told about one clause per run learns nothing about how far off the
    // shape is.
    let asset = lower(&chain(40)).expect("a chain lowers");
    let cost = cost_of(&asset);
    assert_eq!(cost.nodes, 79, "40 leaves and 39 branches");
    let budget = Budget {
        max_nodes: 8,
        max_depth: 4,
        max_evaluations_per_ray: 100,
    };
    let failures = violations(&cost, &budget, 96);
    assert_eq!(
        failures.len(),
        3,
        "every clause should report: {failures:?}"
    );
    assert!(failures.contains(&Violation::TooManyNodes {
        found: 79,
        limit: 8
    }));
    assert!(failures.contains(&Violation::TooDeep {
        found: cost.depth,
        limit: 4
    }));
    assert!(failures.contains(&Violation::TooExpensive {
        found: cost.worst_evaluations_per_ray(96),
        limit: 100
    }));
    assert_eq!(
        cost.worst_evaluations_per_ray(96),
        79 * 96,
        "the bound is doc 04's cost law: steps x nodes"
    );
}

#[test]
fn the_bound_bounds_what_a_real_frame_actually_measures() {
    // A static number is only worth gating on if it bounds the real thing, so compare it against the
    // renderer instead of trusting the algebra. `raster` charges a hit its steps plus one and a missed ray
    // the whole step budget, both of which sit under `nodes x steps` per ray by construction.
    let shape = Shape::Union {
        a: Box::new(Shape::RoundedBox {
            half: [0.4, 0.4, 0.4],
            r: 0.1,
        }),
        b: Box::new(chain(12)),
    };
    let asset = lower(&shape).expect("the mixed shape lowers");
    let cost = cost_of(&asset);
    let tree = render::Tree {
        nodes: asset.tree.nodes,
        params: asset.tree.params,
    };
    let camera = render::raster::Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 32,
        height: 24,
        max_t: 20.0,
        surface: 1e-3,
        max_steps: 96,
    };
    let (_, stats) = render::raster::render(&tree, &camera);
    let per_ray_bound = cost.worst_evaluations_per_ray(camera.max_steps);
    let measured_average = stats.evaluations / u64::from(stats.rays).max(1);
    assert!(
        measured_average <= per_ray_bound,
        "a frame cost more per ray than the bound says is possible: {measured_average} > {per_ray_bound}"
    );
    assert!(
        stats.evaluations <= u64::from(stats.rays) * per_ray_bound,
        "the frame total escaped the bound"
    );
    let headroom = per_ray_bound as f64 / measured_average.max(1) as f64;
    assert!(
        headroom > 1.0,
        "the bound is only useful if it is loose in the right direction: {headroom}"
    );
    println!(
        "BUDGET nodes {} depth {} bound {per_ray_bound}/ray measured {measured_average}/ray headroom {headroom:.1}x",
        cost.nodes, cost.depth
    );
}

#[test]
fn unbounded_kinds_are_counted_because_they_defeat_the_early_out() {
    // `plane` and `repeat` have no sphere containing their surface, so a scene holding one cannot skip a
    // ray at all. The node count hides that, which is why the cost reports it separately.
    let tiled = Shape::Repeat {
        period: 3.0,
        child: Box::new(Shape::Sphere {
            r: 0.5,
            centre: [0.0, 0.0, 0.0],
        }),
    };
    let cost = cost_of_shape(&tiled);
    assert_eq!(
        cost.unbounded, 1,
        "the tiling itself is the unbounded record"
    );
    let flat = Shape::Plane {
        normal: [0.0, 1.0, 0.0],
        offset: 0.0,
    };
    assert_eq!(cost_of_shape(&flat).unbounded, 1);
}

#[test]
fn the_command_refuses_to_write_an_over_budget_asset_and_says_why() {
    let dir = std::env::temp_dir().join(format!("assetc-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir for the test assets");
    let input = dir.join("pair.ron");
    let output = dir.join("pair.bin");
    let _ = std::fs::remove_file(&output);
    // Serialised rather than typed: the authoring format is `Shape`'s own, and hand-writing it in a test
    // is how the test ends up asserting a syntax the compiler never accepted.
    let authored = Shape::Union {
        a: Box::new(Shape::Sphere {
            r: 1.0,
            centre: [0.0, 0.0, 0.0],
        }),
        b: Box::new(Shape::Sphere {
            r: 0.5,
            centre: [2.0, 0.0, 0.0],
        }),
    };
    let text = ron::to_string(&authored).expect("a shape is writable as RON");
    std::fs::write(&input, text).expect("the authoring file is writable");

    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_assetc"))
            .args(args)
            .output()
            .expect("the compiler binary runs")
    };

    let ok = run(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--budget",
        "nodes=8,depth=4,evaluations=1000",
    ]);
    assert!(
        ok.status.success(),
        "a two-record tree must fit the generous ceiling: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(output.exists(), "a fitting asset is written");
    std::fs::remove_file(&output).expect("the fitting asset is removable");

    let tight = run(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--budget",
        "nodes=1,depth=0,evaluations=1",
    ]);
    assert!(!tight.status.success(), "a tight ceiling must fail the run");
    assert!(
        !output.exists(),
        "a refused asset must not replace the file on disk"
    );
    let stderr = String::from_utf8_lossy(&tight.stderr).to_string();
    assert!(stderr.contains("over budget"), "{stderr}");
    assert!(stderr.contains("nodes"), "{stderr}");
    assert!(stderr.contains("depth"), "{stderr}");
    assert!(stderr.contains("worst-case"), "{stderr}");
}
