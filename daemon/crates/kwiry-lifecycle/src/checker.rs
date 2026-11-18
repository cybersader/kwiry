//! Bounded explicit-state model checker with crash injection.
//!
//! **Safety.** Breadth-first search over every interleaving of every enabled
//! action (holder actors, the authority and network, faults, crashes,
//! scenario changes, the operator), checking [`props::check_state`] in every
//! reachable state, [`props::check_step`] on every transition, and I7 (some
//! healthy actor can move) everywhere. BFS order makes every counterexample a
//! shortest trace.
//!
//! **Liveness (L1).** Over the graph of *healthy* transitions only, from
//! every reachable state, no execution that is strongly fair to every
//! healthy action class avoids the goal forever. A strongly connected set of
//! non-goal states admits such an execution exactly when every fairness class
//! enabled somewhere in it also has a transition that stays inside it;
//! otherwise the states enabling an escaping class are removed and the rest
//! is decomposed again (the standard refinement for strong fairness). A
//! non-goal state with no healthy successor is a dead end.
//!
//! **Storage.** States are identified by 128-bit fingerprints (two
//! independent 64-bit hashes), as in TLC's hash compaction; only the BFS
//! frontier keeps full states. With at most 10⁸ states the probability that
//! a fingerprint collision hides a state is below 10⁻²². Traces are rebuilt
//! by replaying the recorded action indices from the initial state.
//!
//! **Why not `stateright`.** Its `eventually` properties are only checked on
//! terminal paths, so a fair cycle that never reaches the goal (the shape of
//! a stranded report) would pass. The fairness analysis needs the healthy
//! subgraph anyway, and this checker has no dependencies.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::time::{Duration, Instant};

use crate::action::{Action, Kind};
use crate::design::Design;
use crate::props::{self, Prop};
use crate::row_id::RowId;
use crate::state::*;
use crate::step::{Config, Outcome, Step, canonical, exec};

/// One bounded run.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub name: &'static str,
    pub design: Design,
    pub budget: Budget,
    /// Enabled fault families (`action::faults`).
    pub faults: u16,
    /// `U1` starts as superseded garbage of R0 (work for GC).
    pub garbage: bool,
    /// Explore optional counts-only same-state reports.
    pub same_state: bool,
    pub liveness: bool,
    /// Properties not to report (`Prop::bit()`), to look past one violation
    /// for another.
    pub skip: u32,
    /// Abort (as a bound failure) beyond this many states.
    pub max_states: usize,
}

#[derive(Clone, Debug)]
pub struct TraceStep {
    pub action: Action,
    pub rows: Vec<RowId>,
}

#[derive(Clone, Debug)]
pub struct Counterexample {
    pub prop: Prop,
    /// From the initial state to the violating transition (or to the entry
    /// of the non-progress cycle).
    pub prefix: Vec<TraceStep>,
    /// For L1: a fair cycle that never reaches the goal (empty for a dead
    /// end or a safety violation).
    pub cycle: Vec<TraceStep>,
    /// The last state (or the cycle's entry), summarized.
    pub state: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub profile: &'static str,
    pub design: Design,
    pub states: usize,
    pub transitions: u64,
    pub healthy_edges: usize,
    pub max_frontier: usize,
    pub depth: usize,
    pub elapsed: Duration,
    /// Per `RowId::index()`: cited by some explored transition.
    pub rows_seen: Vec<bool>,
    pub violation: Option<Counterexample>,
}

// ----------------------------------------------------------------- hashing

/// FxHash-style mixing with a strong finalizer.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.write_u64(u64::from(*b));
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(u64::from(i));
    }
    fn write_u16(&mut self, i: u16) {
        self.write_u64(u64::from(i));
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// The key is already a fingerprint; the map uses its low bits as is.
#[derive(Default)]
struct Pass(u64);

impl Hasher for Pass {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("only fingerprints are hashed")
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }
}

/// A 128-bit fingerprint stored at 4-byte alignment, so a `seen` entry is
/// 20 bytes rather than the 32 a `u128` key pads to.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fp([u32; 4]);

impl Hash for Fp {
    fn hash<H: Hasher>(&self, h: &mut H) {
        h.write_u64(u64::from(self.0[0]) | (u64::from(self.0[1]) << 32));
    }
}

