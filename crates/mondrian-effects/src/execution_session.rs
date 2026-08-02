//! Instance-owned Effect pixel/topology/GPU-plan residency and generation
//! lifetime.
//!
//! Prepared Programs remain immutable. This Module owns the mutable reuse state
//! that Preview and Export must keep separate.

use crate::execution::{EffectFloatOutputCacheKey, EffectNodeOutputCacheKey, EffectOutputCacheKey};
use crate::gpu_plan::{CompiledEffectGpuPlan, EffectGpuPlanBlocker, EffectGpuPlanCache};
use crate::graph::{CompiledEffectGraph, EffectRenderGraph, PreparedEffectGraphTopology};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct EffectTemporalCachedOutput {
    pub(crate) pixels: Arc<Vec<[f32; 4]>>,
    pub(crate) frame_seed: i64,
}

/// Bounded residency and transient-allocation policy for one Effect execution
/// owner.
///
/// The main cache budget is partitioned internally across encoded output,
/// encoded-node, float output, temporal-output, and dynamic graph-topology
/// residency. The partitions sum to the exact caller budget, so one Session
/// can never silently multiply the configured byte cap. GPU plans use their
/// own explicit small entry/byte grant because their retained-size model is
/// unrelated to pixels and backend-neutral topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectExecutionSessionConfig {
    /// Maximum total number of cached frames/nodes/topologies retained by the
    /// Session.
    pub max_cache_entries: usize,
    /// Maximum aggregate pixel/topology logical bytes retained by the Session.
    pub max_cache_bytes: usize,
    /// Maximum active bytes admitted by one temporal/ROI scalar execution,
    /// including retained source coverage and final output residency.
    pub max_working_bytes: usize,
    /// Maximum GPU lowering plans or deterministic blockers retained by the
    /// Session.
    pub max_gpu_plan_entries: usize,
    /// Maximum conservatively estimated bytes retained by GPU planning.
    pub max_gpu_plan_bytes: usize,
}

impl EffectExecutionSessionConfig {
    /// Disable cross-call cache residency while retaining a bounded scalar
    /// working-set admission.
    pub const fn uncached(max_working_bytes: usize) -> Self {
        Self {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        }
    }
}

