//! Deterministic evolving multi-thread block graph for resolver tests.
//!
//! Time is logical: every active thread produces one block per tick without
//! sleeping. Blocks reference the previous tick's neighbor tips, which keeps
//! the generated graph acyclic even though all threads advance concurrently.

use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;

use crate::{BlockId, BlockNode, BlockProvider, ThreadId, TimedBlock};

const LOAD_MAX: i32 = 10_000;
/// `65_455 * 330 ms = 21_600.15 s`: just over six logical hours.
pub const SIX_HOURS_TICKS_330_MS: u64 = 65_455;

#[derive(Clone, Debug)]
pub struct GeneratorConfig {
    pub seed: u64,
    pub ticks: u64,
    pub block_interval_ms: u64,
    pub start_unix_ms: u64,
    pub max_threads: usize,
    pub balance_every_ticks: u64,
    pub rebalance_cooldown_ticks: u64,
    pub split_load: i32,
    pub retire_load: i32,
    /// Maximum absolute change of a thread's latent demand per tick.
    pub demand_step: i32,
    /// Load approaches latent demand by this divisor. Larger is smoother.
    pub load_smoothing: i32,
    /// Probability that a block includes one ref to each available neighbor.
    pub cross_ref_probability_percent: u8,
    /// Number of recent blocks per neighbor eligible for that cross-ref.
    pub cross_ref_window: usize,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            seed: 0x4252_4931_3336,
            ticks: SIX_HOURS_TICKS_330_MS,
            block_interval_ms: 330,
            start_unix_ms: 1_700_000_000_000,
            max_threads: 200,
            balance_every_ticks: 12,
            rebalance_cooldown_ticks: 48,
            split_load: 8_000,
            retire_load: 1_200,
            demand_step: 180,
            load_smoothing: 16,
            cross_ref_probability_percent: 60,
            cross_ref_window: 10,
        }
    }
}

impl GeneratorConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.ticks > 0, "ticks must be > 0");
        anyhow::ensure!(self.block_interval_ms > 0, "block_interval_ms must be > 0");
        anyhow::ensure!(self.max_threads >= 1, "max_threads must be >= 1");
        anyhow::ensure!(self.max_threads <= 200, "max_threads must be <= 200");
        anyhow::ensure!(
            self.balance_every_ticks > 0,
            "balance_every_ticks must be > 0"
        );
        anyhow::ensure!(self.load_smoothing > 0, "load_smoothing must be > 0");
        anyhow::ensure!(
            (0..=LOAD_MAX).contains(&self.retire_load)
                && (0..=LOAD_MAX).contains(&self.split_load)
                && self.retire_load < self.split_load,
            "load thresholds must satisfy 0 <= retire_load < split_load <= {LOAD_MAX}"
        );
        anyhow::ensure!(self.demand_step >= 0, "demand_step must be >= 0");
        anyhow::ensure!(
            self.cross_ref_probability_percent <= 100,
            "cross_ref_probability_percent must be <= 100"
        );
        anyhow::ensure!(self.cross_ref_window > 0, "cross_ref_window must be > 0");
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    Split,
    Retire,
    RootSpawn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    pub tick: u64,
    pub kind: LifecycleKind,
    pub old_thread: ThreadId,
    pub new_threads: Vec<ThreadId>,
}

#[derive(Clone, Debug)]
pub struct GeneratedBlock {
    pub node: BlockNode,
    pub gen_utime_ms: u64,
    pub tick: u64,
    pub ordinal: u64,
    pub load_basis_points: i32,
}

#[derive(Clone, Debug)]
pub struct GeneratedGraph {
    pub blocks: Vec<GeneratedBlock>,
    pub lifecycle: Vec<LifecycleEvent>,
    pub active_thread_counts: Vec<usize>,
    pub peak_threads: usize,
    pub neighbor_refs_attempted: u64,
    pub neighbor_refs_included: u64,
}

impl GeneratedGraph {
    pub fn provider(self) -> SyntheticBlockProvider {
        SyntheticBlockProvider::new(self)
    }
}

