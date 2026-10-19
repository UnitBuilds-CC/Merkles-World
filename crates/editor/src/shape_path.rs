//! Addressing a node inside the authored tree.
//!
//! The inspector used to edit the root variant and nothing else, which for a creature whose root is a
//! `SmoothUnion` means it edited the blend factor of the whole animal and could not reach the snout.
//! A part is reached by a path of [`Step`]s from the root, and each step carries the field name the
//! authored file gives it — `a`/`b`, `kept`/`cutter`, `child` — which are the same names `assetc`'s
//! refusals interleave with the kind (`Subtract.kept`, from `crates/assetc/src/lib.rs`'s `edge` and
//! `label`). The panel's routes are the compact form — `root.b.kept` — and each row prints its node's
//! kind beside the route, so an author holding a compiler error can find the row it refers to.

use assetc::Shape;

/// Which child of a combinator to take. `First`/`Second` are the two operands in the order the grammar
/// means them — for a `Subtract` that is `kept` then `cutter`, and swapping them is a different animal,
/// not a mirror of the same one. `Sole` is a one-child wrapper's child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    First,
    Second,
    Sole,
}

/// The children of a node, with the name the authored file gives each. Leaves return an empty slice, so
/// walking off one is a `None` from [`resolve`] rather than a silent stop at the leaf.
pub fn children(shape: &Shape) -> &'static [(&'static str, Step)] {
    const TWO_AB: &[(&str, Step)] = &[("a", Step::First), ("b", Step::Second)];
    const KEPT_CUTTER: &[(&str, Step)] = &[("kept", Step::First), ("cutter", Step::Second)];
    const ONE_CHILD: &[(&str, Step)] = &[("child", Step::Sole)];
    const NONE: &[(&str, Step)] = &[];

    match shape {
        Shape::Union { .. } | Shape::SmoothUnion { .. } | Shape::Intersect { .. } => TWO_AB,
        Shape::Subtract { .. } => KEPT_CUTTER,
        Shape::Repeat { .. } | Shape::Mirror { .. } => ONE_CHILD,
        Shape::Sphere { .. } | Shape::RoundedBox { .. } | Shape::Plane { .. } => NONE,
    }
}

/// The variant's name as the authored file spells it.
pub fn kind(shape: &Shape) -> &'static str {
    match shape {
        Shape::Sphere { .. } => "Sphere",
        Shape::RoundedBox { .. } => "RoundedBox",
        Shape::Plane { .. } => "Plane",
        Shape::Union { .. } => "Union",
        Shape::SmoothUnion { .. } => "SmoothUnion",
        Shape::Subtract { .. } => "Subtract",
        Shape::Intersect { .. } => "Intersect",
        Shape::Repeat { .. } => "Repeat",
        Shape::Mirror { .. } => "Mirror",
    }
}

/// The node's own parameters, as one line. Only what this node carries: a combinator's children are
/// listed separately, because the outline is what tells an author which of the two snouts they have
/// selected.
pub fn summary(shape: &Shape) -> String {
    let three = |v: &[f32; 3]| format!("[{:.2}, {:.2}, {:.2}]", v[0], v[1], v[2]);
    match shape {
        Shape::Sphere { r, centre } => format!("r {r:.2} at {}", three(centre)),
        Shape::RoundedBox { half, r } => format!("half {} corner r {r:.2}", three(half)),
        Shape::Plane { normal, offset } => format!("normal {} offset {offset:.2}", three(normal)),
        Shape::SmoothUnion { k, .. } => format!("blend k {k:.2}"),
        Shape::Repeat { period, .. } => format!("period {period:.2}"),
        Shape::Mirror { axis, .. } => format!("axis {axis}"),
        Shape::Union { .. } | Shape::Subtract { .. } | Shape::Intersect { .. } => String::new(),
    }
}

fn child_ref(shape: &Shape, step: Step) -> Option<&Shape> {
    match step {
        Step::First => match shape {
            Shape::Union { a, .. } | Shape::SmoothUnion { a, .. } | Shape::Intersect { a, .. } => {
                Some(&**a)
            }
            Shape::Subtract { kept, .. } => Some(&**kept),
            _ => None,
        },
        Step::Second => match shape {
            Shape::Union { b, .. } | Shape::SmoothUnion { b, .. } | Shape::Intersect { b, .. } => {
                Some(&**b)
            }
            Shape::Subtract { cutter, .. } => Some(&**cutter),
            _ => None,
        },
        Step::Sole => match shape {
            Shape::Repeat { child, .. } | Shape::Mirror { child, .. } => Some(&**child),
            _ => None,
        },
    }
}