impl Default for EffectExecutionSessionConfig {
    fn default() -> Self {
        Self {
            max_cache_entries: 32,
            max_cache_bytes: 96 * 1024 * 1024,
            max_working_bytes: 384 * 1024 * 1024,
            max_gpu_plan_entries: 64,
            max_gpu_plan_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Diagnostics for one instance-owned Effect execution/planning cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectExecutionSessionDiagnostics {
    /// Execution/planning generation currently bound to this owner, when any.
    pub generation: Option<u64>,
    /// Total resident cache entries across pixel representations and dynamic
    /// topologies.
    pub cache_entries: usize,
    /// Total logical cache bytes across pixel representations and dynamic
    /// topologies.
    pub cache_bytes: usize,
    /// Configured aggregate cache entry budget.
    pub max_cache_entries: usize,
    /// Configured aggregate cache byte budget.
    pub max_cache_bytes: usize,
    /// Configured temporal/ROI active-working-set byte budget.
    pub max_working_bytes: usize,
    /// Resident dynamic graph topologies.
    pub topology_entries: usize,
    /// Conservative logical bytes retained by dynamic graph topologies.
    pub topology_bytes: usize,
    /// Effective topology entry partition of the aggregate cache grant.
    pub max_topology_entries: usize,
    /// Effective topology byte partition of the aggregate cache grant.
    pub max_topology_bytes: usize,
    /// Resident GPU plans and deterministic lowering blockers.
    pub gpu_plan_entries: usize,
    /// Conservative bytes retained by GPU planning.
    pub gpu_plan_bytes: usize,
    /// Configured GPU-plan entry budget.
    pub max_gpu_plan_entries: usize,
    /// Configured GPU-plan byte budget.
    pub max_gpu_plan_bytes: usize,
}

/// Exclusive, instance-owned Effect execution state.
///
/// Preview and Export own separate Sessions. Reconfiguring or retiring one
/// consumer therefore cannot evict another consumer's pixels, dynamic
/// topologies, or GPU plans, and no process-global Effect cache survives
/// project/session rotation.
#[derive(Debug)]
pub struct EffectExecutionSession {
    config: EffectExecutionSessionConfig,
    generation: Option<u64>,
    encoded_outputs: BoundedLru<EffectOutputCacheKey, Vec<u8>>,
    encoded_nodes: BoundedLru<EffectNodeOutputCacheKey, Vec<u8>>,
    float_outputs: BoundedLru<EffectFloatOutputCacheKey, Vec<[f32; 4]>>,
    temporal_outputs: BoundedLru<[u8; 32], EffectTemporalCachedOutput>,
    topologies: EffectTopologyCache,
    gpu_plans: EffectGpuPlanCache,
}

impl Default for EffectExecutionSession {
    fn default() -> Self {
        Self::new(EffectExecutionSessionConfig::default())
    }
}

impl EffectExecutionSession {
    /// Create one exclusive execution Session.
    pub fn new(config: EffectExecutionSessionConfig) -> Self {
        let partitions = EffectCachePartitions::from_config(config);
        Self {
            config,
            generation: None,
            encoded_outputs: BoundedLru::new(
                partitions.encoded_output_entries,
                partitions.encoded_output_bytes,
                Vec::len,
            ),
            encoded_nodes: BoundedLru::new(
                partitions.encoded_node_entries,
                partitions.encoded_node_bytes,
                Vec::len,
            ),
            float_outputs: BoundedLru::new(
                partitions.float_output_entries,
                partitions.float_output_bytes,
                |frame| frame.len().saturating_mul(std::mem::size_of::<[f32; 4]>()),
            ),
            temporal_outputs: BoundedLru::new(
                partitions.temporal_output_entries,
                partitions.temporal_output_bytes,
                |frame| frame.pixels.len().saturating_mul(std::mem::size_of::<[f32; 4]>()),
            ),
            topologies: EffectTopologyCache::new(
                partitions.topology_entries,
                partitions.topology_bytes,
            ),
            gpu_plans: EffectGpuPlanCache::new(
                config.max_gpu_plan_entries,
                config.max_gpu_plan_bytes,
            ),
        }
    }

    /// Apply a new product resource decision and trim synchronously.
    ///
    /// The working-set limit affects the next execution only; already-returned
    /// frames are caller-owned and cannot be revoked.
    pub fn reconfigure(&mut self, config: EffectExecutionSessionConfig) {
        self.config = config;
        let partitions = EffectCachePartitions::from_config(config);
        self.encoded_outputs.reconfigure(
            partitions.encoded_output_entries,
            partitions.encoded_output_bytes,
        );
        self.encoded_nodes.reconfigure(
            partitions.encoded_node_entries,
            partitions.encoded_node_bytes,
        );
        self.float_outputs.reconfigure(
            partitions.float_output_entries,
            partitions.float_output_bytes,
        );
        self.temporal_outputs.reconfigure(
            partitions.temporal_output_entries,
            partitions.temporal_output_bytes,
        );
        self.topologies
            .reconfigure(partitions.topology_entries, partitions.topology_bytes);
        self.gpu_plans
            .reconfigure(config.max_gpu_plan_entries, config.max_gpu_plan_bytes);
    }

    /// Bind a new execution generation.
    ///
    /// Generation changes are conservative cache barriers. A canceled or
    /// superseded generation can therefore never publish into the next one.
    pub fn bind_generation(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.clear();
            self.generation = Some(generation);
        }
    }

    /// Remove every resident cache entry while retaining configuration.
    pub fn clear(&mut self) {
        self.encoded_outputs.clear();
        self.encoded_nodes.clear();
        self.float_outputs.clear();
        self.temporal_outputs.clear();
        self.topologies.clear();
        self.gpu_plans.clear();
    }

    /// Current bounded-residency diagnostics.
    pub fn diagnostics(&self) -> EffectExecutionSessionDiagnostics {
        EffectExecutionSessionDiagnostics {
            generation: self.generation,
            cache_entries: self
                .encoded_outputs
                .entries()
                .saturating_add(self.encoded_nodes.entries())
                .saturating_add(self.float_outputs.entries())
                .saturating_add(self.temporal_outputs.entries())
                .saturating_add(self.topologies.entries()),
            cache_bytes: self
                .encoded_outputs
                .bytes()
                .saturating_add(self.encoded_nodes.bytes())
                .saturating_add(self.float_outputs.bytes())
                .saturating_add(self.temporal_outputs.bytes())
                .saturating_add(self.topologies.bytes()),
            max_cache_entries: self.config.max_cache_entries,
            max_cache_bytes: self.config.max_cache_bytes,
            max_working_bytes: self.config.max_working_bytes,
            topology_entries: self.topologies.entries(),
            topology_bytes: self.topologies.bytes(),
            max_topology_entries: self.topologies.max_entries(),
            max_topology_bytes: self.topologies.max_bytes(),
            gpu_plan_entries: self.gpu_plans.entries(),
            gpu_plan_bytes: self.gpu_plans.bytes(),
            max_gpu_plan_entries: self.config.max_gpu_plan_entries,
            max_gpu_plan_bytes: self.config.max_gpu_plan_bytes,
        }
    }

    /// Return a GPU lowering plan owned by this exact execution Session.
    ///
    /// Both successful plans and deterministic blockers use the Session's
    /// entry/byte budget and generation barrier.
    pub fn get_or_lower_gpu_plan(
        &mut self,
        compiled: &CompiledEffectGraph,
    ) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker> {
        self.gpu_plans.get_or_lower(compiled)
    }

    pub(crate) fn get_effect_topology(
        &mut self,
        graph: &EffectRenderGraph,
    ) -> Option<Arc<PreparedEffectGraphTopology>> {
        self.topologies.get(graph)
    }

    pub(crate) fn retain_effect_topology(&mut self, topology: Arc<PreparedEffectGraphTopology>) {
        self.topologies.insert(topology);
    }

    pub(crate) const fn max_working_bytes(&self) -> usize {
        self.config.max_working_bytes
    }

    pub(crate) fn get_encoded_output(&mut self, key: &EffectOutputCacheKey) -> Option<Vec<u8>> {
        self.encoded_outputs.get(key)
    }

    pub(crate) fn put_encoded_output(&mut self, key: EffectOutputCacheKey, output: Vec<u8>) {
        self.encoded_outputs.insert(key, output);
    }

    pub(crate) fn get_encoded_node(&mut self, key: &EffectNodeOutputCacheKey) -> Option<Vec<u8>> {
        self.encoded_nodes.get(key)
    }

    pub(crate) fn put_encoded_node(&mut self, key: EffectNodeOutputCacheKey, output: Vec<u8>) {
        self.encoded_nodes.insert(key, output);
    }

    pub(crate) fn get_float_output(
        &mut self,
        key: &EffectFloatOutputCacheKey,
    ) -> Option<Vec<[f32; 4]>> {
        self.float_outputs.get(key)
    }

    pub(crate) fn put_float_output(
        &mut self,
        key: EffectFloatOutputCacheKey,
        output: Vec<[f32; 4]>,
    ) {
        self.float_outputs.insert(key, output);
    }

    pub(crate) fn get_temporal_output(
        &mut self,
        key: &[u8; 32],
    ) -> Option<EffectTemporalCachedOutput> {
        self.temporal_outputs.get(key)
    }

    pub(crate) fn put_temporal_output(
        &mut self,
        key: [u8; 32],
        output: EffectTemporalCachedOutput,
    ) {
        self.temporal_outputs.insert(key, output);
    }
}

#[derive(Debug)]
struct EffectTopologyCache {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: VecDeque<Arc<PreparedEffectGraphTopology>>,
}

impl EffectTopologyCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            total_bytes: 0,
            entries: VecDeque::new(),
        }
    }

    fn get(&mut self, graph: &EffectRenderGraph) -> Option<Arc<PreparedEffectGraphTopology>> {
        if let Some(index) = self.entries.iter().position(|topology| topology.matches(graph)) {
            let topology = self.entries.remove(index)?;
            self.entries.push_back(Arc::clone(&topology));
            return Some(topology);
        }
        None
    }

    fn insert(&mut self, topology: Arc<PreparedEffectGraphTopology>) {
        let retained_bytes = topology_cache_entry_retained_bytes(&topology);
        if self.max_entries == 0 || retained_bytes > self.max_bytes || self.max_bytes == 0 {
            return;
        }
        self.total_bytes = self.total_bytes.saturating_add(retained_bytes);
        self.entries.push_back(topology);
        self.trim();
    }

    fn reconfigure(&mut self, max_entries: usize, max_bytes: usize) {
        self.max_entries = max_entries;
        self.max_bytes = max_bytes;
        self.trim();
    }

    fn clear(&mut self) {
        self.total_bytes = 0;
        self.entries.clear();
    }

    fn entries(&self) -> usize {
        self.entries.len()
    }

    const fn bytes(&self) -> usize {
        self.total_bytes
    }

    const fn max_entries(&self) -> usize {
        self.max_entries
    }

    const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                self.total_bytes = 0;
                break;
            };
            self.total_bytes =
                self.total_bytes.saturating_sub(topology_cache_entry_retained_bytes(&evicted));
        }
    }
}

