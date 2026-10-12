//! Spike S7: does docs/05's three-stage model propagate a distant consequence, how long does it take,
//! and what does it cost per commit?
//!
//! `cargo run --release --example s7_propagation -p engine`
//!
//! The proposal in docs/05 is that an action never simulates miles of terrain: stage 1 realizes the edit
//! locally, stage 2 accrues a bounded local effect every tick, and stage 3 — at commit — runs the field's
//! `Δt` catch-up for the affected regions **plus one ring of neighbours**. The document then asks the
//! question this spike exists to answer — *"how does region B know its neighbour's water table without
//! loading it?"* — and names the failure mode: ring too small and effects arrive late, ring too large and
//! commit cost explodes. Both halves of that are numbers, so this file makes a river out of integers and
//! reads them off.
//!
//! The mechanism under test, as the code implements it: a `frontier` marks the disturbed width. Each
//! commit simulates `Δt` ticks over **the frontier only** — regions past it keep their value untouched,
//! which is the operational meaning of "without loading it" — and then the frontier advances by `ring`
//! regions. A region can only change once the frontier has reached it, and that is the latency measured.
//!
//! What is a stand-in, said rather than implied: the field is the spike's own, because the macro rule
//! library and field schema are A-04 and OQ-01's unwritten surface, and the dam is an integer flag rather
//! than an event tag because A-05's registry is still OQ-08's. Nothing here is hashed and no epoch root
//! is computed, so **no gate SM1 claim is made**: latency and cost are the findings, consensus is not.
//! Values are `i32` throughout because AGENTS.md §4 puts macro field values in the seat a root will
//! someday depend on, and a float in that seat is the bug ADR-017 exists to stop.
//!
//! Informational output; no assertions, and timings labelled the way every other spike number here is.

/// Regions from source to mouth. 512 is a "long world" test, not a claim about content.
const RIVER: usize = 512;
/// Head at the source, before the dam.
const SOURCE: i32 = 1000;
/// Below this a region reads as dry: a lake bed, a failed well.
const DRY: i32 = 50;
/// Head lost per region along the chain. Honest note, because it turned out not to do what this line
/// claims: the reference equilibrium is a flat 1000 end to end, not a `SOURCE - distance` profile,
/// because relaxation halves the gap and a one-unit gap truncates to zero. The gradient never forms.
/// That is fine for the question being asked — how fast a disturbance travels — and it is a small
/// demonstration of why this field is integer: quantisation is a property of the model, not of the
/// hardware, and it is the same on every target.
const LOSS_PER_REGION: i32 = 1;
/// Ticks of catch-up per commit — the `Δt` of stage 3.
const TICKS_PER_COMMIT: usize = 20;
/// Commits simulated per configuration; long enough for ring 1 to reach the mouth.
const COMMITS: usize = 600;
/// The proposal's ring, and the alternatives OQ-01 would weigh against it.
const RINGS: [usize; 4] = [1, 2, 4, 8];
/// Distances from the dam whose arrival is reported.
const DISTANCES: [usize; 5] = [1, 5, 20, 100, 500];

/// One tick of the water table across `wet` regions: each relaxes toward what its upstream can feed it —
/// one unit of head per region — by half the gap, and only the wetted prefix changes. Integer division
/// truncates toward zero, so the same operands give the same answer on every target with no rounding mode
/// to disagree about, which is the whole reason this field is `i32`.
fn tick(water: &mut [i32], wet: usize, dammed: bool) {
    let wet = wet.min(water.len());
    let mut next = water.to_vec();
    for i in 0..wet {
        let upstream = if i == 0 {
            if dammed {
                0
            } else {
                SOURCE
            }
        } else {
            water[i - 1]
        };
        let target = (upstream - LOSS_PER_REGION).max(0);
        let gap = water[i] - target;
        next[i] = (water[i] - gap / 2).max(0);
    }
    water.copy_from_slice(&next);
}

/// The state the world starts from: run the field until nothing moves. "The river has always been here."
fn equilibrium() -> Vec<i32> {
    let mut water = vec![SOURCE; RIVER];
    for _ in 0..20_000 {
        let before = water.clone();
        tick(&mut water, RIVER, false);
        if before == water {
            return water;
        }
    }
    water
}