#[derive(Clone, Debug)]
struct ThreadState {
    id: ThreadId,
    serial: u64,
    height: u64,
    tip: Option<BlockId>,
    initial_parent: Option<BlockId>,
    pending_refs: Vec<BlockId>,
    recent_blocks: VecDeque<BlockId>,
    load: i32,
    demand: i32,
    cooldown_until: u64,
    time_offset_ms: u64,
}

impl ThreadState {
    fn new(
        id: ThreadId,
        serial: u64,
        inherited_tip: Option<BlockId>,
        load: i32,
        tick: u64,
        time_offset_ms: u64,
    ) -> Self {
        Self {
            id,
            serial,
            height: 0,
            tip: None,
            initial_parent: inherited_tip,
            pending_refs: Vec::new(),
            recent_blocks: VecDeque::new(),
            load,
            demand: load,
            cooldown_until: tick,
            time_offset_ms,
        }
    }
}

pub struct TestGraphGenerator {
    config: GeneratorConfig,
    rng: DeterministicRng,
    next_thread_serial: u64,
    next_block_ordinal: u64,
}

impl TestGraphGenerator {
    pub fn new(config: GeneratorConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            rng: DeterministicRng::new(config.seed),
            config,
            next_thread_serial: 1,
            next_block_ordinal: 0,
        })
    }

    pub fn generate(mut self) -> GeneratedGraph {
        let root = ThreadState::new(
            ThreadId::ZERO,
            0,
            None,
            LOAD_MAX / 2,
            0,
            thread_time_offset_ms(self.config.seed, 0, self.config.block_interval_ms),
        );
        let mut active = vec![root];
        let mut blocks = Vec::new();
        let mut lifecycle = Vec::new();
        let mut active_thread_counts = Vec::with_capacity(self.config.ticks as usize);
        let mut peak_threads = 1usize;
        let mut neighbor_refs_attempted = 0u64;
        let mut neighbor_refs_included = 0u64;

        for tick in 0..self.config.ticks {
            peak_threads = peak_threads.max(active.len());
            active_thread_counts.push(active.len());
            self.update_loads(&mut active);
            let prior_windows: Vec<Vec<BlockId>> = active
                .iter()
                .map(|thread| thread.recent_blocks.iter().copied().collect())
                .collect();

            for (index, thread) in active.iter_mut().enumerate() {
                let mut refs = Vec::new();
                if let Some(parent) = thread.tip.or(thread.initial_parent.take()) {
                    refs.push(parent);
                }
                for (neighbor_index, window) in neighbor_windows(index, &prior_windows) {
                    neighbor_refs_attempted += 1;
                    if let Some(neighbor) = choose_neighbor_ref(
                        self.config.seed,
                        tick,
                        thread.serial,
                        neighbor_index,
                        window,
                        self.config.cross_ref_probability_percent,
                    ) {
                        neighbor_refs_included += 1;
                        push_unique(&mut refs, neighbor);
                    }
                }
                for handoff in std::mem::take(&mut thread.pending_refs) {
                    push_unique(&mut refs, handoff);
                }
                let block_id =
                    deterministic_block_id(self.config.seed, tick, thread.serial, thread.height);
                let node = BlockNode {
                    block_id,
                    thread_id: thread.id,
                    height: thread.height,
                    refs,
                };
                blocks.push(GeneratedBlock {
                    node,
                    gen_utime_ms: self.config.start_unix_ms
                        + tick * self.config.block_interval_ms
                        + thread.time_offset_ms,
                    tick,
                    ordinal: self.next_block_ordinal,
                    load_basis_points: thread.load,
                });
                self.next_block_ordinal += 1;
                thread.tip = Some(block_id);
                thread.recent_blocks.push_back(block_id);
                while thread.recent_blocks.len() > self.config.cross_ref_window {
                    thread.recent_blocks.pop_front();
                }
                thread.height += 1;
            }

            if (tick + 1) % self.config.balance_every_ticks == 0 {
                self.rebalance(tick + 1, &mut active, &mut lifecycle);
            }
        }

        GeneratedGraph {
            blocks,
            lifecycle,
            active_thread_counts,
            peak_threads,
            neighbor_refs_attempted,
            neighbor_refs_included,
        }
    }

    fn update_loads(&mut self, active: &mut [ThreadState]) {
        for thread in active {
            let demand_delta = self
                .rng
                .range_i32(-self.config.demand_step, self.config.demand_step);
            thread.demand = (thread.demand + demand_delta).clamp(0, LOAD_MAX);
            thread.load += (thread.demand - thread.load) / self.config.load_smoothing;
        }
    }

    fn rebalance(
        &mut self,
        tick: u64,
        active: &mut Vec<ThreadState>,
        lifecycle: &mut Vec<LifecycleEvent>,
    ) {
        let decision = active
            .iter()
            .enumerate()
            .filter(|(_, thread)| tick >= thread.cooldown_until)
            .max_by_key(|(_, thread)| {
                if thread.load >= self.config.split_load {
                    thread.load - self.config.split_load + LOAD_MAX
                } else if !thread.id.is_zero() && thread.load <= self.config.retire_load {
                    self.config.retire_load - thread.load
                } else {
                    -1
                }
            })
            .and_then(|(index, thread)| {
                let actionable = thread.load >= self.config.split_load
                    || (!thread.id.is_zero() && thread.load <= self.config.retire_load);
                actionable.then_some((index, thread.load >= self.config.split_load))
            });
        let Some((index, split)) = decision else {
            return;
        };

        if split {
            let root_split = active[index].id.is_zero();
            if active.len().saturating_add(if root_split { 2 } else { 1 }) > self.config.max_threads
            {
                return;
            }
            self.split_thread(tick, index, active, lifecycle);
        } else {
            self.retire_thread(tick, index, active, lifecycle);
        }
    }

    fn split_thread(
        &mut self,
        tick: u64,
        index: usize,
        active: &mut Vec<ThreadState>,
        lifecycle: &mut Vec<LifecycleEvent>,
    ) {
        let old = active[index].clone();
        let child_load = (old.load / 2).max(self.config.retire_load + 1);
        let left = self.new_thread(old.tip, child_load, tick);
        let right = self.new_thread(old.tip, child_load, tick);
        let new_threads = vec![left.id, right.id];

        if old.id.is_zero() {
            let root = &mut active[index];
            root.load = child_load;
            root.demand = child_load;
            root.cooldown_until = tick + self.config.rebalance_cooldown_ticks;
        } else {
            active.remove(index);
        }
        let insertion = if old.id.is_zero() { index + 1 } else { index };
        active.insert(insertion, left);
        active.insert(insertion + 1, right);
        lifecycle.push(LifecycleEvent {
            tick,
            kind: if old.id.is_zero() {
                LifecycleKind::RootSpawn
            } else {
                LifecycleKind::Split
            },
            old_thread: old.id,
            new_threads,
        });
    }

    fn retire_thread(
        &mut self,
        tick: u64,
        index: usize,
        active: &mut Vec<ThreadState>,
        lifecycle: &mut Vec<LifecycleEvent>,
    ) {
        let old = active.remove(index);
        if let Some(tip) = old.tip {
            let receiver_index = index.saturating_sub(1).min(active.len() - 1);
            if let Some(receiver) = active.get_mut(receiver_index) {
                push_unique(&mut receiver.pending_refs, tip);
            }
        }
        lifecycle.push(LifecycleEvent {
            tick,
            kind: LifecycleKind::Retire,
            old_thread: old.id,
            new_threads: Vec::new(),
        });
    }

    fn new_thread(&mut self, inherited_tip: Option<BlockId>, load: i32, tick: u64) -> ThreadState {
        let serial = self.next_thread_serial;
        self.next_thread_serial += 1;
        ThreadState::new(
            deterministic_thread_id(self.config.seed, serial),
            serial,
            inherited_tip,
            load,
            tick + self.config.rebalance_cooldown_ticks,
            thread_time_offset_ms(self.config.seed, serial, self.config.block_interval_ms),
        )
    }
}