fn child_mut(shape: &mut Shape, step: Step) -> Option<&mut Shape> {
    match step {
        Step::First => match shape {
            Shape::Union { a, .. } | Shape::SmoothUnion { a, .. } | Shape::Intersect { a, .. } => {
                Some(&mut **a)
            }
            Shape::Subtract { kept, .. } => Some(&mut **kept),
            _ => None,
        },
        Step::Second => match shape {
            Shape::Union { b, .. } | Shape::SmoothUnion { b, .. } | Shape::Intersect { b, .. } => {
                Some(&mut **b)
            }
            Shape::Subtract { cutter, .. } => Some(&mut **cutter),
            _ => None,
        },
        Step::Sole => match shape {
            Shape::Repeat { child, .. } | Shape::Mirror { child, .. } => Some(&mut **child),
            _ => None,
        },
    }
}

/// The node a path names, or `None` if the path walks off a leaf or through a node that has no such
/// child. There is no "closest match" here on purpose: an editor that edited the parent because the
/// child was gone would author a change the file does not describe.
pub fn resolve<'s>(root: &'s Shape, path: &[Step]) -> Option<&'s Shape> {
    let mut node = root;
    for step in path {
        node = child_ref(node, *step)?;
    }
    Some(node)
}

/// The mutable twin of [`resolve`].
pub fn resolve_mut<'s>(root: &'s mut Shape, path: &[Step]) -> Option<&'s mut Shape> {
    let mut node = root;
    for step in path {
        node = child_mut(node, *step)?;
    }
    Some(node)
}

/// One node of the tree as the panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The path from the root. Empty for the root itself.
    pub path: Vec<Step>,
    /// The names the authored file gives this node's children, positionally paired with
    /// [`children`]'s steps — so a row reads `Subtract.kept` rather than "the first one".
    pub name: String,
    pub kind: &'static str,
    pub summary: String,
    pub depth: usize,
}

/// Depth-first, root first: every node in the tree, each with the path that reaches it.
pub fn outline(root: &Shape) -> Vec<Entry> {
    let mut out = Vec::new();
    walk(root, &mut Vec::new(), "root", &mut out);
    out
}