/// One configuration: the arrival commit at each queried distance, the mean regions simulated per commit,
/// and the region-ticks spent.
fn run(start: &[i32], ring: usize) -> (Vec<Option<usize>>, f64, u64) {
    let mut water = start.to_vec();
    let mut arrival = vec![None; RIVER];
    let mut frontier = 1usize;
    let mut region_ticks = 0u64;
    let mut regions_simulated = 0u64;

    for commit in 1..=COMMITS {
        for _ in 0..TICKS_PER_COMMIT {
            tick(&mut water, frontier, true);
            region_ticks += frontier as u64;
        }
        regions_simulated += frontier as u64;
        for i in 0..frontier.min(RIVER) {
            if water[i] < DRY && arrival[i].is_none() {
                arrival[i] = Some(commit);
            }
        }
        frontier = (frontier + ring).min(RIVER);
    }

    let latencies = DISTANCES.iter().map(|d| arrival[*d]).collect::<Vec<_>>();
    (
        latencies,
        regions_simulated as f64 / COMMITS as f64,
        region_ticks,
    )
}

fn main() {
    println!("// spike S7 - docs/05's three-stage model as a river of {RIVER} integer regions");
    println!("// dam at region 0; dry below {DRY}; Δt = {TICKS_PER_COMMIT} ticks per commit; {COMMITS} commits per ring");
    println!("// values are i32 by AGENTS.md §4; nothing is hashed, so this measures latency and cost, not consensus");
    println!();

    let start = equilibrium();
    println!(
        " equilibrium      : source {} | region 20 {} | region 100 {} | mouth {} (saturated everywhere; see LOSS_PER_REGION)",
        start[0],
        start[20],
        start[100],
        start[RIVER - 1]
    );
    println!();
    println!(" ring | epochs to dry, at distance 1 / 5 / 20 / 100 / 500 | mean regions simulated per commit | region-ticks | vs simulating everything");

    // The comparison stage 3 is claiming against: simulate the whole river every commit, which is what
    // "to know about the neighbour, load the neighbour" costs.
    let everything = (COMMITS * RIVER * TICKS_PER_COMMIT) as u64;

    for ring in RINGS {
        let (latencies, mean_simulated, region_ticks) = run(&start, ring);
        let shown = latencies
            .iter()
            .map(|maybe| match maybe {
                Some(commit) => format!("{commit}"),
                None => String::from("never"),
            })
            .collect::<Vec<_>>()
            .join(" / ");
        println!(
            " {:>4} | {:>47} | {:>35.1} | {:>10} | {:>6.0}%",
            ring,
            shown,
            mean_simulated,
            region_ticks,
            100.0 * region_ticks as f64 / everything as f64
        );
    }

    println!();
    println!("// Four readings of that table, and what none of them settle:");
    println!("// - latency is distance over ring, plus the ticks to drain. The failure mode docs/05 names");
    println!(
        "//   is real and linear: reaching the mouth takes 501 / 251 / 126 / 64 commits at ring"
    );
    println!("//   1 / 2 / 4 / 8. That is 'dam → riverbed next epoch → wells the epoch after', quantified.");
    println!(
        "// - the ring is not free, and the earlier draft of this commentary was wrong about that."
    );
    println!("//   Against simulating the whole river every commit, the narrowest ring spends 57% of the");
    println!("//   work and the widest 95%: a bigger front disturbs more area sooner, so latency is bought");
    println!("//   with work rather than taken for nothing. The saved fraction is a curve from 43% down to");
    println!("//   5%, not a cliff in either direction, and docs/05's 'too large and commit cost explodes'");
    println!("//   should be re-argued against that curve before it decides anything.");
    println!("// - what stage 3 does deliver, and the reason to keep it: a commit's cost is bounded by the");
    println!("//   disturbed area rather than by the world, and no region is loaded, simulated or written");
    println!("//   before the frontier reaches it. An action stays local in effect even when its cause was");
    println!("//   global in reach. That is the mechanism gate SM1 leans on, and it behaved as described.");
    println!("// - what no row answers is the other half of SM1: two clients agreeing on a ROOT. That needs");
    println!("//   A-04's field schema and A-05's tag registry, neither of which exists, so S7 stays open");
    println!("//   with its latency and cost halves measured and its consensus half unbuildable.");
}