fn neighbor_windows(index: usize, windows: &[Vec<BlockId>]) -> Vec<(u64, &[BlockId])> {
    let mut result = Vec::new();
    if index > 0 && !windows[index - 1].is_empty() {
        result.push((0, windows[index - 1].as_slice()));
    }
    if index + 1 < windows.len() && !windows[index + 1].is_empty() {
        result.push((1, windows[index + 1].as_slice()));
    }
    result
}

fn push_unique(values: &mut Vec<BlockId>, value: BlockId) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn choose_neighbor_ref(
    seed: u64,
    tick: u64,
    thread_serial: u64,
    neighbor_index: u64,
    window: &[BlockId],
    probability_percent: u8,
) -> Option<BlockId> {
    debug_assert!(!window.is_empty());
    if probability_percent == 0 {
        return None;
    }
    let sample = splitmix64(
        seed ^ tick.rotate_left(13)
            ^ thread_serial.rotate_left(29)
            ^ neighbor_index.rotate_left(41),
    );
    if sample % 100 >= u64::from(probability_percent) {
        return None;
    }
    let choice = splitmix64(sample) % u64::try_from(window.len()).expect("window length fits u64");
    Some(window[usize::try_from(choice).expect("window index fits usize")])
}

fn deterministic_thread_id(seed: u64, serial: u64) -> ThreadId {
    let mut bytes = [0u8; 34];
    fill_deterministic(&mut bytes, seed ^ 0x5448_5245_4144, serial, 0, 0);
    ThreadId::from_bytes(bytes)
}