fn fingerprint(s: &State) -> Fp {
    let mut a = Fx::default();
    s.hash(&mut a);
    let mut b = DefaultHasher::new();
    s.hash(&mut b);
    let (a, b) = (a.finish(), b.finish());
    Fp([b as u32, (b >> 32) as u32, a as u32, (a >> 32) as u32])
}

// ----------------------------------------------------------------- explore

struct Graph {
    seen: HashMap<Fp, u32, BuildHasherDefault<Pass>>,
    parent: Vec<u32>,
    via: Vec<u16>,
    goal: Vec<bool>,
    off: Vec<u32>,
    to: Vec<u32>,
    key: Vec<u16>,
}

impl Graph {
    fn edges(&self, v: u32) -> impl Iterator<Item = (u16, u32)> + '_ {
        let (a, b) = (
            self.off[v as usize] as usize,
            self.off[v as usize + 1] as usize,
        );
        self.key[a..b]
            .iter()
            .copied()
            .zip(self.to[a..b].iter().copied())
    }

    /// Candidate indices from the initial state to `v`.
    fn path_to(&self, mut v: u32) -> Vec<u16> {
        let mut out = Vec::new();
        while self.parent[v as usize] != u32::MAX {
            out.push(self.via[v as usize]);
            v = self.parent[v as usize];
        }
        out.reverse();
        out
    }
}

/// A BFS frontier stored in fixed-size chunks.
#[derive(Default)]
struct Frontier {
    chunks: VecDeque<Vec<State>>,
    len: usize,
}

impl Frontier {
    const CHUNK: usize = 1 << 14;

    fn push(&mut self, s: State) {
        if self.chunks.back().is_none_or(|c| c.len() == Self::CHUNK) {
            self.chunks.push_back(Vec::with_capacity(Self::CHUNK));
        }
        self.chunks.back_mut().expect("just ensured").push(s);
        self.len += 1;
    }
}

/// One step of the explored quotient: [`exec`], then [`canonical`].
fn quotient(s: &State, a: Action, cfg: &Config) -> Step {
    let mut out = exec(s, a, cfg)?;
    if let Some(o) = out.as_mut() {
        canonical(&mut o.next, cfg);
    }
    Ok(out)
}

struct Run {
    cfg: Config,
    candidates: Vec<Action>,
    initial: State,
}

impl Run {
    fn init(&self) -> State {
        let mut s = self.initial.clone();
        canonical(&mut s, &self.cfg);
        s
    }

    fn step(&self, s: &State, ci: u16) -> Outcome {
        quotient(s, self.candidates[ci as usize], &self.cfg)
            .expect("replayed steps were checked")
            .expect("replayed steps were enabled")
    }

    /// Replay a path, returning the trace and the final state.
    fn replay(&self, path: &[u16]) -> (Vec<TraceStep>, State) {
        let mut s = self.init();
        let mut trace = Vec::new();
        for &ci in path {
            let out = self.step(&s, ci);
            trace.push(TraceStep {
                action: self.candidates[ci as usize],
                rows: out.rows.iter().collect(),
            });
            s = out.next;
        }
        (trace, s)
    }

    fn safety_cex(
        &self,
        g: &Graph,
        prop: Prop,
        at: u32,
        via: Option<u16>,
        detail: String,
    ) -> Counterexample {
        let mut path = g.path_to(at);
        path.extend(via);
        let (prefix, last) = self.replay(&path);
        Counterexample {
            prop,
            prefix,
            cycle: Vec::new(),
            state: summary(&last),
            detail,
        }
    }
}

pub fn check(p: &Profile) -> Report {
    let b = p.budget;
    // U1 must be permanently absent, and neither room can have a distinct
    // scenario role. Restrict this quotient to retirement/crash profiles:
    // faults, changes and restores aren't needed for this reduction.
    let both_rooms = bit(0) | bit(1);
    let symmetric = !p.garbage
        && b.retire_rooms == both_rooms
        && (b.change_rooms == 0 || b.change_rooms == both_rooms)
        && b.faults == 0
        && b.changes == 0
        && b.drops == 0
        && b.restores == 0;
    explore(
        p,
        State::initial(b, p.garbage),
        Action::candidates(),
        symmetric,
    )
}

/// Explore a focused subsystem from a particular fixture, with its explicit
/// candidate set. Room symmetry is disabled: the initial roles or candidates
/// may not be interchangeable. Otherwise the same checker is used.
pub fn check_from(p: &Profile, initial: State, candidates: Vec<Action>) -> Report {
    explore(p, initial, candidates, false)
}

