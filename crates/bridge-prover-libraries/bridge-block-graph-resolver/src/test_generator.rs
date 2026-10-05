//! Deterministic evolving multi-thread block graph for resolver tests.
//!
//! Time is logical and advances in fixed ticks without sleeping. Every thread
//! has its own drifting block interval and may skip ticks. A collapsing thread
//! ends by emitting a tombstone. Every emitted block is queued for all other
//! live threads, grouped by source thread; consumers reference each group's
//! newest item and discard older pending items. This keeps the graph acyclic
//! even when many threads become due on the same tick. Thread zero is permanent
//! and never collapses.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;

use crate::{BlockId, BlockNode, BlockProvider, ProofBlock, ThreadId, TimedBlock};

const LOAD_MAX: i32 = 10_000;
/// `65_455 * 330 ms = 21_600.15 s`: just over six logical hours.
pub const SIX_HOURS_TICKS_330_MS: u64 = 65_455;
pub const TEN_MINUTES_MS: u64 = 10 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct GeneratorConfig {
    pub seed: u64,
    pub ticks: u64,
    pub block_interval_ms: u64,
    /// Inclusive range for each thread's drifting block-production interval.
    pub min_thread_block_interval_ms: u64,
    pub max_thread_block_interval_ms: u64,
    /// Maximum interval random-walk step after a normal block.
    pub thread_block_interval_step_ms: u64,
    pub start_unix_ms: u64,
    pub max_threads: usize,
    /// Percentage of `max_threads` retained before growth resumes.
    pub cooling_floor_percent: u8,
    /// Minimum duration of each cooling phase.
    pub minimum_cooling_ticks: u64,
    pub balance_every_ticks: u64,
    pub rebalance_cooldown_ticks: u64,
    pub split_load: i32,
    pub collapse_load: i32,
    /// Maximum absolute change of a thread's latent demand per tick.
    pub demand_step: i32,
    /// Load approaches latent demand by this divisor. Larger is smoother.
    pub load_smoothing: i32,
    /// Inclusion probability for each ordinary inbox selected by the fanout
    /// budget.
    pub cross_ref_probability_percent: u8,
    /// Maximum number of ordinary source-thread inboxes sampled per block.
    /// Tombstones are mandatory and do not consume this budget.
    pub max_cross_refs_per_block: usize,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            seed: 0x4252_4931_3336,
            ticks: SIX_HOURS_TICKS_330_MS,
            block_interval_ms: 330,
            min_thread_block_interval_ms: 330,
            max_thread_block_interval_ms: 1_500,
            thread_block_interval_step_ms: 120,
            start_unix_ms: 1_700_000_000_000,
            max_threads: 200,
            cooling_floor_percent: 55,
            minimum_cooling_ticks: 300,
            balance_every_ticks: 12,
            rebalance_cooldown_ticks: 48,
            split_load: 8_000,
            collapse_load: 1_200,
            demand_step: 180,
            load_smoothing: 16,
            cross_ref_probability_percent: 60,
            max_cross_refs_per_block: 6,
        }
    }
}