fn thread_time_offset_ms(seed: u64, serial: u64, block_interval_ms: u64) -> u64 {
    splitmix64(seed ^ 0x5449_4d45 ^ serial.rotate_left(23)) % block_interval_ms
}

fn deterministic_block_id(seed: u64, tick: u64, thread: u64, height: u64) -> BlockId {
    let mut bytes = [0u8; 32];
    fill_deterministic(&mut bytes, seed ^ 0x0042_4c4f_434b, tick, thread, height);
    BlockId::from_bytes(bytes)
}

fn fill_deterministic(bytes: &mut [u8], seed: u64, a: u64, b: u64, c: u64) {
    let mut state = seed ^ a.rotate_left(17) ^ b.rotate_left(31) ^ c.rotate_left(47);
    for chunk in bytes.chunks_mut(8) {
        state = splitmix64(state);
        chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = splitmix64(self.0);
        self.0
    }

    fn range_i32(&mut self, start: i32, end: i32) -> i32 {
        if start == end {
            return start;
        }
        let width = i64::from(end) - i64::from(start) + 1;
        start
            + i32::try_from(self.next_u64() % u64::try_from(width).expect("positive range"))
                .expect("range fits i32")
    }
}

pub struct SyntheticBlockProvider {
    blocks: Vec<GeneratedBlock>,
    by_id: HashMap<BlockId, usize>,
    by_thread_height: HashMap<(ThreadId, u64), usize>,
    thread_tips: HashMap<ThreadId, usize>,
    calls: ProviderCallCounters,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderCallCounts {
    pub block_by_id: u64,
    pub latest_blocks: u64,
    pub timed_block_by_id: u64,
    pub timed_block_by_height: u64,
    pub latest_timed_block_in_thread: u64,
}

impl ProviderCallCounts {
    pub fn total(self) -> u64 {
        self.block_by_id
            + self.latest_blocks
            + self.timed_block_by_id
            + self.timed_block_by_height
            + self.latest_timed_block_in_thread
    }

    pub fn since(self, earlier: Self) -> Self {
        Self {
            block_by_id: self.block_by_id - earlier.block_by_id,
            latest_blocks: self.latest_blocks - earlier.latest_blocks,
            timed_block_by_id: self.timed_block_by_id - earlier.timed_block_by_id,
            timed_block_by_height: self.timed_block_by_height - earlier.timed_block_by_height,
            latest_timed_block_in_thread: self.latest_timed_block_in_thread
                - earlier.latest_timed_block_in_thread,
        }
    }