fn explore(p: &Profile, initial: State, candidates: Vec<Action>, symmetric: bool) -> Report {
    let started = Instant::now();
    let run = Run {
        cfg: Config {
            design: p.design,
            faults: p.faults,
            same_state: p.same_state,
        },
        candidates,
        initial,
    };
    let first_retire_r0 = if symmetric {
        run.candidates
            .iter()
            .position(|a| *a == Action::AuthRetire(0))
            .expect("full candidate set") as u16
    } else {
        u16::MAX
    };
    let mut g = Graph {
        seen: HashMap::default(),
        parent: vec![u32::MAX],
        via: vec![u16::MAX],
        goal: Vec::new(),
        off: vec![0],
        to: Vec::new(),
        key: Vec::new(),
    };
    let init = run.init();
    g.seen.insert(fingerprint(&init), 0);
    let mut keys: HashMap<Action, u16> = HashMap::new();
    let mut rows_seen = vec![false; RowId::ALL.len()];
    let mut transitions = 0u64;
    let mut violation = None;
    let mut max_frontier = 1;
    let mut depth = 0;

    let mut layer = Frontier::default();
    layer.push(init);
    let mut layer_start = 0u32;
    'bfs: while layer.len != 0 {
        max_frontier = max_frontier.max(layer.len);
        let mut next = Frontier::default();
        let mut k = 0u32;
        // Chunks are freed as soon as they are expanded, so the peak holds
        // about one frontier rather than two. Order (and so state ids) is
        // unchanged.
        while let Some(chunk) = layer.chunks.pop_front() {
            for s in &chunk {
                let here = layer_start + k;
                k += 1;
                g.goal.push(props::goal(s));
                if let Some(prop) = props::check_state(s, p.skip) {
                    violation = Some(run.safety_cex(&g, prop, here, None, String::new()));
                    break 'bfs;
                }
                let mut healthy = false;
                for (ci, &a) in run.candidates.iter().enumerate() {
                    let ci = ci as u16;
                    let out = match quotient(s, a, &run.cfg) {
                        Ok(Some(out)) => out,
                        Ok(None) => continue,
                        Err(b) => {
                            violation = Some(run.safety_cex(
                                &g,
                                Prop::Bound,
                                here,
                                None,
                                format!("{b} (taking {a:?})"),
                            ));
                            break 'bfs;
                        }
                    };
                    transitions += 1;
                    for r in out.rows.iter() {
                        rows_seen[r.index()] = true;
                    }
                    if let Some(prop) = props::check_step(s, &out, p.skip) {
                        violation = Some(run.safety_cex(&g, prop, here, Some(ci), String::new()));
                        break 'bfs;
                    }
                    let mut successor = out.next;
                    let mut via = ci;
                    if symmetric
                        && a == Action::AuthRetire(1)
                        && s.ghost.retired_at == [None; ROOMS]
                    {
                        // Until the first retirement, both rooms stay active and
                        // mapped. No obligation, per-room work or unattributed
                        // data exists; all reachable states are fixed by the
                        // room swap. AuthRetire changes only these two indexed
                        // fields (the unused journal was erased by canonical()).
                        successor.auth.core.rooms.swap(0, 1);
                        successor.ghost.retired_at.swap(0, 1);
                        via = first_retire_r0;
                    }
                    let fp = fingerprint(&successor);
                    let j = match g.seen.get(&fp) {
                        Some(&j) => j,
                        None => {
                            let j = u32::try_from(g.parent.len()).expect("fewer than 2^32 states");
                            g.seen.insert(fp, j);
                            g.parent.push(here);
                            g.via.push(via);
                            next.push(successor);
                            j
                        }
                    };
                    if a.healthy() {
                        healthy = true;
                        if p.liveness {
                            let n = keys.len() as u16;
                            g.to.push(j);
                            g.key.push(*keys.entry(a.fairness_key()).or_insert(n));
                        }
                    }
                }
                if p.liveness {
                    g.off
                        .push(u32::try_from(g.to.len()).expect("fewer than 2^32 edges"));
                }
                if !healthy && !props::settled(s) && p.skip & Prop::I7Deadlock.bit() == 0 {
                    violation =
                        Some(run.safety_cex(&g, Prop::I7Deadlock, here, None, String::new()));
                    break 'bfs;
                }
                if g.parent.len() > p.max_states {
                    let detail = format!("more than {} states", p.max_states);
                    violation = Some(run.safety_cex(&g, Prop::Bound, here, None, detail));
                    break 'bfs;
                }
            }
        }
        layer_start += layer.len as u32;
        layer = next;
        depth += 1;
    }
    drop(layer);

    if violation.is_none() && p.liveness && p.skip & Prop::L1Liveness.bit() == 0 {
        violation = liveness(&run, &g, keys.len());
    }

    Report {
        profile: p.name,
        design: p.design,
        states: g.parent.len(),
        transitions,
        healthy_edges: g.to.len(),
        max_frontier,
        depth,
        elapsed: started.elapsed(),
        rows_seen,
        violation,
    }
}

