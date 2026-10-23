//! The lowering stage, checked against the evaluator it feeds.
//!
//! Deliberately **not** a byte-for-byte comparison with `crates/render`'s hand-built tree: a node's
//! pool offset depends on emission order, so demanding identical words would pin an accident of the
//! implementation instead of the contract. What is asserted is what must hold - the lowered tree
//! answers distance questions identically, its spans are pre-order-consistent (`render::validate`
//! recomputes them independently), the root is flagged once, and the refusals refuse.

use assetc::{compile_entity, lower, EntityDef, LowerError, Shape};

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::sdf_eval;

fn scene() -> Shape {
    Shape::Union {
        a: Box::new(Shape::RoundedBox {
            half: [0.35, 0.35, 0.9],
            r: 0.12,
        }),
        b: Box::new(Shape::SmoothUnion {
            a: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [-0.9, 0.0, 0.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [0.9, 0.0, 0.0],
            }),
            k: 0.5,
        }),
    }
}

/// The same scene as `crates/render/examples/scene_steps.rs` builds it by hand.
fn hand_built() -> render::Tree {
    let mut pool: Vec<f32> = Vec::new();
    let mut push = |kind: u16, arity: u16, subtree: u32, vals: &[f32], flags: u16| -> NodeRecord {
        let first = pool.len() as u16;
        pool.extend_from_slice(vals);
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, flags),
            pack_params_len(vals.len() as u16),
        ])
    };
    let left = push(NODE_SPHERE, 0, 1, &[0.7, -0.9, 0.0, 0.0], 0);
    let right = push(NODE_SPHERE, 0, 1, &[0.7, 0.9, 0.0, 0.0], 0);
    let blended = push(NODE_SMOOTH_UNION, 2, 3, &[0.5], 0);
    let block = push(NODE_ROUNDED_BOX, 0, 1, &[0.35, 0.35, 0.9, 0.12], 0);
    let whole = push(NODE_UNION, 2, 5, &[], FLAG_IS_ROOT);
    render::Tree {
        nodes: vec![whole, block, blended, left, right],
        params: pool,
    }
}

#[test]
fn a_lowered_scene_answers_distance_questions_like_the_hand_built_one() {
    let asset = lower(&scene()).expect("the scene lowers");
    let hand = hand_built();
    render::validate(&hand).expect("the hand-built tree is the same layout");
    render::validate(&asset.tree).expect("and so is the lowered one");
    assert_eq!(asset.tree.nodes.len(), 5);
    assert_eq!(asset.depth, 3, "union -> blend -> sphere");

    for probe in [
        [0.0f32, 0.0, 0.0],
        [0.0, 0.0, 0.95],
        [-0.9, 0.0, 0.0],
        [3.0, 0.0, 0.0],
        [0.0, 0.0, -3.0],
    ] {
        let lowered = sdf_eval(&asset.tree, render::ROOT, probe);
        let built = sdf_eval(&hand, render::ROOT, probe);
        assert!(
            (lowered - built).abs() < 1e-6,
            "at {probe:?}: lowered {lowered} vs hand-built {built}"
        );
    }
}

#[test]
fn the_root_is_flagged_once_and_nothing_claims_a_material() {
    let asset = lower(&scene()).expect("lowers");
    let roots = asset
        .tree
        .nodes
        .iter()
        .filter(|record| record.flags() & FLAG_IS_ROOT != 0)
        .count();
    assert_eq!(roots, 1, "exactly one root, on the outermost node");
    assert_eq!(
        asset.tree.nodes[0].subtree_len() as usize,
        asset.tree.nodes.len(),
        "a root's span covers the buffer, which is what makes index zero the entry point"
    );
    assert!(!asset.has_material(), "no Shape variant carries one yet");
}

#[test]
fn refusals_refuse_before_any_byte_exists() {
    let axis = lower(&Shape::Mirror {
        axis: 7,
        child: Box::new(Shape::Sphere {
            r: 1.0,
            centre: [0.0; 3],
        }),
    })
    .unwrap_err(); // an out-of-range axis is refused, not clamped into a different shape
    assert_eq!(axis.error, LowerError::BadAxis);
    assert_eq!(
        axis.location(),
        "Mirror",
        "and the author is told which shape"
    );

    let nan = lower(&Shape::Sphere {
        r: f32::NAN,
        centre: [0.0; 3],
    })
    .unwrap_err(); // NaN travels a distance field into every pixel and raises nothing
    assert_eq!(nan.error, LowerError::NonFiniteParameter);
    assert_eq!(nan.location(), "Sphere");

    let mut deep = Shape::Sphere {
        r: 0.5,
        centre: [0.0; 3],
    };
    for _ in 0..20 {
        deep = Shape::Repeat {
            period: 2.0,
            child: Box::new(deep),
        };
    }
    let too_deep = lower(&deep).unwrap_err(); // a 20-deep tiling is refused
    assert!(matches!(too_deep.error, LowerError::TooDeep { .. }));
    assert_eq!(too_deep.location(), "Repeat");
}