    pub fn add(&mut self, other: Self) {
        self.block_by_id += other.block_by_id;
        self.latest_blocks += other.latest_blocks;
        self.timed_block_by_id += other.timed_block_by_id;
        self.timed_block_by_height += other.timed_block_by_height;
        self.latest_timed_block_in_thread += other.latest_timed_block_in_thread;
    }
}

#[derive(Default)]
struct ProviderCallCounters {
    block_by_id: AtomicU64,
    latest_blocks: AtomicU64,
    timed_block_by_id: AtomicU64,
    timed_block_by_height: AtomicU64,
    latest_timed_block_in_thread: AtomicU64,
}

impl SyntheticBlockProvider {
    pub fn new(graph: GeneratedGraph) -> Self {
        let mut by_id = HashMap::new();
        let mut by_thread_height = HashMap::new();
        let mut thread_tips = HashMap::new();
        for (index, block) in graph.blocks.iter().enumerate() {
            by_id.insert(block.node.block_id, index);
            by_thread_height.insert((block.node.thread_id, block.node.height), index);
            thread_tips
                .entry(block.node.thread_id)
                .and_modify(|tip: &mut usize| {
                    if graph.blocks[*tip].node.height < block.node.height {
                        *tip = index;
                    }
                })
                .or_insert(index);
        }
        Self {
            blocks: graph.blocks,
            by_id,
            by_thread_height,
            thread_tips,
            calls: ProviderCallCounters::default(),
        }
    }

    pub fn blocks(&self) -> &[GeneratedBlock] {
        &self.blocks
    }

    pub fn call_counts(&self) -> ProviderCallCounts {
        ProviderCallCounts {
            block_by_id: self.calls.block_by_id.load(Ordering::Relaxed),
            latest_blocks: self.calls.latest_blocks.load(Ordering::Relaxed),
            timed_block_by_id: self.calls.timed_block_by_id.load(Ordering::Relaxed),
            timed_block_by_height: self.calls.timed_block_by_height.load(Ordering::Relaxed),
            latest_timed_block_in_thread: self
                .calls
                .latest_timed_block_in_thread
                .load(Ordering::Relaxed),
        }
    }