// ---------------------------------------------------------------- liveness

struct Tarjan {
    index: Vec<u32>,
    low: Vec<u32>,
    on: Vec<bool>,
}

impl Tarjan {
    fn sccs(&mut self, nodes: &[u32], mark: &[u32], token: u32, g: &Graph) -> Vec<Vec<u32>> {
        const UNSEEN: u32 = u32::MAX;
        let mut out = Vec::new();
        let mut counter = 0u32;
        let mut stack: Vec<u32> = Vec::new();
        let mut call: Vec<(u32, u32)> = Vec::new();
        for &root in nodes {
            if self.index[root as usize] != UNSEEN {
                continue;
            }
            self.open(root, &mut counter, &mut stack);
            call.push((root, g.off[root as usize]));
            while let Some(&mut (v, ref mut pos)) = call.last_mut() {
                if *pos < g.off[v as usize + 1] {
                    let w = g.to[*pos as usize];
                    *pos += 1;
                    if mark[w as usize] != token {
                        continue;
                    }
                    if self.index[w as usize] == UNSEEN {
                        self.open(w, &mut counter, &mut stack);
                        call.push((w, g.off[w as usize]));
                    } else if self.on[w as usize] {
                        self.low[v as usize] = self.low[v as usize].min(self.index[w as usize]);
                    }
                } else {
                    call.pop();
                    if let Some(&(u, _)) = call.last() {
                        self.low[u as usize] = self.low[u as usize].min(self.low[v as usize]);
                    }
                    if self.low[v as usize] == self.index[v as usize] {
                        let mut scc = Vec::new();
                        loop {
                            let w = stack.pop().expect("v is on the stack");
                            self.on[w as usize] = false;
                            scc.push(w);
                            if w == v {
                                break;
                            }
                        }
                        out.push(scc);
                    }
                }
            }
        }
        for &v in nodes {
            self.index[v as usize] = UNSEEN;
        }
        out
    }

    fn open(&mut self, v: u32, counter: &mut u32, stack: &mut Vec<u32>) {
        self.index[v as usize] = *counter;
        self.low[v as usize] = *counter;
        *counter += 1;
        stack.push(v);
        self.on[v as usize] = true;
    }
}

fn liveness(run: &Run, g: &Graph, nkeys: usize) -> Option<Counterexample> {
    let n = g.parent.len();
    let nongoal: Vec<u32> = (0..n as u32).filter(|v| !g.goal[*v as usize]).collect();
    for &v in &nongoal {
        if g.edges(v).next().is_none() {
            let (prefix, last) = run.replay(&g.path_to(v));
            return Some(Counterexample {
                prop: Prop::L1Liveness,
                prefix,
                cycle: Vec::new(),
                state: summary(&last),
                detail: "dead end: no healthy action is enabled and an obligation is open".into(),
            });
        }
    }
    let mut mark = vec![0u32; n];
    let mut token = 1u32;
    for &v in &nongoal {
        mark[v as usize] = token;
    }
    let mut tarjan = Tarjan {
        index: vec![u32::MAX; n],
        low: vec![0; n],
        on: vec![false; n],
    };
    let mut work = tarjan.sccs(&nongoal, &mark, token, g);
    let mut enabled = vec![false; nkeys];
    let mut internal = vec![false; nkeys];
    while let Some(scc) = work.pop() {
        token += 1;
        for &v in &scc {
            mark[v as usize] = token;
        }
        let nontrivial = scc.len() > 1 || g.edges(scc[0]).any(|(_, w)| w == scc[0]);
        if !nontrivial {
            continue;
        }
        enabled.iter_mut().for_each(|x| *x = false);
        internal.iter_mut().for_each(|x| *x = false);
        for &v in &scc {
            for (k, w) in g.edges(v) {
                enabled[k as usize] = true;
                if mark[w as usize] == token {
                    internal[k as usize] = true;
                }
            }
        }
        let escaping: Vec<bool> = (0..nkeys).map(|k| enabled[k] && !internal[k]).collect();
        if !escaping.iter().any(|x| *x) {
            return Some(fair_cycle(run, g, &scc, &mark, token));
        }
        let rest: Vec<u32> = scc
            .iter()
            .copied()
            .filter(|v| !g.edges(*v).any(|(k, _)| escaping[k as usize]))
            .collect();
        token += 1;
        for &v in &rest {
            mark[v as usize] = token;
        }
        work.extend(tarjan.sccs(&rest, &mark, token, g));
    }
    None
}

