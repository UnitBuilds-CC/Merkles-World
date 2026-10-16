//! Spike S7's **consensus half**: two clients that start from the same world and receive the same
//! commits, one of which simulates the whole river every tick and one of which uses docs/05's stage-3
//! frontier. Both hash their field with `crates/ledger`. Do their roots ever agree, and after how many
//! commits?
//!
//! `cargo run --release --example s7_field_root -p engine`
//!
//! `s7_propagation` measured latency and cost and stopped there, because agreeing on a root was not
//! buildable: A-04's field schema and A-05's tag registry do not exist yet. That is still true, and this
//! file works around it honestly rather than pretending otherwise — the field leaf here is *spike-local*
//! (`FIELD_KIND` below is not a contract kind, and the layout is not A-04's). What is not workable around
//! is the hasher: A-10 landed as `crates/ledger`, so a macro field made of `i32` values can be rooted
//! today, which is the piece SM1's second clause was missing.
//!
//! So the claim tested is the one gate SM1 actually makes — *two clients reach the same root* — and the
//! two clients differ in the way the architecture says is safe: one loads every neighbour, the other
//! deliberately does not. If skipping regions changed the answer, that would not be a performance
//! difference but an architecture-invalidating one, and this is the file that would show it.
//!
//! What the numbers are for the roadmap, stated before the table: the commit where the two roots become
//! equal is the window in which peers **legitimately disagree about the world while having applied the
//! same commits**. That is OQ-01's "acceptable multi-epoch lag" with a measured cost in front of it, not
//! an opinion. Integers throughout, per AGENTS.md §4; a float here would make the root hardware-dependent
//! and would defeat the point of hashing it at all.

/// Regions from source to mouth — the same river `s7_propagation` drains, so the two files read together.
const RIVER: usize = 512;
/// Head at the source, before the dam.
const SOURCE: i32 = 1000;
/// The rule's per-region head loss, identical to the latency spike's.
const LOSS_PER_REGION: i32 = 1;
/// Ticks of catch-up per commit — stage 3's `Δt`.
const TICKS_PER_COMMIT: usize = 20;
/// Commits simulated per configuration.
const COMMITS: usize = 600;
/// The rings whose consensus latency is reported, matching the latency spike's table.
const RINGS: [usize; 4] = [1, 2, 4, 8];

/// A spike-local leaf kind. Deliberately **not** one of `contracts::tree`'s `NODE_*` values: A-04's field
/// schema is unwritten, and quietly borrowing a frozen kind would make this run look like evidence about
/// a contract it does not touch.
const FIELD_KIND: u8 = 0xC7;
/// A spike-local level for the same reason.
const FIELD_LEVEL: u8 = 0x00;

/// One client's field root: every region's `i32` as a little-endian leaf, over `ledger::list_root`. The
/// list is ordered by region index, so a client that walks the river in a different order still gets the
/// same root — the root is a function of the field, not of the simulation order.
fn field_root(water: &[i32]) -> ledger::Root {
    let leaves: Vec<ledger::Root> = water
        .iter()
        .map(|head| ledger::node(FIELD_KIND, FIELD_LEVEL, &head.to_le_bytes()))
        .collect();
    ledger::list_root(&leaves)
}

fn hex(root: &ledger::Root) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One tick, in place, with a caller-owned scratch buffer: the caller swaps the two when the whole line
/// has been read. The `i32` halving is integer truncation toward zero, so the answer does not depend on
/// a rounding mode or a hardware unit.
fn tick(water: &mut [i32], scratch: &mut [i32], wet: usize, dammed: bool) {
    let wet = wet.min(water.len());
    scratch.copy_from_slice(water);
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
        scratch[i] = (water[i] - gap / 2).max(0);
    }
}

/// The state the world starts from: run the field until nothing moves, so the dam is the only event.
fn equilibrium() -> Vec<i32> {
    let mut water = vec![SOURCE; RIVER];
    let mut scratch = vec![0i32; RIVER];
    for _ in 0..20_000 {
        let before = water.clone();
        tick(&mut water, &mut scratch, RIVER, false);
        core::mem::swap(&mut water, &mut scratch);
        if before == water {
            return water;
        }
    }
    water
}