    fn timed(&self, index: usize) -> TimedBlock {
        TimedBlock {
            block: self.blocks[index].node.clone(),
            gen_utime_ms: self.blocks[index].gen_utime_ms,
        }
    }
}

#[async_trait]
impl BlockProvider for SyntheticBlockProvider {
    async fn block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
        self.calls.block_by_id.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .by_id
            .get(id)
            .map(|index| self.blocks[*index].node.clone()))
    }

    async fn latest_blocks(&self, limit: usize) -> anyhow::Result<Vec<BlockNode>> {
        self.calls.latest_blocks.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .blocks
            .iter()
            .rev()
            .take(limit)
            .map(|block| block.node.clone())
            .collect())
    }

    async fn timed_block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<TimedBlock>> {
        self.calls.timed_block_by_id.fetch_add(1, Ordering::Relaxed);
        Ok(self.by_id.get(id).map(|index| self.timed(*index)))
    }

    async fn timed_block_by_height(
        &self,
        thread: &ThreadId,
        height: u64,
    ) -> anyhow::Result<Option<TimedBlock>> {
        self.calls
            .timed_block_by_height
            .fetch_add(1, Ordering::Relaxed);
        Ok(self
            .by_thread_height
            .get(&(*thread, height))
            .map(|index| self.timed(*index)))
    }

    async fn latest_timed_block_in_thread(
        &self,
        thread: &ThreadId,
    ) -> anyhow::Result<Option<TimedBlock>> {
        self.calls
            .latest_timed_block_in_thread
            .fetch_add(1, Ordering::Relaxed);
        Ok(self.thread_tips.get(thread).map(|index| self.timed(*index)))
    }

    fn namespace(&self) -> &str {
        "synthetic"
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap, HashSet},
        io::{self, Write},
        sync::Arc,
    };

    use super::*;
    use crate::{
        GraphResolver, MemoryStore, ResolutionAlgorithm, ResolutionPolicy, ResolutionRequest,
        ResolverLimits,
    };

    fn evolving_config() -> GeneratorConfig {
        GeneratorConfig {
            ticks: SIX_HOURS_TICKS_330_MS,
            balance_every_ticks: 3,
            rebalance_cooldown_ticks: 6,
            split_load: 5_700,
            retire_load: 4_300,
            demand_step: 900,
            load_smoothing: 8,
            max_threads: 200,
            ..GeneratorConfig::default()
        }
    }

    #[test]
    fn generated_graph_is_deterministic_acyclic_and_obeys_limits() {
        let first = TestGraphGenerator::new(evolving_config())
            .unwrap()
            .generate();
        let second = TestGraphGenerator::new(evolving_config())
            .unwrap()
            .generate();
        println!(
            "generated ticks={} peak_threads={} blocks={} lifecycle_events={} neighbor_refs={}/{}",
            first.active_thread_counts.len(),
            first.peak_threads,
            first.blocks.len(),
            first.lifecycle.len(),
            first.neighbor_refs_included,
            first.neighbor_refs_attempted
        );
        assert_eq!(first.blocks.len(), second.blocks.len());
        assert_eq!(first.lifecycle, second.lifecycle);
        assert_eq!(
            (first.neighbor_refs_attempted, first.neighbor_refs_included),
            (
                second.neighbor_refs_attempted,
                second.neighbor_refs_included
            )
        );
        assert_eq!(
            first
                .blocks
                .iter()
                .map(|block| &block.node)
                .collect::<Vec<_>>(),
            second
                .blocks
                .iter()
                .map(|block| &block.node)
                .collect::<Vec<_>>()
        );
        assert!(first.peak_threads <= 200);
        assert!(first.neighbor_refs_attempted > 0);
        let observed_cross_ref_percent =
            first.neighbor_refs_included * 100 / first.neighbor_refs_attempted;
        assert!(
            (58..=62).contains(&observed_cross_ref_percent),
            "expected about 60% neighbor refs, got {observed_cross_ref_percent}%"
        );
        assert!(first
            .lifecycle
            .iter()
            .any(|event| event.kind == LifecycleKind::RootSpawn));
        assert!(first
            .lifecycle
            .iter()
            .any(|event| event.kind == LifecycleKind::Split));
        assert!(first
            .lifecycle
            .iter()
            .any(|event| event.kind == LifecycleKind::Retire));

        let ordinal_by_id: HashMap<_, _> = first
            .blocks
            .iter()
            .map(|block| (block.node.block_id, block.ordinal))
            .collect();
        assert_eq!(
            ordinal_by_id.len(),
            first.blocks.len(),
            "block ids must be unique"
        );
        let rebalanced: HashSet<_> = first
            .lifecycle
            .iter()
            .map(|event| ((event.tick), event.old_thread))
            .collect();
        let mut previous_by_thread = HashMap::new();
        let mut previous_load = HashMap::new();
        let mut time_offset_by_thread = HashMap::new();
        let mut blocks_per_tick = vec![0usize; evolving_config().ticks as usize];
        for block in &first.blocks {
            blocks_per_tick[block.tick as usize] += 1;
            let tick_time =
                evolving_config().start_unix_ms + block.tick * evolving_config().block_interval_ms;
            let offset = block.gen_utime_ms - tick_time;
            assert!(
                offset < evolving_config().block_interval_ms,
                "thread timestamp offset must stay inside its tick"
            );
            if let Some(previous_offset) =
                time_offset_by_thread.insert(block.node.thread_id, offset)
            {
                assert_eq!(
                    offset, previous_offset,
                    "a thread must retain its deterministic timestamp offset"
                );
            }
            let mut unique = HashSet::new();
            for reference in &block.node.refs {
                assert!(unique.insert(*reference), "duplicate reference slot");
                assert!(
                    ordinal_by_id[reference] < block.ordinal,
                    "reference must point to an older block"
                );
            }
            if let Some(previous) =
                previous_by_thread.insert(block.node.thread_id, block.node.block_id)
            {
                assert_eq!(
                    block.node.refs.first(),
                    Some(&previous),
                    "slot 0 must continue the thread parent chain"
                );
            } else if block.node.thread_id.is_zero() {
                assert!(block.node.refs.is_empty(), "root genesis has no parent");
            } else {
                assert!(
                    !block.node.refs.is_empty(),
                    "a spawned thread must inherit the split tip"
                );
            }
            if let Some(previous) =
                previous_load.insert(block.node.thread_id, block.load_basis_points)
            {
                if !rebalanced.contains(&(block.tick, block.node.thread_id)) {
                    assert!(
                        (block.load_basis_points - previous).abs()
                            <= LOAD_MAX / evolving_config().load_smoothing,
                        "load changed too abruptly without a rebalance"
                    );
                }
            }
        }
        assert_eq!(blocks_per_tick, first.active_thread_counts);
        assert!(
            time_offset_by_thread
                .values()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                > 1,
            "different threads should receive different timestamp offsets"
        );

        let root_blocks = first
            .blocks
            .iter()
            .filter(|block| block.node.thread_id.is_zero())
            .count();
        assert_eq!(root_blocks, evolving_config().ticks as usize);
    }

    #[tokio::test]
    async fn resolver_reports_hop_statistics_for_1000_historical_targets() {
        let graph = TestGraphGenerator::new(evolving_config())
            .unwrap()
            .generate();
        let mut candidates_by_tick = BTreeMap::<u64, Vec<BlockId>>::new();
        for block in graph.blocks.iter().filter(|block| {
            !block.node.thread_id.is_zero() && block.tick + 100 < evolving_config().ticks
        }) {
            candidates_by_tick
                .entry(block.tick)
                .or_default()
                .push(block.node.block_id);
        }
        let candidates_by_tick: Vec<_> = candidates_by_tick.into_iter().collect();
        assert!(candidates_by_tick.len() >= 1_000);
        let targets: Vec<_> = (0..1_000)
            .map(|sample| {
                let tick_index = sample * (candidates_by_tick.len() - 1) / 999;
                let (tick, blocks) = &candidates_by_tick[tick_index];
                // Rotate the choice within each time slice instead of always
                // sampling the same thread when many are active.
                (blocks[sample % blocks.len()], *tick)
            })
            .collect();
        assert_eq!(
            targets
                .iter()
                .map(|(id, _)| *id)
                .collect::<HashSet<_>>()
                .len(),
            1_000,
            "time-stratified targets must be distinct"
        );

        let algorithms = match std::env::var("RESOLVER_BENCH_ALGORITHM").as_deref() {
            Ok("reverse-index") => vec![ResolutionAlgorithm::ReverseIndex],
            Ok("forward-thread") => vec![ResolutionAlgorithm::ForwardThread],
            Ok(value) => panic!("unsupported RESOLVER_BENCH_ALGORITHM={value}"),
            Err(_) => vec![
                ResolutionAlgorithm::ReverseIndex,
                ResolutionAlgorithm::ForwardThread,
            ],
        };
        for algorithm in algorithms {
            let provider = Arc::new(graph.clone().provider());
            let resolver = GraphResolver::new(provider.clone(), Arc::new(MemoryStore::new()), 40)
                .with_algorithm(algorithm);
            let started = std::time::Instant::now();
            let mut hops = Vec::with_capacity(1_000);
            let mut histogram = BTreeMap::<usize, usize>::new();
            let mut provider_calls = Vec::with_capacity(1_000);
            let mut provider_call_histogram = BTreeMap::<u64, usize>::new();
            let mut elapsed_micros = Vec::with_capacity(1_000);
            let mut successful_elapsed_micros = Vec::with_capacity(1_000);
            let mut aggregate_calls = ProviderCallCounts::default();
            let mut failures = 0_usize;
            for (sample, (target, tick)) in targets.iter().copied().enumerate() {
                let calls_before = provider.call_counts();
                let resolve_started = std::time::Instant::now();
                let result = resolver
                    .resolve(ResolutionRequest {
                        target,
                        policy: ResolutionPolicy::FirstValid,
                        limits: ResolverLimits {
                            max_hops: 500,
                            max_visited_blocks: 10_000,
                        },
                    })
                    .await;
                let resolve_elapsed_micros = resolve_started.elapsed().as_micros();
                elapsed_micros.push(resolve_elapsed_micros);
                let call_delta = provider.call_counts().since(calls_before);
                aggregate_calls.add(call_delta);
                *provider_call_histogram
                    .entry(call_delta.total())
                    .or_default() += 1;
                provider_calls.push(call_delta.total());
                match result {
                    Ok(path) => {
                        assert_eq!(path.target, target);
                        assert!(!path.hops.is_empty());
                        assert_eq!(path.hops.first().unwrap().from, path.anchor);
                        assert_eq!(path.hops.last().unwrap().to, target);
                        *histogram.entry(path.hops.len()).or_default() += 1;
                        hops.push(path.hops.len());
                        successful_elapsed_micros.push(resolve_elapsed_micros);
                        println!(
                            "algorithm={algorithm:?} resolve sample={} tick={} status=ok hops={} \
                             provider_calls={} elapsed_us={resolve_elapsed_micros}",
                            sample + 1,
                            tick,
                            path.hops.len(),
                            call_delta.total(),
                        );
                    },
                    Err(error) => {
                        failures += 1;
                        println!(
                            "algorithm={algorithm:?} resolve sample={} tick={} status=error \
                             provider_calls={} elapsed_us={resolve_elapsed_micros} error={error:#}",
                            sample + 1,
                            tick,
                            call_delta.total(),
                        );
                    },
                }
                io::stdout().flush().unwrap();
            }
            hops.sort_unstable();
            provider_calls.sort_unstable();
            elapsed_micros.sort_unstable();
            successful_elapsed_micros.sort_unstable();
            let total_hops: usize = hops.iter().sum();
            let total_provider_calls: u64 = provider_calls.iter().sum();
            let total_elapsed_micros: u128 = elapsed_micros.iter().sum();
            let total_successful_elapsed_micros: u128 = successful_elapsed_micros.iter().sum();
            let hop_p95 = (hops.len() - 1) * 95 / 100;
            let success_time_p95 = (successful_elapsed_micros.len() - 1) * 95 / 100;
            println!(
                "algorithm={algorithm:?} resolved_1000 successes={} failures={} \
                 histogram={histogram:?} hops_min={} hops_median={} hops_p95={} hops_max={} \
                 hops_mean={:.2}",
                hops.len(),
                failures,
                hops[0],
                hops[hops.len() / 2],
                hops[hop_p95],
                hops[hops.len() - 1],
                total_hops as f64 / hops.len() as f64,
            );
            println!(
                "algorithm={algorithm:?} provider_calls histogram={provider_call_histogram:?} \
                 min={} median={} p95={} max={} mean={:.2} local_zero={} \
                 aggregate={aggregate_calls:?}",
                provider_calls[0],
                provider_calls[499],
                provider_calls[949],
                provider_calls[999],
                total_provider_calls as f64 / provider_calls.len() as f64,
                provider_calls.iter().filter(|calls| **calls == 0).count(),
            );
            println!(
                "algorithm={algorithm:?} timing wall_ms={} all_mean_us={:.2} all_median_us={} \
                 all_p95_us={} all_max_us={} success_mean_us={:.2} success_median_us={} \
                 success_p95_us={} success_max_us={}",
                started.elapsed().as_millis(),
                total_elapsed_micros as f64 / elapsed_micros.len() as f64,
                elapsed_micros[elapsed_micros.len() / 2],
                elapsed_micros[949],
                elapsed_micros[999],
                total_successful_elapsed_micros as f64 / successful_elapsed_micros.len() as f64,
                successful_elapsed_micros[successful_elapsed_micros.len() / 2],
                successful_elapsed_micros[success_time_p95],
                successful_elapsed_micros[successful_elapsed_micros.len() - 1],
            );
            assert_eq!(hops.len() + failures, 1_000);
            assert!(!hops.is_empty());
        }
    }
}