fn fair_cycle(run: &Run, g: &Graph, scc: &[u32], mark: &[u32], token: u32) -> Counterexample {
    // The member closest to the initial state gives the shortest prefix.
    let entry = *scc.iter().min().expect("non-empty SCC");
    let entry_path = g.path_to(entry);
    let (prefix, entry_state) = run.replay(&entry_path);
    // Shortest healthy loop from the entry back to itself inside the SCC.
    let mut prev: HashMap<u32, u32> = HashMap::new();
    let mut queue = VecDeque::from([entry]);
    let mut closing = None;
    'search: while let Some(v) = queue.pop_front() {
        for (_, w) in g.edges(v) {
            if mark[w as usize] != token {
                continue;
            }
            if w == entry {
                closing = Some(v);
                break 'search;
            }
            if let std::collections::hash_map::Entry::Vacant(e) = prev.entry(w) {
                e.insert(v);
                queue.push_back(w);
            }
        }
    }
    let mut loop_states = vec![entry];
    if let Some(mut v) = closing {
        let mut back = vec![v];
        while v != entry {
            v = prev[&v];
            back.push(v);
        }
        back.reverse();
        loop_states = back;
        loop_states.push(entry);
    }
    let mut cycle = Vec::new();
    let mut s = entry_state.clone();
    for w in loop_states.windows(2) {
        let target = w[1];
        let found = run.candidates.iter().enumerate().find_map(|(ci, a)| {
            if !a.healthy() {
                return None;
            }
            match quotient(&s, *a, &run.cfg) {
                Ok(Some(out)) if g.seen.get(&fingerprint(&out.next)) == Some(&target) => {
                    Some((ci, out))
                }
                _ => None,
            }
        });
        let Some((ci, out)) = found else { break };
        cycle.push(TraceStep {
            action: run.candidates[ci],
            rows: out.rows.iter().collect(),
        });
        s = out.next;
    }
    Counterexample {
        prop: Prop::L1Liveness,
        prefix,
        cycle,
        state: summary(&entry_state),
        detail: format!(
            "a strongly fair healthy execution can stay among {} non-goal states forever",
            scc.len()
        ),
    }
}

// ------------------------------------------------------------------ output

/// A summary of the fields that matter for a counterexample.
pub fn summary(s: &State) -> String {
    let mut out = String::new();
    let a = &s.auth.core;
    let _ = write!(out, "authority: epoch {} seq {}", a.epoch, a.seq);
    for (r, room) in a.rooms.iter().enumerate() {
        let _ = write!(
            out,
            "; R{r} {:?} accepted {:?}",
            room.phase,
            room.obl
                .and_then(|o| o.accepted)
                .map(|x| (x.state, x.epoch))
        );
    }
    if let Some(i) = a.instr {
        let _ = write!(out, "; instruction {} manifest {:03b}", i.id, i.manifest);
    }
    let d = &s.dur;
    let _ = write!(
        out,
        "\nholder durable: hw {:?} tomb {:02b} accept {:?} floors {:?} units {:?} instr {:?}",
        d.hw.map(|h| (h.epoch, h.seq)),
        d.tomb,
        d.accept.map(|x| ((x.from.epoch, x.from.seq), x.to)),
        d.floors.map(|f| f.map(|f| (f.epoch, f.state))),
        d.units,
        d.instr
    );
    for c in 0..CHANS {
        if let Some(p) = d.slots[c] {
            let _ = write!(
                out,
                "\n  slot {c}: {:?} {:?} earlier={} vol={:?}",
                p.st, p.key, p.earlier, s.vol.slotv[c]
            );
        }
        if !d.uncertain[c].is_empty() {
            let _ = write!(out, "\n  uncertain {c}: {:?}", d.uncertain[c]);
        }
    }
    let v = &s.vol;
    let _ = write!(
        out,
        "\nholder volatile: {:?} route {:?} classes {:?} block {:02b} inv {:03b} umem {:?} apply {:?} rsv {:?} relist {}",
        v.phase, v.route, v.class, v.block, v.inv, v.umem, v.apply, v.rsv, v.relist
    );
    let _ = write!(
        out,
        "\nservable: {:?}; admitted list: {:?}",
        (0..ROOMS)
            .map(|r| crate::step::servable(s, r))
            .collect::<Vec<_>>(),
        v.admitted.map(|l| l.items)
    );
    if s.env.disk_full {
        out.push_str("\nvolume: full");
    }
    out
}