#[test]
fn subtracts_argument_order_belongs_to_the_keeper() {
    // The evaluator's `max(dKept, -dCutter)` and the authoring field named `kept` have to agree, or a
    // compiled asset cuts the wrong half of a shape - the exact inversion the evaluator's first draft
    // carried.
    let asset = lower(&Shape::Subtract {
        kept: Box::new(Shape::RoundedBox {
            half: [0.5, 0.5, 0.5],
            r: 0.1,
        }),
        cutter: Box::new(Shape::Plane {
            normal: [0.0, 0.0, 1.0],
            offset: 0.2,
        }),
    })
    .expect("lowers");
    assert!(
        sdf_eval(&asset.tree, render::ROOT, [0.0, 0.0, 0.3]) < 0.0,
        "inside the box and above the cut plane, so kept"
    );
    assert!(
        sdf_eval(&asset.tree, render::ROOT, [0.0, 0.0, -0.3]) > 0.0,
        "below it, removed"
    );
}
#[test]
fn a_refusal_names_the_authored_route_not_the_record_index() {
    // AS3's whole point is that the message is actionable in the file. Node 4 is a fact about the
    // compiled output; `Union.b.Mirror.a.Sphere` is a fact about what the author wrote, and only one of
    // those two can be fixed without opening the file and reading it back.
    let nested = Shape::Union {
        a: Box::new(Shape::Sphere {
            r: 1.0,
            centre: [0.0, 0.0, 0.0],
        }),
        b: Box::new(Shape::Mirror {
            axis: 2,
            child: Box::new(Shape::Subtract {
                kept: Box::new(Shape::Sphere {
                    r: f32::NAN,
                    centre: [0.0, 0.0, 0.0],
                }),
                cutter: Box::new(Shape::Sphere {
                    r: 0.5,
                    centre: [1.0, 0.0, 0.0],
                }),
            }),
        }),
    };
    let refusal = lower(&nested).unwrap_err(); // a NaN two levels down is refused
    assert_eq!(refusal.error, LowerError::NonFiniteParameter);
    assert_eq!(
        refusal.location(),
        "Union.b.Mirror.child.Subtract.kept.Sphere"
    );
    let message = refusal.to_string();
    assert!(
        message.contains("Union.b.Mirror.child.Subtract.kept") && message.contains("non-finite"),
        "the sentence has to carry both the place and the reason: {message}"
    );

    // The watch loop surfaces the same sentence, because that is what an agent editing the file reads.
    let dir = std::env::temp_dir().join(format!("assetc-span-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir for the bad authoring file");
    let input = dir.join("bad.ron");
    let output = dir.join("bad.bin");
    let _ = std::fs::remove_file(&output);
    let text = ron::to_string(&nested).expect("the shape is writable as RON");
    std::fs::write(&input, text).expect("the file is writable");
    let compiled =
        assetc::watch::Watch::default().observe(input.to_str().unwrap(), output.to_str().unwrap());
    match compiled {
        assetc::watch::Outcome::Failed(reason) => {
            assert!(
                reason.contains("schema violation") && reason.contains("Subtract.kept.Sphere"),
                "the reload loop must report the authored route: {reason}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!output.exists(), "a refused shape writes nothing");
}

#[test]
fn a_placement_refusal_names_the_clause_that_moved() {
    // The editor's preview and its MCP pipe both quote these sentences back to an author. Calling a
    // finite, merely-unnormalised quaternion "non-finite" sends them hunting for a NaN that is not in
    // the file, and the two mistakes have opposite fixes: one is deleted, the other normalised.
    let sphere = || EntityDef {
        shape: Shape::Sphere {
            r: 1.0,
            centre: [0.0, 0.0, 0.0],
        },
        position: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: 1.0,
        material_id: 0,
        flags: 0x03,
        quantity: 0,
    };

    let mut wild = sphere();
    wild.rotation = [0.0, 2.0, 0.0, 1.0];
    let refusal =
        compile_entity(&wild).expect_err("a quaternion of squared length 5 is not a rotation");
    assert_eq!(refusal.error, LowerError::NonUnitQuaternion);
    let words = refusal.to_string();
    assert!(
        words.contains("unit quaternion") && !words.contains("non-finite"),
        "the sentence has to name the clause that moved: {words}"
    );

    let mut flat = sphere();
    flat.scale = 0.0;
    let refusal = compile_entity(&flat).expect_err("nothing has an inside at scale zero");
    assert_eq!(refusal.error, LowerError::NonPositiveScale);
    assert!(
        !refusal.to_string().contains("non-finite"),
        "a zero scale is a finite number: {refusal}"
    );

    // The case the shared name was right for still says what it means.
    let mut nan = sphere();
    nan.rotation = [0.0, f32::NAN, 0.0, 1.0];
    let refusal = compile_entity(&nan).expect_err("NaN in a lane is refused");
    assert_eq!(refusal.error, LowerError::NonFiniteParameter);
}