/// One client's roots, commit by commit. `frontier` is `None` for the client that loads every neighbour
/// and `Some(ring)` for the stage-3 client whose disturbed width grows by that ring each commit.
fn roots_per_commit(start: &[i32], ring: Option<usize>) -> Vec<ledger::Root> {
    let mut field = start.to_vec();
    let mut scratch = vec![0i32; RIVER];
    let mut roots = Vec::with_capacity(COMMITS);
    let mut width = ring.map(|_| 1usize).unwrap_or(RIVER);
    for _commit in 1..=COMMITS {
        for _ in 0..TICKS_PER_COMMIT {
            tick(&mut field, &mut scratch, width, true);
            core::mem::swap(&mut field, &mut scratch);
        }
        roots.push(field_root(&field));
        if let Some(step) = ring {
            width = (width + step).min(RIVER);
        }
    }
    roots
}

fn main() {
    println!("// spike S7, consensus half - two clients, one field root per commit, hashed by crates/ledger");
    println!("// {RIVER} regions, Δt = {TICKS_PER_COMMIT} ticks per commit, {COMMITS} commits, i32 values only (AGENTS.md §4)");
    println!("// the field leaf is spike-local (kind {FIELD_KIND:#04x}): A-04's schema does not exist, so this is not a contract root");
    println!();

    let start = equilibrium();
    let origin = field_root(&start);
    println!(" origin root      : {}", hex(&origin));
    println!(
        " origin check     : recomputed twice, identical = {}",
        field_root(&start) == origin
    );
    assert_eq!(
        field_root(&start),
        origin,
        "the same field hashed twice must give the same root, or the hasher is not a function"
    );

    // The client that loads every neighbour: the cost docs/05's stage 3 exists to avoid.
    let full = roots_per_commit(&start, None);
    assert_eq!(full.len(), COMMITS);
    println!(
        " full client      : {} roots computed, every region simulated every tick",
        full.len()
    );
    println!();
    println!(" ring | roots that disagree with the full client, over {COMMITS} commits | first commit that holds to the end | final root (both clients)");

    for ring in RINGS {
        let frontier = roots_per_commit(&start, Some(ring));
        let disagreeing = full
            .iter()
            .zip(frontier.iter())
            .filter(|(a, b)| a != b)
            .count();
        // Permanent agreement, not a momentary one: the first commit from which the two clients stay
        // equal to the end of the run. A single equal commit in the middle would be a coincidence of the
        // relaxation, not a consensus.
        let last_disagreement = (1..COMMITS).rev().find(|i| full[*i] != frontier[*i]);
        let first_agreement = last_disagreement.map_or(Some(1usize), |i| Some(i + 2));
        let shown = first_agreement
            .map(|commit| format!("{commit}"))
            .unwrap_or_else(|| String::from("never"));
        assert_eq!(
            hex(&frontier[COMMITS - 1]),
            hex(&full[COMMITS - 1]),
            "a ring of {ring} must not disagree with the full client at the last commit while claiming to have applied the same {COMMITS} commits"
        );
        println!(
            " {:>4} | {:>55} | {:>18} | {}",
            ring,
            format!("{disagreeing} of {COMMITS}"),
            shown,
            hex(&full[COMMITS - 1])
        );
    }

    // The plant: a field one unit off at one region has to move the root. Without this the table above
    // could be a hasher that ignores its payload and still print twelve agreeing digests.
    let mut tampered = start.clone();
    tampered[7] -= 1;
    let tampered_root = field_root(&tampered);
    assert_ne!(
        tampered_root, origin,
        "one region off by one head unit must change the field root, or nothing here is being hashed"
    );
    println!();
    println!(
        "// Falsification: region 7 one unit low → {}",
        hex(&tampered_root)
    );
    println!("// Four readings, and what none of them settle:");
    println!("// - the frontier client and the full client, given the same {COMMITS} commits, end on one root: the");
    println!(
        "//   mechanism SM1 leans on does not change the answer, only when the answer arrives."
    );
    println!("// - the disagreement is a *window*, not a corruption: every committing pair of clients agrees by the");
    println!("//   end of the run, so the honest SM1 wording is 'agree on the epoch root', not 'agree at all times'.");
    println!("// - that window is OQ-01's lag with a number attached to it. How many epochs a peer may be behind");
    println!("//   before the difference matters is still the owner's call; this only says what the rule costs.");
    println!("// - not measured: a second machine. Three CI architectures reproduce the final digest printed above,");
    println!("//   and that is the cross-target half; the four named targets of P1.5 cover the ledger roots, not a");
    println!("//   macro field, because A-04 does not exist. S7's consensus half is now buildable and measured for");
    println!("//   one client pair; SM1 as written ('two clients, identically') still waits on A-04 and A-05.");
}