impl Counterexample {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.prop.describe());
        if !self.detail.is_empty() {
            let _ = writeln!(out, "  ({})", self.detail);
        }
        let fmt = |out: &mut String, n: usize, t: &TraceStep| {
            let rows: Vec<_> = t.rows.iter().map(|r| r.name()).collect();
            let kind = match t.action.kind() {
                Kind::Holder => "holder",
                Kind::Env => "env",
                Kind::Operator => "operator",
                Kind::Fault => "FAULT",
                Kind::Crash => "CRASH",
                Kind::Scenario => "scenario",
            };
            let _ = writeln!(out, "  {n:>3}. [{kind}] {:?} {}", t.action, rows.join(" "));
        };
        for (n, t) in self.prefix.iter().enumerate() {
            fmt(&mut out, n + 1, t);
        }
        if !self.cycle.is_empty() {
            let _ = writeln!(out, "  then forever (fair cycle):");
            for (n, t) in self.cycle.iter().enumerate() {
                fmt(&mut out, n + 1, t);
            }
        }
        let _ = writeln!(out, "  state:\n    {}", self.state.replace('\n', "\n    "));
        out
    }

    /// The row ids the counterexample's steps cite, in order, deduplicated.
    pub fn rows(&self) -> Vec<RowId> {
        let mut out: Vec<RowId> = Vec::new();
        for t in self.prefix.iter().chain(self.cycle.iter()) {
            for r in &t.rows {
                if !out.contains(r) {
                    out.push(*r);
                }
            }
        }
        out
    }
}

impl Report {
    pub fn render(&self) -> String {
        let mut out = format!(
            "profile {} ({:?}): {} states, {} transitions, {} healthy edges, depth {}, widest layer {}, {:.1?}",
            self.profile,
            self.design,
            self.states,
            self.transitions,
            self.healthy_edges,
            self.depth,
            self.max_frontier,
            self.elapsed
        );
        match &self.violation {
            None => out.push_str(", all properties hold\n"),
            Some(c) => {
                out.push_str(", VIOLATION\n");
                out.push_str(&c.render());
            }
        }
        out
    }
}

// ------------------------------------------------------------------ script

/// How a scripted run ended early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptEnd {
    /// Step `n` was not enabled.
    Disabled(usize, Action),
    /// Step `n` (or the state it reached) violated a property.
    Violation(usize, Prop),
}

/// Run a fixed action sequence from the initial state, checking every
/// property after each step. Returns the final state, or how it ended early.
pub fn script(p: &Profile, actions: &[Action]) -> Result<State, ScriptEnd> {
    let cfg = Config {
        design: p.design,
        faults: p.faults,
        same_state: p.same_state,
    };
    let mut s = State::initial(p.budget, p.garbage);
    for (n, &a) in actions.iter().enumerate() {
        let out = match exec(&s, a, &cfg) {
            Ok(Some(out)) => out,
            Ok(None) => return Err(ScriptEnd::Disabled(n, a)),
            Err(_) => return Err(ScriptEnd::Violation(n, Prop::Bound)),
        };
        if let Some(prop) = props::check_step(&s, &out, p.skip) {
            return Err(ScriptEnd::Violation(n, prop));
        }
        s = out.next;
        if let Some(prop) = props::check_state(&s, p.skip) {
            return Err(ScriptEnd::Violation(n, prop));
        }
    }
    Ok(s)
}