fn topology_cache_entry_retained_bytes(topology: &PreparedEffectGraphTopology) -> usize {
    std::mem::size_of::<Arc<PreparedEffectGraphTopology>>()
        .saturating_add(topology.retained_bytes_estimate())
        .saturating_add(32)
}

#[derive(Debug)]
struct BoundedLru<K, V> {
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    entries: HashMap<K, V>,
    order: VecDeque<K>,
    size_of: fn(&V) -> usize,
}

impl<K, V> BoundedLru<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    fn new(max_entries: usize, max_bytes: usize, size_of: fn(&V) -> usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            total_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
            size_of,
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        let value = self.entries.get(key).cloned()?;
        self.remove_from_order(key);
        self.order.push_back(key.clone());
        Some(value)
    }

    fn insert(&mut self, key: K, value: V) {
        let size = (self.size_of)(&value);
        if size > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key.clone(), value) {
            self.total_bytes = self.total_bytes.saturating_sub((self.size_of)(&previous));
            self.remove_from_order(&key);
        }
        self.total_bytes = self.total_bytes.saturating_add(size);
        self.order.push_back(key);
        self.trim();
    }

    fn reconfigure(&mut self, max_entries: usize, max_bytes: usize) {
        self.max_entries = max_entries;
        self.max_bytes = max_bytes;
        self.trim();
    }

    fn clear(&mut self) {
        self.total_bytes = 0;
        self.entries.clear();
        self.order.clear();
    }

    fn entries(&self) -> usize {
        self.entries.len()
    }

    const fn bytes(&self) -> usize {
        self.total_bytes
    }

    fn remove_from_order(&mut self, key: &K) {
        if let Some(index) = self.order.iter().position(|existing| existing == key) {
            self.order.remove(index);
        }
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(evicted_key) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&evicted_key) {
                self.total_bytes = self.total_bytes.saturating_sub((self.size_of)(&evicted));
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct EffectCachePartitions {
    encoded_output_entries: usize,
    encoded_output_bytes: usize,
    encoded_node_entries: usize,
    encoded_node_bytes: usize,
    float_output_entries: usize,
    float_output_bytes: usize,
    temporal_output_entries: usize,
    temporal_output_bytes: usize,
    topology_entries: usize,
    topology_bytes: usize,
}

impl EffectCachePartitions {
    fn from_config(config: EffectExecutionSessionConfig) -> Self {
        let entries = partition_five_evenly(config.max_cache_entries);
        let bytes = partition_effect_cache_bytes(config.max_cache_bytes);
        Self {
            encoded_output_entries: entries[0],
            encoded_output_bytes: bytes[0],
            encoded_node_entries: entries[1],
            encoded_node_bytes: bytes[1],
            float_output_entries: entries[2],
            float_output_bytes: bytes[2],
            temporal_output_entries: entries[3],
            temporal_output_bytes: bytes[3],
            topology_entries: entries[4],
            topology_bytes: bytes[4],
        }
    }
}

fn partition_five_evenly(total: usize) -> [usize; 5] {
    let share = total / 5;
    let remainder = total % 5;
    std::array::from_fn(|index| share + usize::from(index < remainder))
}

fn partition_four_evenly(total: usize) -> [usize; 4] {
    let share = total / 4;
    let remainder = total % 4;
    std::array::from_fn(|index| share + usize::from(index < remainder))
}

fn partition_effect_cache_bytes(total: usize) -> [usize; 5] {
    let topology = total.div_ceil(32);
    let pixels = partition_four_evenly(total.saturating_sub(topology));
    [pixels[0], pixels[1], pixels[2], pixels[3], topology]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_never_multiply_the_declared_budget() {
        for total in 0..20 {
            assert_eq!(
                partition_five_evenly(total).into_iter().sum::<usize>(),
                total
            );
            assert_eq!(
                partition_effect_cache_bytes(total).into_iter().sum::<usize>(),
                total
            );
        }
    }

    #[test]
    fn generation_rotation_and_reconfigure_trim_synchronously() {
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_cache_entries: 4,
            max_cache_bytes: 160,
            max_working_bytes: 128,
            max_gpu_plan_entries: 4,
            max_gpu_plan_bytes: 4 * 1024,
        });
        session.put_temporal_output(
            [1; 32],
            EffectTemporalCachedOutput { pixels: Arc::new(vec![[0.0; 4]; 2]), frame_seed: 0 },
        );
        assert_eq!(session.diagnostics().cache_entries, 1);
        session.bind_generation(1);
        assert_eq!(session.diagnostics().cache_entries, 0);
        session.put_temporal_output(
            [2; 32],
            EffectTemporalCachedOutput { pixels: Arc::new(vec![[0.0; 4]; 2]), frame_seed: 0 },
        );
        session.reconfigure(EffectExecutionSessionConfig {
            max_cache_entries: 0,
            max_cache_bytes: 0,
            max_working_bytes: 128,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        });
        assert_eq!(session.diagnostics().cache_entries, 0);
        assert_eq!(session.diagnostics().cache_bytes, 0);
    }
}