impl GeneratorConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.ticks > 0, "ticks must be > 0");
        anyhow::ensure!(self.block_interval_ms > 0, "block_interval_ms must be > 0");
        anyhow::ensure!(
            self.min_thread_block_interval_ms >= self.block_interval_ms,
            "min_thread_block_interval_ms must be >= block_interval_ms"
        );
        anyhow::ensure!(
            self.max_thread_block_interval_ms >= self.min_thread_block_interval_ms,
            "thread block interval range must be non-empty"
        );
        anyhow::ensure!(
            self.max_thread_block_interval_ms < u64::MAX,
            "max_thread_block_interval_ms must be < u64::MAX"
        );
        anyhow::ensure!(
            self.thread_block_interval_step_ms <= i64::MAX as u64,
            "thread_block_interval_step_ms must fit i64"
        );
        anyhow::ensure!(
            self.ticks
                .checked_mul(self.block_interval_ms)
                .and_then(|duration| self.start_unix_ms.checked_add(duration))
                .and_then(|end| end.checked_add(self.max_thread_block_interval_ms))
                .is_some(),
            "configured logical time range overflows u64"
        );
        anyhow::ensure!(self.max_threads >= 1, "max_threads must be >= 1");
        anyhow::ensure!(self.max_threads <= 200, "max_threads must be <= 200");
        anyhow::ensure!(
            (1..=100).contains(&self.cooling_floor_percent),
            "cooling_floor_percent must be in 1..=100"
        );
        anyhow::ensure!(
            self.balance_every_ticks > 0,
            "balance_every_ticks must be > 0"
        );
        anyhow::ensure!(self.load_smoothing > 0, "load_smoothing must be > 0");
        anyhow::ensure!(
            (0..=LOAD_MAX).contains(&self.collapse_load)
                && (0..=LOAD_MAX).contains(&self.split_load)
                && self.collapse_load < self.split_load,
            "load thresholds must satisfy 0 <= collapse_load < split_load <= {LOAD_MAX}"
        );
        anyhow::ensure!(self.demand_step >= 0, "demand_step must be >= 0");
        anyhow::ensure!(
            self.cross_ref_probability_percent <= 100,
            "cross_ref_probability_percent must be <= 100"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    Split,
    Collapse,
    RootSpawn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    pub tick: u64,
    pub kind: LifecycleKind,
    pub old_thread: ThreadId,
    pub new_threads: Vec<ThreadId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedBlock {
    pub node: BlockNode,
    pub gen_utime_ms: u64,
    pub tick: u64,
    pub ordinal: u64,
    pub load_basis_points: i32,
    pub is_tombstone: bool,
}

#[derive(Clone, Debug)]
pub struct GeneratedGraph {
    proof_seed: u64,
    pub blocks: Vec<GeneratedBlock>,
    pub lifecycle: Vec<LifecycleEvent>,
    pub active_thread_counts: Vec<usize>,
    pub total_threads: usize,
    pub peak_threads: usize,
    /// Ticks that switched load evolution from growth to cooling.
    pub cooling_started_ticks: Vec<u64>,
    /// Ticks that resumed growth after reaching the cooling floor.
    pub growth_resumed_ticks: Vec<u64>,
    pub queued_refs_attempted: u64,
    pub queued_refs_included: u64,
    pub tombstone_refs_included: u64,
    pub thread_lifetime: ThreadLifetimeStats,
    pub thread_counts_by_10_minutes: Vec<ThreadCountPeriodStats>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThreadLifetimeStats {
    pub completed_threads: usize,
    pub surviving_threads: usize,
    pub min_ms: u64,
    pub median_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
    pub mean_ms: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThreadCountPeriodStats {
    pub start_offset_ms: u64,
    pub duration_ms: u64,
    pub sampled_ticks: usize,
    pub created_threads: usize,
    pub min_active_threads: usize,
    pub max_active_threads: usize,
    pub mean_active_threads: f64,
}

impl GeneratedGraph {
    pub fn provider(self) -> SyntheticBlockProvider {
        SyntheticBlockProvider::new(self)
    }

    /// Build reachability from all thread-0 blocks. Thread-0 anchors are
    /// processed oldest first, so the first mark also identifies the earliest
    /// anchor that can reach a block.
    pub fn accessibility_map(&self) -> anyhow::Result<BlockAccessibilityMap> {
        let by_id: HashMap<_, _> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.node.block_id, index))
            .collect();
        let mut entries = HashMap::with_capacity(self.blocks.len());
        let mut stack = Vec::new();

        for (anchor_index, anchor) in self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.node.thread_id.is_zero())
        {
            if entries.contains_key(&anchor.node.block_id) {
                continue;
            }
            stack.push(anchor_index);
            while let Some(index) = stack.pop() {
                let block = &self.blocks[index];
                if entries.contains_key(&block.node.block_id) {
                    continue;
                }
                entries.insert(block.node.block_id, BlockAccessibility {
                    earliest_anchor: anchor.node.block_id,
                    earliest_anchor_height: anchor.node.height,
                    earliest_anchor_tick: anchor.tick,
                    delay_ms: anchor.gen_utime_ms.saturating_sub(block.gen_utime_ms),
                });
                for reference in &block.node.refs {
                    let referenced_index = by_id.get(reference).copied().ok_or_else(|| {
                        anyhow::anyhow!(
                            "generated block {} references missing block {reference}",
                            block.node.block_id
                        )
                    })?;
                    if !entries.contains_key(reference) {
                        stack.push(referenced_index);
                    }
                }
            }
        }

        Ok(BlockAccessibilityMap {
            entries,
            total_blocks: self.blocks.len(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockAccessibility {
    pub earliest_anchor: BlockId,
    pub earliest_anchor_height: u64,
    pub earliest_anchor_tick: u64,
    pub delay_ms: u64,
}

#[derive(Clone, Debug)]
pub struct BlockAccessibilityMap {
    entries: HashMap<BlockId, BlockAccessibility>,
    total_blocks: usize,
}

impl BlockAccessibilityMap {
    pub fn get(&self, block_id: &BlockId) -> Option<&BlockAccessibility> {
        self.entries.get(block_id)
    }

    pub fn is_reachable(&self, block_id: &BlockId) -> bool {
        self.entries.contains_key(block_id)
    }

    pub fn reachable_blocks(&self) -> usize {
        self.entries.len()
    }

    pub fn unreachable_blocks(&self) -> usize {
        self.total_blocks - self.entries.len()
    }

    pub fn total_blocks(&self) -> usize {
        self.total_blocks
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThreadPhase {
    Active,
    Collapsing,
    Tombstoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadStrategy {
    Growth,
    Cooling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IncomingBlock {
    block_id: BlockId,
    is_tombstone: bool,
}

#[derive(Clone, Copy, Debug)]
struct ThreadTiming {
    cooldown_until_tick: u64,
    block_interval_ms: u64,
    next_block_at_ms: u64,
    created_at_ms: u64,
}

#[derive(Clone, Debug)]
struct ThreadState {
    id: ThreadId,
    serial: u64,
    height: u64,
    tip: Option<BlockId>,
    initial_parent: Option<BlockId>,
    inbox: BTreeMap<ThreadId, VecDeque<IncomingBlock>>,
    load: i32,
    demand: i32,
    cooldown_until: u64,
    block_interval_ms: u64,
    next_block_at_ms: u64,
    created_at_ms: u64,
    cross_ref_cursor: usize,
    phase: ThreadPhase,
}

impl ThreadState {
    fn new(
        id: ThreadId,
        serial: u64,
        inherited_tip: Option<BlockId>,
        load: i32,
        timing: ThreadTiming,
    ) -> Self {
        Self {
            id,
            serial,
            height: 0,
            tip: None,
            initial_parent: inherited_tip,
            inbox: BTreeMap::new(),
            load,
            demand: load,
            cooldown_until: timing.cooldown_until_tick,
            block_interval_ms: timing.block_interval_ms,
            next_block_at_ms: timing.next_block_at_ms,
            created_at_ms: timing.created_at_ms,
            cross_ref_cursor: 0,
            phase: ThreadPhase::Active,
        }
    }

    fn is_live(&self) -> bool {
        self.phase != ThreadPhase::Tombstoned
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
        let root_interval = initial_thread_interval_ms(&self.config, self.config.seed, 0);
        let root = ThreadState::new(ThreadId::ZERO, 0, None, LOAD_MAX / 2, ThreadTiming {
            cooldown_until_tick: 0,
            block_interval_ms: root_interval,
            next_block_at_ms: self.config.start_unix_ms
                + thread_time_offset_ms(self.config.seed, 0, self.config.block_interval_ms),
            created_at_ms: self.config.start_unix_ms,
        });
        let mut active = vec![root];
        let mut blocks = Vec::new();
        let mut lifecycle = Vec::new();
        let mut active_thread_counts = Vec::with_capacity(self.config.ticks as usize);
        let mut peak_threads = 1usize;
        let mut load_strategy = LoadStrategy::Growth;
        let mut cooling_until_tick = 0;
        let mut cooling_started_ticks = Vec::new();
        let mut growth_resumed_ticks = Vec::new();
        let mut queued_refs_attempted = 0u64;
        let mut queued_refs_included = 0u64;
        let mut tombstone_refs_included = 0u64;
        let mut completed_lifetimes_ms = Vec::new();
        let simulation_end_ms =
            self.config.start_unix_ms + self.config.ticks * self.config.block_interval_ms;

        for tick in 0..self.config.ticks {
            let tick_time_ms = self.config.start_unix_ms + tick * self.config.block_interval_ms;
            let live_threads = active.iter().filter(|thread| thread.is_live()).count();
            peak_threads = peak_threads.max(live_threads);
            active_thread_counts.push(live_threads);
            if load_strategy == LoadStrategy::Growth && live_threads >= self.config.max_threads {
                load_strategy = LoadStrategy::Cooling;
                cooling_started_ticks.push(tick);
                cooling_until_tick = tick.saturating_add(self.config.minimum_cooling_ticks);
            } else if load_strategy == LoadStrategy::Cooling
                && tick >= cooling_until_tick
                && live_threads <= cooling_thread_floor(&self.config)
            {
                load_strategy = LoadStrategy::Growth;
                growth_resumed_ticks.push(tick);
            }
            self.update_loads(&mut active, load_strategy);
            let mut emitted = Vec::new();

            for thread in &mut active {
                if thread.phase == ThreadPhase::Tombstoned || thread.next_block_at_ms > tick_time_ms
                {
                    continue;
                }
                let is_tombstone = thread.phase == ThreadPhase::Collapsing;
                let mut refs = Vec::new();
                if let Some(parent) = thread.tip.or(thread.initial_parent.take()) {
                    refs.push(parent);
                }
                fill_cross_refs(
                    self.config.seed,
                    tick,
                    thread,
                    self.config.cross_ref_probability_percent,
                    self.config.max_cross_refs_per_block,
                    &mut refs,
                    &mut queued_refs_attempted,
                    &mut queued_refs_included,
                    &mut tombstone_refs_included,
                );
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
                    gen_utime_ms: thread.next_block_at_ms,
                    tick,
                    ordinal: self.next_block_ordinal,
                    load_basis_points: thread.load,
                    is_tombstone,
                });
                self.next_block_ordinal += 1;
                thread.tip = Some(block_id);
                thread.height += 1;
                emitted.push((thread.id, IncomingBlock {
                    block_id,
                    is_tombstone,
                }));
                if is_tombstone {
                    completed_lifetimes_ms
                        .push(thread.next_block_at_ms.saturating_sub(thread.created_at_ms));
                    thread.phase = ThreadPhase::Tombstoned;
                } else {
                    let interval_step = self.config.thread_block_interval_step_ms as i64;
                    let step = match load_strategy {
                        LoadStrategy::Growth => self.rng.range_i64(-interval_step, interval_step),
                        LoadStrategy::Cooling => self.rng.range_i64(0, interval_step),
                    };
                    thread.block_interval_ms =
                        thread.block_interval_ms.saturating_add_signed(step).clamp(
                            self.config.min_thread_block_interval_ms,
                            self.config.max_thread_block_interval_ms,
                        );
                    thread.next_block_at_ms += thread.block_interval_ms;
                }
            }

            broadcast_emitted(&mut active, &emitted);
            active.retain(ThreadState::is_live);

            if (tick + 1) % self.config.balance_every_ticks == 0 {
                self.rebalance(
                    tick + 1,
                    simulation_end_ms,
                    load_strategy,
                    &mut active,
                    &mut lifecycle,
                );
            }
        }

        let total_threads =
            usize::try_from(self.next_thread_serial).expect("generated thread count fits usize");
        let surviving_threads = active.iter().filter(|thread| thread.is_live()).count();
        let thread_counts_by_10_minutes = summarize_thread_counts_by_period(
            &active_thread_counts,
            &lifecycle,
            self.config.block_interval_ms,
            self.config.ticks * self.config.block_interval_ms,
        );
        GeneratedGraph {
            proof_seed: self.config.seed,
            blocks,
            lifecycle,
            active_thread_counts,
            total_threads,
            peak_threads,
            cooling_started_ticks,
            growth_resumed_ticks,
            queued_refs_attempted,
            queued_refs_included,
            tombstone_refs_included,
            thread_lifetime: summarize_thread_lifetimes(completed_lifetimes_ms, surviving_threads),
            thread_counts_by_10_minutes,
        }
    }

    fn update_loads(&mut self, active: &mut [ThreadState], strategy: LoadStrategy) {
        for thread in active {
            if thread.phase != ThreadPhase::Active {
                continue;
            }
            let demand_delta = match strategy {
                LoadStrategy::Growth => self.rng.range_i32(0, self.config.demand_step),
                LoadStrategy::Cooling => self.rng.range_i32(-self.config.demand_step, 0),
            };
            thread.demand = (thread.demand + demand_delta).clamp(0, LOAD_MAX);
            thread.load += (thread.demand - thread.load) / self.config.load_smoothing;
        }
    }

    fn rebalance(
        &mut self,
        tick: u64,
        simulation_end_ms: u64,
        strategy: LoadStrategy,
        active: &mut Vec<ThreadState>,
        lifecycle: &mut Vec<LifecycleEvent>,
    ) {
        let mut decisions = active
            .iter()
            .filter(|thread| {
                thread.phase == ThreadPhase::Active
                    && tick >= thread.cooldown_until
                    && thread.next_block_at_ms < simulation_end_ms
            })
            .filter_map(|thread| {
                if strategy == LoadStrategy::Growth && thread.load > self.config.split_load {
                    Some((
                        thread.load - self.config.split_load + LOAD_MAX,
                        thread.id,
                        true,
                    ))
                } else if strategy == LoadStrategy::Cooling
                    && !thread.id.is_zero()
                    && thread.load < self.config.collapse_load
                {
                    Some((self.config.collapse_load - thread.load, thread.id, false))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        decisions.sort_unstable_by(|a, b| b.cmp(a));

        let collapsing_threads = active
            .iter()
            .filter(|thread| thread.phase == ThreadPhase::Collapsing)
            .count();
        let mut projected_live_threads = active
            .iter()
            .filter(|thread| thread.is_live())
            .count()
            .saturating_sub(collapsing_threads);
        let cooling_floor = cooling_thread_floor(&self.config);

        for (_, thread_id, split) in decisions {
            let Some(index) = active.iter().position(|thread| thread.id == thread_id) else {
                continue;
            };
            if split {
                let live_threads = active.iter().filter(|thread| thread.is_live()).count();
                if live_threads.saturating_add(2) <= self.config.max_threads {
                    self.split_thread(tick, index, active, lifecycle);
                }
            } else if projected_live_threads > cooling_floor {
                self.collapse_thread(tick, index, active, lifecycle);
                projected_live_threads = projected_live_threads.saturating_sub(1);
            }
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
        let child_load = old.load / 2;
        let left = self.new_thread(old.tip, child_load, tick);
        let right = self.new_thread(old.tip, child_load, tick);
        let new_threads = vec![left.id, right.id];

        if old.id.is_zero() {
            let root = &mut active[index];
            root.load = child_load;
            root.demand = child_load;
            root.cooldown_until = tick + self.config.rebalance_cooldown_ticks;
            active.insert(index + 1, left);
            active.insert(index + 2, right);
        } else {
            active[index].phase = ThreadPhase::Collapsing;
            // Keep the source alive until it emits the tombstone that will be
            // broadcast into both children's inboxes.
            active.insert(index, left);
            active.insert(index + 2, right);
        }
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

    fn collapse_thread(
        &mut self,
        tick: u64,
        index: usize,
        active: &mut [ThreadState],
        lifecycle: &mut Vec<LifecycleEvent>,
    ) {
        let old = active[index].clone();
        active[index].phase = ThreadPhase::Collapsing;
        lifecycle.push(LifecycleEvent {
            tick,
            kind: LifecycleKind::Collapse,
            old_thread: old.id,
            new_threads: Vec::new(),
        });
    }

    fn new_thread(&mut self, inherited_tip: Option<BlockId>, load: i32, tick: u64) -> ThreadState {
        let serial = self.next_thread_serial;
        self.next_thread_serial += 1;
        let interval = initial_thread_interval_ms(&self.config, self.config.seed, serial);
        let created_at_ms = self.config.start_unix_ms + tick * self.config.block_interval_ms;
        ThreadState::new(
            deterministic_thread_id(self.config.seed, serial),
            serial,
            inherited_tip,
            load,
            ThreadTiming {
                cooldown_until_tick: tick + self.config.rebalance_cooldown_ticks,
                block_interval_ms: interval,
                next_block_at_ms: created_at_ms
                    + self.config.block_interval_ms
                    + thread_time_offset_ms(
                        self.config.seed,
                        serial,
                        self.config.block_interval_ms,
                    ),
                created_at_ms,
            },
        )
    }
}

fn cooling_thread_floor(config: &GeneratorConfig) -> usize {
    (config.max_threads * usize::from(config.cooling_floor_percent) / 100).max(1)
}

fn push_unique(values: &mut Vec<BlockId>, value: BlockId) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn summarize_thread_lifetimes(
    mut completed_ms: Vec<u64>,
    surviving_threads: usize,
) -> ThreadLifetimeStats {
    if completed_ms.is_empty() {
        return ThreadLifetimeStats {
            surviving_threads,
            ..ThreadLifetimeStats::default()
        };
    }
    completed_ms.sort_unstable();
    let count = completed_ms.len();
    let total: u128 = completed_ms.iter().map(|value| u128::from(*value)).sum();
    ThreadLifetimeStats {
        completed_threads: count,
        surviving_threads,
        min_ms: completed_ms[0],
        median_ms: completed_ms[count / 2],
        p95_ms: completed_ms[(count - 1) * 95 / 100],
        max_ms: completed_ms[count - 1],
        mean_ms: total as f64 / count as f64,
    }
}

fn summarize_thread_counts_by_period(
    active_counts: &[usize],
    lifecycle: &[LifecycleEvent],
    tick_interval_ms: u64,
    total_duration_ms: u64,
) -> Vec<ThreadCountPeriodStats> {
    if active_counts.is_empty() {
        return Vec::new();
    }
    #[derive(Clone, Copy)]
    struct Accumulator {
        samples: usize,
        created: usize,
        min: usize,
        max: usize,
        sum: u128,
    }

    let last_tick = u64::try_from(active_counts.len() - 1).expect("tick count fits u64");
    let period_count = usize::try_from(last_tick * tick_interval_ms / TEN_MINUTES_MS + 1)
        .expect("period count fits usize");
    let mut periods = vec![
        Accumulator {
            samples: 0,
            created: 0,
            min: usize::MAX,
            max: 0,
            sum: 0,
        };
        period_count
    ];
    periods[0].created = 1; // Permanent thread zero.

    for (tick, active) in active_counts.iter().copied().enumerate() {
        let offset = u64::try_from(tick).expect("tick index fits u64") * tick_interval_ms;
        let index = usize::try_from(offset / TEN_MINUTES_MS).expect("period index fits usize");
        let period = &mut periods[index];
        period.samples += 1;
        period.min = period.min.min(active);
        period.max = period.max.max(active);
        period.sum += active as u128;
    }
    for event in lifecycle {
        let offset = event.tick * tick_interval_ms;
        let index = usize::try_from(offset / TEN_MINUTES_MS).expect("period index fits usize");
        if let Some(period) = periods.get_mut(index) {
            period.created += event.new_threads.len();
        }
    }

    periods
        .into_iter()
        .enumerate()
        .map(|(index, period)| {
            let start_offset_ms =
                u64::try_from(index).expect("period index fits u64") * TEN_MINUTES_MS;
            ThreadCountPeriodStats {
                start_offset_ms,
                duration_ms: TEN_MINUTES_MS.min(total_duration_ms - start_offset_ms),
                sampled_ticks: period.samples,
                created_threads: period.created,
                min_active_threads: period.min,
                max_active_threads: period.max,
                mean_active_threads: period.sum as f64 / period.samples as f64,
            }
        })
        .collect()
}

fn broadcast_emitted(active: &mut [ThreadState], emitted: &[(ThreadId, IncomingBlock)]) {
    for (source_thread, block) in emitted {
        for recipient in active
            .iter_mut()
            .filter(|thread| thread.is_live() && thread.id != *source_thread)
        {
            recipient
                .inbox
                .entry(*source_thread)
                .or_default()
                .push_back(*block);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_cross_refs(
    seed: u64,
    tick: u64,
    thread: &mut ThreadState,
    probability_percent: u8,
    max_cross_refs: usize,
    refs: &mut Vec<BlockId>,
    attempted: &mut u64,
    included: &mut u64,
    tombstones_included: &mut u64,
) {
    let mut empty_groups = Vec::new();
    let mut tombstone_groups = Vec::new();
    let mut ordinary_groups = Vec::new();
    for (source_thread, queue) in &thread.inbox {
        match queue.back() {
            None => empty_groups.push(*source_thread),
            Some(latest) if latest.is_tombstone => tombstone_groups.push(*source_thread),
            Some(_) => ordinary_groups.push(*source_thread),
        }
    }

    // Lifecycle handoffs must not be lost to either fanout or probability.
    for source_thread in tombstone_groups {
        let queue = thread
            .inbox
            .get_mut(&source_thread)
            .expect("collected inbox group must still exist");
        let latest = queue.back().copied().expect("group was non-empty");
        push_unique(refs, latest.block_id);
        *tombstones_included += 1;
        queue.clear();
        empty_groups.push(source_thread);
    }

    let selected = max_cross_refs.min(ordinary_groups.len());
    let start = if ordinary_groups.is_empty() {
        0
    } else {
        thread.cross_ref_cursor % ordinary_groups.len()
    };
    thread.cross_ref_cursor = thread.cross_ref_cursor.wrapping_add(selected);
    for offset in 0..selected {
        let source_thread = ordinary_groups[(start + offset) % ordinary_groups.len()];
        *attempted += 1;
        let source_sample = u64::from_le_bytes(
            source_thread.as_bytes()[..8]
                .try_into()
                .expect("thread id prefix has eight bytes"),
        );
        let sample = splitmix64(
            seed ^ tick.rotate_left(13)
                ^ thread.serial.rotate_left(29)
                ^ source_sample.rotate_left(41),
        );
        if probability_percent == 0 || sample % 100 >= u64::from(probability_percent) {
            continue;
        }

        let queue = thread
            .inbox
            .get_mut(&source_thread)
            .expect("selected inbox group must still exist");
        let latest = queue.back().copied().expect("group was non-empty");
        push_unique(refs, latest.block_id);
        *included += 1;

        // Selecting the newest pending block makes every older item stale.
        // Keep it as the last known source state until a newer block arrives.
        queue.clear();
        queue.push_back(latest);
    }
    for source_thread in empty_groups {
        thread.inbox.remove(&source_thread);
    }
}

fn deterministic_thread_id(seed: u64, serial: u64) -> ThreadId {
    let mut bytes = [0u8; 34];
    fill_deterministic(&mut bytes, seed ^ 0x5448_5245_4144, serial, 0, 0);
    ThreadId::from_bytes(bytes)
}

fn thread_time_offset_ms(seed: u64, serial: u64, block_interval_ms: u64) -> u64 {
    splitmix64(seed ^ 0x5449_4d45 ^ serial.rotate_left(23)) % block_interval_ms
}

fn initial_thread_interval_ms(config: &GeneratorConfig, seed: u64, serial: u64) -> u64 {
    let width = config.max_thread_block_interval_ms - config.min_thread_block_interval_ms + 1;
    config.min_thread_block_interval_ms
        + splitmix64(seed ^ 0x494e_5445_5256_414c ^ serial.rotate_left(37)) % width
}

fn synthetic_proof_material(
    seed: u64,
    tick: u64,
    ordinal: u64,
) -> ([u8; 32], [u8; 32], [[u8; 32]; 16]) {
    let mut leaves = [[0u8; 32]; 16];
    for (index, leaf) in leaves.iter_mut().enumerate() {
        fill_deterministic(leaf, seed ^ 0x4d45_524b_4c45, tick, ordinal, index as u64);
    }
    let (envelope_hash, tracked_ext_out_messages_root) =
        synthetic_block_leaf_metadata(seed, tick, ordinal);
    leaves[8] = tracked_ext_out_messages_root;
    (envelope_hash, tracked_ext_out_messages_root, leaves)
}

fn synthetic_block_leaf_metadata(seed: u64, tick: u64, ordinal: u64) -> ([u8; 32], [u8; 32]) {
    let mut envelope_hash = [0u8; 32];
    fill_deterministic(
        &mut envelope_hash,
        seed ^ 0x454e_5645_4c4f_5045,
        tick,
        ordinal,
        0,
    );
    let mut tracked_ext_out_messages_root = [0u8; 32];
    fill_deterministic(
        &mut tracked_ext_out_messages_root,
        seed ^ 0x4d45_524b_4c45,
        tick,
        ordinal,
        8,
    );
    (envelope_hash, tracked_ext_out_messages_root)
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

    fn range_i64(&mut self, start: i64, end: i64) -> i64 {
        if start == end {
            return start;
        }
        let width = i128::from(end) - i128::from(start) + 1;
        let offset = self.next_u64() % u64::try_from(width).expect("positive range");
        i64::try_from(i128::from(start) + i128::from(offset)).expect("sample fits i64")
    }
}

pub struct SyntheticBlockProvider {
    proof_seed: u64,
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
    pub proof_block_by_id: u64,
    pub proof_block_by_height: u64,
}

impl ProviderCallCounts {
    pub fn total(self) -> u64 {
        self.block_by_id
            + self.latest_blocks
            + self.timed_block_by_id
            + self.timed_block_by_height
            + self.latest_timed_block_in_thread
            + self.proof_block_by_id
            + self.proof_block_by_height
    }

    pub fn since(self, earlier: Self) -> Self {
        Self {
            block_by_id: self.block_by_id - earlier.block_by_id,
            latest_blocks: self.latest_blocks - earlier.latest_blocks,
            timed_block_by_id: self.timed_block_by_id - earlier.timed_block_by_id,
            timed_block_by_height: self.timed_block_by_height - earlier.timed_block_by_height,
            latest_timed_block_in_thread: self.latest_timed_block_in_thread
                - earlier.latest_timed_block_in_thread,
            proof_block_by_id: self.proof_block_by_id - earlier.proof_block_by_id,
            proof_block_by_height: self.proof_block_by_height - earlier.proof_block_by_height,
        }
    }

    pub fn add(&mut self, other: Self) {
        self.block_by_id += other.block_by_id;
        self.latest_blocks += other.latest_blocks;
        self.timed_block_by_id += other.timed_block_by_id;
        self.timed_block_by_height += other.timed_block_by_height;
        self.latest_timed_block_in_thread += other.latest_timed_block_in_thread;
        self.proof_block_by_id += other.proof_block_by_id;
        self.proof_block_by_height += other.proof_block_by_height;
    }
}

#[derive(Default)]
struct ProviderCallCounters {
    block_by_id: AtomicU64,
    latest_blocks: AtomicU64,
    timed_block_by_id: AtomicU64,
    timed_block_by_height: AtomicU64,
    latest_timed_block_in_thread: AtomicU64,
    proof_block_by_id: AtomicU64,
    proof_block_by_height: AtomicU64,
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
            proof_seed: graph.proof_seed,
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
            proof_block_by_id: self.calls.proof_block_by_id.load(Ordering::Relaxed),
            proof_block_by_height: self.calls.proof_block_by_height.load(Ordering::Relaxed),
        }
    }

    fn timed(&self, index: usize) -> TimedBlock {
        TimedBlock {
            block: self.blocks[index].node.clone(),
            gen_utime_ms: self.blocks[index].gen_utime_ms,
        }
    }

    fn proof(&self, index: usize) -> ProofBlock {
        let generated = &self.blocks[index];
        let (envelope_hash, tracked_ext_out_messages_root, leaves) =
            synthetic_proof_material(self.proof_seed, generated.tick, generated.ordinal);
        ProofBlock {
            block: generated.node.clone(),
            gen_utime_ms: generated.gen_utime_ms,
            envelope_hash,
            tracked_ext_out_messages_root,
            history_proofs: BTreeMap::new(),
            block_merkle_tree_leaves: leaves,
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

    async fn proof_block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<ProofBlock>> {
        self.calls.proof_block_by_id.fetch_add(1, Ordering::Relaxed);
        Ok(self.by_id.get(id).map(|index| self.proof(*index)))
    }

    async fn proof_block_by_height(
        &self,
        thread: &ThreadId,
        height: u64,
    ) -> anyhow::Result<Option<ProofBlock>> {
        self.calls
            .proof_block_by_height
            .fetch_add(1, Ordering::Relaxed);
        Ok(self
            .by_thread_height
            .get(&(*thread, height))
            .map(|index| self.proof(*index)))
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
    use crate::{GraphResolver, MemoryStore, ResolutionPolicy, ResolutionRequest, ResolverLimits};

    fn evolving_config() -> GeneratorConfig {
        GeneratorConfig {
            ticks: SIX_HOURS_TICKS_330_MS,
            balance_every_ticks: 3,
            rebalance_cooldown_ticks: 6,
            split_load: 5_700,
            collapse_load: 1_200,
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
        let mean_active_threads = first.active_thread_counts.iter().sum::<usize>() as f64
            / first.active_thread_counts.len() as f64;
        let split_events = first
            .lifecycle
            .iter()
            .filter(|event| event.kind == LifecycleKind::Split)
            .count();
        let collapse_events = first
            .lifecycle
            .iter()
            .filter(|event| event.kind == LifecycleKind::Collapse)
            .count();
        let root_spawn_events = first
            .lifecycle
            .iter()
            .filter(|event| event.kind == LifecycleKind::RootSpawn)
            .count();
        println!(
            "generated ticks={} total_threads={} peak_threads={} mean_active_threads={:.2} \
             cooling_cycles={} growth_resumes={} blocks={} tombstones={} lifecycle_events={} \
             splits={} collapses={} root_spawns={} queued_refs={}/{} tombstone_refs={}",
            first.active_thread_counts.len(),
            first.total_threads,
            first.peak_threads,
            mean_active_threads,
            first.cooling_started_ticks.len(),
            first.growth_resumed_ticks.len(),
            first.blocks.len(),
            first
                .blocks
                .iter()
                .filter(|block| block.is_tombstone)
                .count(),
            first.lifecycle.len(),
            split_events,
            collapse_events,
            root_spawn_events,
            first.queued_refs_included,
            first.queued_refs_attempted,
            first.tombstone_refs_included,
        );
        println!(
            "thread_lifetime completed={} surviving={} min_ms={} median_ms={} p95_ms={} max_ms={} \
             mean_ms={:.2}",
            first.thread_lifetime.completed_threads,
            first.thread_lifetime.surviving_threads,
            first.thread_lifetime.min_ms,
            first.thread_lifetime.median_ms,
            first.thread_lifetime.p95_ms,
            first.thread_lifetime.max_ms,
            first.thread_lifetime.mean_ms,
        );
        for (index, period) in first.thread_counts_by_10_minutes.iter().enumerate() {
            println!(
                "thread_period index={} start_min={} created={} active_min={} active_mean={:.2} \
                 active_max={} sampled_ticks={}",
                index,
                period.start_offset_ms / 60_000,
                period.created_threads,
                period.min_active_threads,
                period.mean_active_threads,
                period.max_active_threads,
                period.sampled_ticks,
            );
        }
        assert_eq!(first.peak_threads, evolving_config().max_threads);
        assert!(
            mean_active_threads > 100.0,
            "six-hour mean must exceed 100 active threads, got {mean_active_threads:.2}"
        );
        assert_eq!(
            first.total_threads,
            1 + first
                .lifecycle
                .iter()
                .map(|event| event.new_threads.len())
                .sum::<usize>()
        );
        assert_eq!(
            first.thread_lifetime.completed_threads,
            first
                .blocks
                .iter()
                .filter(|block| block.is_tombstone)
                .count()
        );
        assert_eq!(
            first.thread_lifetime.completed_threads + first.thread_lifetime.surviving_threads,
            first.total_threads
        );
        assert!(first.thread_lifetime.min_ms > 0);
        assert!(first.thread_lifetime.min_ms <= first.thread_lifetime.median_ms);
        assert!(first.thread_lifetime.median_ms <= first.thread_lifetime.p95_ms);
        assert!(first.thread_lifetime.p95_ms <= first.thread_lifetime.max_ms);
        assert_eq!(first.thread_counts_by_10_minutes.len(), 36);
        assert_eq!(
            first
                .thread_counts_by_10_minutes
                .iter()
                .map(|period| period.created_threads)
                .sum::<usize>(),
            first.total_threads
        );
        assert_eq!(
            first
                .thread_counts_by_10_minutes
                .iter()
                .map(|period| period.sampled_ticks)
                .sum::<usize>(),
            evolving_config().ticks as usize
        );
        assert_eq!(
            first
                .thread_counts_by_10_minutes
                .iter()
                .map(|period| period.max_active_threads)
                .max(),
            Some(first.peak_threads)
        );
        let cooling_tick = first.cooling_started_ticks[0];
        assert_eq!(first.active_thread_counts[cooling_tick as usize], 200);
        assert!(
            first.active_thread_counts[cooling_tick as usize..]
                .iter()
                .any(|threads| *threads < 200),
            "cooling must eventually reduce the live-thread count"
        );
        assert!(
            first.cooling_started_ticks.len() > 1,
            "load must cycle through multiple cooling phases"
        );
        assert!(!first.growth_resumed_ticks.is_empty());
        for (cooling, resumed) in first
            .cooling_started_ticks
            .iter()
            .zip(&first.growth_resumed_ticks)
        {
            assert!(resumed >= &(cooling + evolving_config().minimum_cooling_ticks));
            assert_eq!(
                first.active_thread_counts[*resumed as usize],
                cooling_thread_floor(&evolving_config())
            );
        }
        assert!(
            first.thread_lifetime.surviving_threads >= cooling_thread_floor(&evolving_config())
        );
        assert!(first.thread_lifetime.surviving_threads <= evolving_config().max_threads);
        assert!(first.queued_refs_attempted > 0);
        assert!(first.tombstone_refs_included > 0);
        let observed_cross_ref_percent =
            first.queued_refs_included * 100 / first.queued_refs_attempted;
        assert!(
            (58..=62).contains(&observed_cross_ref_percent),
            "expected about 60% ordinary refs, got {observed_cross_ref_percent}%"
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
            .any(|event| event.kind == LifecycleKind::Collapse));

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
        let mut previous_by_thread = HashMap::new();
        let mut previous_time_by_thread = HashMap::new();
        let mut intervals_by_thread = HashMap::<ThreadId, HashSet<u64>>::new();
        let mut tombstones_by_thread = HashMap::<ThreadId, Vec<BlockId>>::new();
        let mut observed_loads = HashSet::new();
        let mut blocks_per_tick = vec![0usize; evolving_config().ticks as usize];
        for block in &first.blocks {
            blocks_per_tick[block.tick as usize] += 1;
            let tick_time =
                evolving_config().start_unix_ms + block.tick * evolving_config().block_interval_ms;
            assert!(
                block.gen_utime_ms <= tick_time
                    && block.gen_utime_ms + evolving_config().block_interval_ms >= tick_time,
                "a due block timestamp must fall in the current logical tick"
            );
            if let Some(previous_time) =
                previous_time_by_thread.insert(block.node.thread_id, block.gen_utime_ms)
            {
                let interval = block.gen_utime_ms - previous_time;
                intervals_by_thread
                    .entry(block.node.thread_id)
                    .or_default()
                    .insert(interval);
                assert!(
                    (evolving_config().min_thread_block_interval_ms
                        ..=evolving_config().max_thread_block_interval_ms)
                        .contains(&interval),
                    "thread interval {interval}ms is outside the configured range"
                );
            }
            observed_loads.insert(block.load_basis_points);
            if block.is_tombstone {
                tombstones_by_thread
                    .entry(block.node.thread_id)
                    .or_default()
                    .push(block.node.block_id);
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
        }
        assert!(
            blocks_per_tick
                .iter()
                .zip(&first.active_thread_counts)
                .all(|(blocks, active)| blocks <= active),
            "a thread may emit at most one block per tick"
        );
        assert!(
            blocks_per_tick
                .iter()
                .zip(&first.active_thread_counts)
                .any(|(blocks, active)| blocks < active),
            "variable intervals must make some threads skip ticks"
        );
        assert!(
            observed_loads.len() > 100,
            "load factor must drift over time"
        );
        assert!(
            intervals_by_thread
                .values()
                .any(|intervals| intervals.len() > 1),
            "a thread's production interval must drift over time"
        );

        for event in &first.lifecycle {
            if event.kind != LifecycleKind::RootSpawn {
                assert_eq!(
                    tombstones_by_thread.get(&event.old_thread).map(Vec::len),
                    Some(1),
                    "every collapsing thread must emit exactly one tombstone"
                );
            }
        }
        for (thread, tombstones) in &tombstones_by_thread {
            assert_eq!(tombstones.len(), 1);
            assert_eq!(previous_by_thread[thread], tombstones[0]);
        }

        let root_blocks = first
            .blocks
            .iter()
            .filter(|block| block.node.thread_id.is_zero())
            .count();
        assert!(root_blocks < evolving_config().ticks as usize);
        assert!(!tombstones_by_thread.contains_key(&ThreadId::ZERO));
    }

    #[test]
    fn small_graph_is_deterministic() {
        let config = GeneratorConfig {
            ticks: 1_000,
            max_threads: 20,
            balance_every_ticks: 3,
            rebalance_cooldown_ticks: 6,
            split_load: 5_700,
            collapse_load: 1_200,
            demand_step: 900,
            load_smoothing: 8,
            ..GeneratorConfig::default()
        };
        let first = TestGraphGenerator::new(config.clone()).unwrap().generate();
        let second = TestGraphGenerator::new(config).unwrap().generate();
        assert_eq!(first.blocks, second.blocks);
        assert_eq!(first.lifecycle, second.lifecycle);
        assert_eq!(first.active_thread_counts, second.active_thread_counts);
        assert_eq!(first.cooling_started_ticks, second.cooling_started_ticks);
        assert_eq!(first.growth_resumed_ticks, second.growth_resumed_ticks);
        assert_eq!(first.thread_lifetime, second.thread_lifetime);
        assert_eq!(
            first.thread_counts_by_10_minutes,
            second.thread_counts_by_10_minutes
        );
    }

    #[test]
    fn inbox_selects_latest_discards_older_and_consumes_tombstone() {
        let source = deterministic_thread_id(1, 10);
        let mut thread = ThreadState::new(
            deterministic_thread_id(1, 11),
            11,
            None,
            5_000,
            ThreadTiming {
                cooldown_until_tick: 0,
                block_interval_ms: 330,
                next_block_at_ms: 0,
                created_at_ms: 0,
            },
        );
        let first = IncomingBlock {
            block_id: BlockId::from_bytes([1; 32]),
            is_tombstone: false,
        };
        let second = IncomingBlock {
            block_id: BlockId::from_bytes([2; 32]),
            is_tombstone: false,
        };
        let tombstone = IncomingBlock {
            block_id: BlockId::from_bytes([3; 32]),
            is_tombstone: true,
        };
        thread.inbox.insert(source, VecDeque::from([first, second]));
        let mut attempted = 0;
        let mut included = 0;
        let mut tombstones = 0;

        let mut refs = Vec::new();
        fill_cross_refs(
            1,
            1,
            &mut thread,
            100,
            1,
            &mut refs,
            &mut attempted,
            &mut included,
            &mut tombstones,
        );
        assert_eq!(refs, vec![second.block_id]);
        assert_eq!(thread.inbox[&source], VecDeque::from([second]));

        thread.inbox.get_mut(&source).unwrap().push_back(tombstone);
        refs.clear();
        fill_cross_refs(
            1,
            2,
            &mut thread,
            100,
            1,
            &mut refs,
            &mut attempted,
            &mut included,
            &mut tombstones,
        );
        assert_eq!(refs, vec![tombstone.block_id]);
        assert!(!thread.inbox.contains_key(&source));
        assert_eq!((attempted, included, tombstones), (1, 1, 1));
    }

    #[test]
    fn emitted_block_is_broadcast_to_every_other_live_thread() {
        let make_thread = |serial| {
            ThreadState::new(
                deterministic_thread_id(1, serial),
                serial,
                None,
                5_000,
                ThreadTiming {
                    cooldown_until_tick: 0,
                    block_interval_ms: 330,
                    next_block_at_ms: 0,
                    created_at_ms: 0,
                },
            )
        };
        let mut active = vec![make_thread(1), make_thread(2), make_thread(3)];
        let source = active[0].id;
        let block = IncomingBlock {
            block_id: BlockId::from_bytes([5; 32]),
            is_tombstone: false,
        };
        broadcast_emitted(&mut active, &[(source, block)]);

        assert!(!active[0].inbox.contains_key(&source));
        assert_eq!(active[1].inbox[&source], VecDeque::from([block]));
        assert_eq!(active[2].inbox[&source], VecDeque::from([block]));
    }

    #[test]
    fn split_halves_load_and_starts_source_collapse() {
        let config = GeneratorConfig {
            ticks: 10,
            ..GeneratorConfig::default()
        };
        let mut generator = TestGraphGenerator::new(config).unwrap();
        let old_tip = BlockId::from_bytes([9; 32]);
        let mut source = ThreadState::new(
            deterministic_thread_id(generator.config.seed, 42),
            42,
            None,
            8_002,
            ThreadTiming {
                cooldown_until_tick: 0,
                block_interval_ms: 330,
                next_block_at_ms: generator.config.start_unix_ms,
                created_at_ms: generator.config.start_unix_ms,
            },
        );
        source.tip = Some(old_tip);
        let source_id = source.id;
        let mut active = vec![source];
        let mut lifecycle = Vec::new();

        generator.split_thread(1, 0, &mut active, &mut lifecycle);

        assert_eq!(active.len(), 3);
        assert_eq!(active[0].load, 4_001);
        assert_eq!(active[1].id, source_id);
        assert_eq!(active[1].phase, ThreadPhase::Collapsing);
        assert_eq!(active[2].load, 4_001);
        assert_eq!(active[0].initial_parent, Some(old_tip));
        assert_eq!(active[2].initial_parent, Some(old_tip));
        assert_eq!(lifecycle[0].kind, LifecycleKind::Split);
    }

    #[tokio::test]
    async fn resolver_reports_hop_statistics_for_1000_historical_targets() {
        let graph = TestGraphGenerator::new(evolving_config())
            .unwrap()
            .generate();
        let mut candidates_by_tick = BTreeMap::<u64, Vec<BlockId>>::new();
        for block in graph.blocks.iter().filter(|block| {
            !block.node.thread_id.is_zero() && block.tick + 5_000 < evolving_config().ticks
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

        let accessibility = graph.accessibility_map().unwrap();
        let reachable_targets = targets
            .iter()
            .filter(|(target, _)| accessibility.is_reachable(target))
            .count();
        let mut target_anchor_delays_ms = targets
            .iter()
            .filter_map(|(target, _)| accessibility.get(target).map(|entry| entry.delay_ms))
            .collect::<Vec<_>>();
        assert!(
            !target_anchor_delays_ms.is_empty(),
            "at least one sampled target must be reachable"
        );
        target_anchor_delays_ms.sort_unstable();
        let target_delay_p95 = (target_anchor_delays_ms.len() - 1) * 95 / 100;
        println!(
            "accessibility reachable={}/{} unreachable={} sampled_targets_reachable={}/{} \
             target_anchor_delay_ms_min={} median={} p95={} max={}",
            accessibility.reachable_blocks(),
            accessibility.total_blocks(),
            accessibility.unreachable_blocks(),
            reachable_targets,
            targets.len(),
            target_anchor_delays_ms[0],
            target_anchor_delays_ms[target_anchor_delays_ms.len() / 2],
            target_anchor_delays_ms[target_delay_p95],
            target_anchor_delays_ms[target_anchor_delays_ms.len() - 1],
        );
        drop(accessibility);

        {
            let provider = Arc::new(graph.provider());
            let resolver = GraphResolver::new(provider.clone(), Arc::new(MemoryStore::new()), 40);
            let started = std::time::Instant::now();
            let mut hops = Vec::with_capacity(1_000);
            let mut histogram = BTreeMap::<usize, usize>::new();
            let mut provider_calls = Vec::with_capacity(1_000);
            let mut provider_call_histogram = BTreeMap::<u64, usize>::new();
            let mut elapsed_micros = Vec::with_capacity(1_000);
            let mut successful_elapsed_micros = Vec::with_capacity(1_000);
            let mut aggregate_calls = ProviderCallCounts::default();
            let mut resolved_paths = Vec::with_capacity(1_000);
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
                        resolved_paths.push(path.clone());
                        successful_elapsed_micros.push(resolve_elapsed_micros);
                        println!(
                            "resolve sample={} tick={} status=ok hops={} provider_calls={} \
                             elapsed_us={resolve_elapsed_micros}",
                            sample + 1,
                            tick,
                            path.hops.len(),
                            call_delta.total(),
                        );
                    },
                    Err(error) => {
                        failures += 1;
                        println!(
                            "resolve sample={} tick={} status=error provider_calls={} \
                             elapsed_us={resolve_elapsed_micros} error={error:#}",
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
                "resolved_1000 successes={} failures={} histogram={histogram:?} hops_min={} \
                 hops_median={} hops_p95={} hops_max={} hops_mean={:.2}",
                hops.len(),
                failures,
                hops[0],
                hops[hops.len() / 2],
                hops[hop_p95],
                hops[hops.len() - 1],
                total_hops as f64 / hops.len() as f64,
            );
            println!(
                "provider_calls histogram={provider_call_histogram:?} min={} median={} p95={} \
                 max={} mean={:.2} local_zero={} aggregate={aggregate_calls:?}",
                provider_calls[0],
                provider_calls[499],
                provider_calls[949],
                provider_calls[999],
                total_provider_calls as f64 / provider_calls.len() as f64,
                provider_calls.iter().filter(|calls| **calls == 0).count(),
            );
            println!(
                "timing wall_ms={} all_mean_us={:.2} all_median_us={} all_p95_us={} all_max_us={} \
                 success_mean_us={:.2} success_median_us={} success_p95_us={} success_max_us={}",
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

            let proof_started = std::time::Instant::now();
            let mut proof_hops = Vec::with_capacity(resolved_paths.len());
            let mut proof_calls = Vec::with_capacity(resolved_paths.len());
            let mut parent_edges = 0usize;
            let mut cross_edges = 0usize;
            let mut proof_failures = 0usize;
            for (sample, path) in resolved_paths.iter().enumerate() {
                let target = path.target;
                let before = provider.call_counts();
                let result = resolver
                    .resolve_proof(ResolutionRequest {
                        target,
                        policy: ResolutionPolicy::FirstValid,
                        limits: ResolverLimits {
                            max_hops: 500,
                            max_visited_blocks: 10_000,
                        },
                    })
                    .await;
                let calls = provider.call_counts().since(before).total();
                match result {
                    Ok(proof) => {
                        assert_eq!(proof.path.target, target);
                        for edge in &proof.path.hops {
                            if edge.ref_index == 0 {
                                parent_edges += 1;
                            } else {
                                cross_edges += 1;
                            }
                        }
                        proof_hops.push(proof.path.hops.len());
                        proof_calls.push(calls);
                    },
                    Err(error) => {
                        proof_failures += 1;
                        println!(
                            "proof sample={} status=error provider_calls={} error={error:#}",
                            sample + 1,
                            calls,
                        );
                    },
                }
            }
            proof_hops.sort_unstable();
            proof_calls.sort_unstable();
            println!(
                "proof_1000 successes={} failures={} parent_edges={} cross_edges={} hops_min={} \
                 median={} p95={} max={} calls_min={} median={} p95={} max={} time_wall_ms={}",
                proof_hops.len(),
                proof_failures,
                parent_edges,
                cross_edges,
                proof_hops[0],
                proof_hops[proof_hops.len() / 2],
                proof_hops[949],
                proof_hops[999],
                proof_calls[0],
                proof_calls[proof_calls.len() / 2],
                proof_calls[949],
                proof_calls[999],
                proof_started.elapsed().as_millis(),
            );
            assert_eq!(proof_failures, 0);
            assert_eq!(proof_hops.len(), 1_000);
        }
    }
}