fn walk(shape: &Shape, path: &mut Vec<Step>, name: &str, out: &mut Vec<Entry>) {
    out.push(Entry {
        path: path.clone(),
        name: name.to_string(),
        kind: kind(shape),
        summary: summary(shape),
        depth: path.len(),
    });
    for (child_name, step) in children(shape) {
        let child =
            resolve(shape, &[*step]).expect("children() named a step resolve() cannot take");
        path.push(*step);
        walk(child, path, &format!("{name}.{child_name}"), out);
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(r: f32) -> Shape {
        Shape::Sphere {
            r,
            centre: [0.0, 0.0, 0.0],
        }
    }

    /// A tree that touches every combinator shape the panel has to tell apart: two operands, a named
    /// kept/cutter pair, and a single-child wrapper.
    fn mixed() -> Shape {
        Shape::Union {
            a: Box::new(sphere(1.0)),
            b: Box::new(Shape::Subtract {
                kept: Box::new(Shape::RoundedBox {
                    half: [1.0, 1.0, 1.0],
                    r: 0.1,
                }),
                cutter: Box::new(Shape::Mirror {
                    axis: 1,
                    child: Box::new(Shape::SmoothUnion {
                        a: Box::new(sphere(0.4)),
                        b: Box::new(sphere(0.6)),
                        k: 0.15,
                    }),
                }),
            }),
        }
    }

    /// An independent traversal, written without [`children`] or [`resolve`], that the outline has to
    /// agree with. Two ways of counting that share a helper cannot disagree.
    fn count(shape: &Shape) -> usize {
        let two = |a: &Shape, b: &Shape| 1 + count(a) + count(b);
        let one = |c: &Shape| 1 + count(c);
        match shape {
            Shape::Sphere { .. } | Shape::RoundedBox { .. } | Shape::Plane { .. } => 1,
            Shape::Union { a, b } | Shape::SmoothUnion { a, b, .. } | Shape::Intersect { a, b } => {
                two(a, b)
            }
            Shape::Subtract { kept, cutter } => two(kept, cutter),
            Shape::Repeat { child, .. } | Shape::Mirror { child, .. } => one(child),
        }
    }

    #[test]
    fn the_outline_names_every_node_exactly_once() {
        let root = mixed();
        let listed = outline(&root);
        assert_eq!(
            listed.len(),
            count(&root),
            "the outline lost or duplicated a node"
        );
        assert_eq!(
            listed[0].path,
            Vec::<Step>::new(),
            "the root must be listed"
        );

        let seen: std::collections::HashSet<String> =
            listed.iter().map(|e| format!("{:?}", e.path)).collect();
        assert_eq!(seen.len(), listed.len(), "two nodes share one path");
    }

    #[test]
    fn every_outlined_path_resolves_to_the_node_the_outline_described() {
        let root = mixed();
        for entry in outline(&root) {
            let found = resolve(&root, &entry.path);
            assert!(
                found.is_some(),
                "the panel offered {} but nothing resolves it",
                entry.name
            );
            assert_eq!(kind(found.unwrap()), entry.kind, "{}", entry.name);
        }
    }

    #[test]
    fn a_path_that_walks_off_a_leaf_refuses_instead_of_editing_the_parent() {
        let mut root = mixed();
        // Union.a is a sphere: it has no children, so any step into it names a node that is not there.
        let leaf = [Step::First];
        assert_eq!(kind(resolve(&root, &leaf).unwrap()), "Sphere");
        assert!(
            resolve_mut(&mut root, &[Step::First, Step::Sole]).is_none(),
            "a step into a leaf handed back something to edit"
        );
        assert!(
            resolve_mut(&mut root, &[Step::Sole]).is_none(),
            "a Union has no `child`"
        );
    }

    #[test]
    fn editing_a_part_changes_that_part_and_leaves_its_sibling_alone() {
        let mut root = mixed();
        // root.b.Subtract.cutter.Mirror.child is the smooth union, and its `b` is the 0.6 sphere.
        let snout = [Step::Second, Step::Second, Step::Sole, Step::Second];
        if let Some(Shape::Sphere { r, .. }) = resolve_mut(&mut root, &snout) {
            *r = 0.9;
        } else {
            panic!("the deep path did not reach a sphere");
        }

        match resolve(&root, &snout) {
            Some(Shape::Sphere { r, .. }) => assert_eq!(*r, 0.9),
            other => panic!("the edit did not land: {other:?}"),
        }
        // The sibling of the node that was edited, and the root's other operand, are untouched.
        assert!(
            matches!(
                resolve(&root, &[Step::Second, Step::Second, Step::Sole, Step::First]),
                Some(Shape::Sphere { r, .. }) if *r == 0.4
            ),
            "editing one sphere moved the one beside it"
        );
        assert!(
            matches!(resolve(&root, &[Step::First]), Some(Shape::Sphere { r, .. }) if *r == 1.0),
            "editing a deep part moved the root's other operand"
        );
    }

    #[test]
    fn the_panel_and_the_compiler_name_the_same_field_for_a_deep_node() {
        // A tree the compiler has to refuse deep inside it, so the route in its error text names the
        // very node the panel lists. `assetc` only ever reorders `Union` children (`order::rebalance`),
        // so a `Subtract` root keeps the authored `kept`/`cutter` order and the routes stay comparable.
        let tree = Shape::Subtract {
            kept: Box::new(sphere(1.0)),
            cutter: Box::new(Shape::Mirror {
                axis: 7,
                child: Box::new(sphere(0.2)),
            }),
        };
        let refusal = assetc::lower(&tree).expect_err("an axis of 7 has to be refused");
        assert_eq!(
            refusal.path,
            vec!["Subtract", "cutter", "Mirror"],
            "the compiler's route stopped alternating kind and field, so this module's doc needs to move"
        );

        let mirror = outline(&tree)
            .into_iter()
            .find(|e| e.kind == "Mirror")
            .expect("the panel does not list the node the compiler refused");
        let is_kind = |seg: &str| {
            matches!(
                seg,
                "Sphere"
                    | "RoundedBox"
                    | "Plane"
                    | "Union"
                    | "SmoothUnion"
                    | "Subtract"
                    | "Intersect"
                    | "Repeat"
                    | "Mirror"
            )
        };
        let mut compiler_fields = Vec::new();
        for seg in refusal.path {
            if !is_kind(seg) {
                compiler_fields.push(seg);
            }
        }
        let panel_fields: Vec<&str> = mirror.name.split('.').skip(1).collect(); // drop `root`
        assert_eq!(
            panel_fields, compiler_fields,
            "the panel route and the compiler route name different fields, so an author holding an \
             error text cannot find the row it refers to"
        );
    }

    #[test]
    fn the_shipped_pig_can_be_reached_part_by_part() {
        let text = include_str!("../../../assets/entities/pig.ron");
        let def: assetc::EntityDef = ron::from_str(text).expect("pig.ron does not parse");
        let listed = outline(&def.shape);
        assert!(
            listed.len() > 8,
            "the animal is a handful of nodes, so this test proves nothing: {}",
            listed.len()
        );
        assert!(
            listed.iter().any(|e| e.depth >= 3),
            "nothing in the pig is nested, so reaching a part is not being exercised"
        );
        for entry in &listed {
            assert!(
                resolve(&def.shape, &entry.path).is_some(),
                "pig.ron lists {} at {}/{} but the path does not resolve",
                entry.name,
                entry.depth,
                listed.len()
            );
        }
    }
}
