//! Shared execution driver for one prepared visual frame closure.
//!
//! [`crate::PreparedVisualFrameClosure`] freezes recursive Timeline semantics,
//! but consumers must still materialize its nodes. This Module owns the one
//! deterministic child-before-parent walk and proves that every Adapter sees
//! only outputs belonging to the exact immutable closure. Preview and Export
//! provide pixels and product-specific failure behavior through
//! [`PreparedVisualExecutionAdapter`]; they do not implement another nested
//! traversal.

use crate::{
    PreparedVisualFrameClosure, PreparedVisualFrameNode, PreparedVisualFrameNodeId,
    PreparedVisualNestedBinding, PreparedVisualNestedSample,
};
use mondrian_core::timeline_data::TimelineClipExecutionRef;
use std::fmt;

/// Consumer Adapter invoked exactly once for every reachable prepared node.
///
/// Calls are deterministic and child-before-parent. `inputs` exposes only
/// already-materialized direct children and the exact inbound binding. The
/// Adapter remains responsible for decode, generated pixels, compositing,
/// Preview pending state, presentation, encoding, and publication.
pub trait PreparedVisualExecutionAdapter<T> {
    /// Materialized value retained for a parent or returned for the root.
    type Output;
    /// Consumer-owned execution failure.
    type Error;

    /// Materialize one prepared Sequence occurrence.
    fn materialize_node(
        &mut self,
        inputs: PreparedVisualExecutionNodeInputs<'_, T, Self::Output>,
    ) -> Result<Self::Output, Self::Error>;
}

/// Immutable inputs for one Adapter call.
pub struct PreparedVisualExecutionNodeInputs<'a, T, O> {
    closure: &'a PreparedVisualFrameClosure<T>,
    node: &'a PreparedVisualFrameNode<T>,
    inbound: Option<&'a PreparedVisualNestedBinding>,
    outputs: &'a [Option<O>],
}

impl<'a, T, O> PreparedVisualExecutionNodeInputs<'a, T, O> {
    /// Owning immutable recursive closure.
    pub const fn closure(&self) -> &'a PreparedVisualFrameClosure<T> {
        self.closure
    }

    /// Exact node being materialized.
    pub const fn node(&self) -> &'a PreparedVisualFrameNode<T> {
        self.node
    }

    /// Whether this node is the closure root.
    pub fn is_root(&self) -> bool {
        self.node.id() == self.closure.root()
    }

    /// Exact parent binding, absent only for the root.
    pub const fn inbound_binding(&self) -> Option<&'a PreparedVisualNestedBinding> {
        self.inbound
    }

    /// Already-materialized output for one exact direct child.
    pub fn child_output(&self, child: PreparedVisualFrameNodeId) -> Option<&O> {
        if !self.node.bindings().iter().any(|binding| binding.child() == child) {
            return None;
        }
        self.outputs.get(child.index()).and_then(Option::as_ref)
    }

    /// Resolve and read one current or temporal nested child in a single
    /// closure-authoritative operation.
    pub fn nested_output(
        &self,
        placement: TimelineClipExecutionRef,
        sample: PreparedVisualNestedSample,
    ) -> Option<&O> {
        self.node
            .nested_child(placement, sample)
            .and_then(|child| self.child_output(child))
    }
}

/// Execute every reachable node through one consumer Adapter.
///
/// The driver validates the closure as a rooted tree, derives one stable
/// post-order without recursion, and retains child outputs until their parent
/// has run. This matches the closure's conservative active-byte admission and
/// avoids consumer-owned recursive materialization stacks.
pub fn execute_prepared_visual_closure<T, A>(
    closure: &PreparedVisualFrameClosure<T>,
    adapter: &mut A,
) -> Result<A::Output, PreparedVisualExecutionError<A::Error>>
where
    A: PreparedVisualExecutionAdapter<T>,
{
    let schedule = PreparedVisualExecutionSchedule::prepare(closure)
        .map_err(PreparedVisualExecutionError::Structure)?;
    let mut outputs = std::iter::repeat_with(|| None)
        .take(closure.len())
        .collect::<Vec<Option<A::Output>>>();

    for entry in &schedule.entries {
        let node = closure.node(entry.node).ok_or_else(|| {
            PreparedVisualExecutionError::Structure(
                PreparedVisualExecutionStructureError::MissingNode { node: entry.node },
            )
        })?;
        for binding in node.bindings() {
            if outputs.get(binding.child().index()).and_then(Option::as_ref).is_none() {
                return Err(PreparedVisualExecutionError::Structure(
                    PreparedVisualExecutionStructureError::ChildOutputUnavailable {
                        parent: node.id(),
                        child: binding.child(),
                    },
                ));
            }
        }
        let inbound = match entry.inbound {
            Some(inbound) => {
                let binding = closure
                    .node(inbound.parent)
                    .and_then(|parent| parent.bindings().get(inbound.binding_index))
                    .filter(|binding| binding.child() == entry.node)
                    .ok_or_else(|| {
                        PreparedVisualExecutionError::Structure(
                            PreparedVisualExecutionStructureError::MissingInboundBinding {
                                parent: inbound.parent,
                                child: entry.node,
                            },
                        )
                    })?;
                Some(binding)
            }
            None => None,
        };
        let output = adapter
            .materialize_node(PreparedVisualExecutionNodeInputs {
                closure,
                node,
                inbound,
                outputs: &outputs,
            })
            .map_err(PreparedVisualExecutionError::Adapter)?;
        outputs[entry.node.index()] = Some(output);
    }

    outputs.get_mut(closure.root().index()).and_then(Option::take).ok_or_else(|| {
        PreparedVisualExecutionError::Structure(
            PreparedVisualExecutionStructureError::RootOutputUnavailable { root: closure.root() },
        )
    })
}

#[derive(Debug, Clone, Copy)]
struct PreparedVisualExecutionInbound {
    parent: PreparedVisualFrameNodeId,
    binding_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct PreparedVisualExecutionScheduleEntry {
    node: PreparedVisualFrameNodeId,
    inbound: Option<PreparedVisualExecutionInbound>,
}

struct PreparedVisualExecutionSchedule {
    entries: Vec<PreparedVisualExecutionScheduleEntry>,
}

impl PreparedVisualExecutionSchedule {
    fn prepare<T>(
        closure: &PreparedVisualFrameClosure<T>,
    ) -> Result<Self, PreparedVisualExecutionStructureError> {
        if closure.is_empty() {
            return Err(PreparedVisualExecutionStructureError::EmptyClosure);
        }
        let root = closure.root();
        if closure.node(root).is_none() {
            return Err(PreparedVisualExecutionStructureError::MissingNode { node: root });
        }

        let mut inbound = vec![None; closure.len()];
        let mut state = vec![0_u8; closure.len()];
        let mut entries = Vec::with_capacity(closure.len());
        let mut stack = vec![(root, false)];
        while let Some((node_id, exiting)) = stack.pop() {
            let Some(node) = closure.node(node_id) else {
                return Err(PreparedVisualExecutionStructureError::MissingNode { node: node_id });
            };
            if exiting {
                state[node_id.index()] = 2;
                entries.push(PreparedVisualExecutionScheduleEntry {
                    node: node_id,
                    inbound: inbound[node_id.index()],
                });
                continue;
            }
            match state[node_id.index()] {
                1 => {
                    return Err(PreparedVisualExecutionStructureError::Cycle { node: node_id });
                }
                2 => continue,
                _ => {}
            }
            state[node_id.index()] = 1;
            stack.push((node_id, true));
            for (binding_index, binding) in node.bindings().iter().enumerate().rev() {
                if binding.parent() != node_id {
                    return Err(
                        PreparedVisualExecutionStructureError::BindingParentMismatch {
                            owner: node_id,
                            declared: binding.parent(),
                        },
                    );
                }
                let child = binding.child();
                if closure.node(child).is_none() {
                    return Err(PreparedVisualExecutionStructureError::MissingNode { node: child });
                }
                let slot = &mut inbound[child.index()];
                if slot
                    .replace(PreparedVisualExecutionInbound { parent: node_id, binding_index })
                    .is_some()
                {
                    return Err(PreparedVisualExecutionStructureError::MultipleParents { child });
                }
                stack.push((child, false));
            }
        }
        if let Some(index) = state.iter().position(|state| *state != 2) {
            let node = PreparedVisualFrameNodeId::from_index_for_execution(index)?;
            return Err(PreparedVisualExecutionStructureError::UnreachableNode { node });
        }
        Ok(Self { entries })
    }
}

impl PreparedVisualFrameNodeId {
    fn from_index_for_execution(
        index: usize,
    ) -> Result<Self, PreparedVisualExecutionStructureError> {
        let index = u32::try_from(index)
            .map_err(|_| PreparedVisualExecutionStructureError::NodeIndexOverflow { index })?;
        Ok(Self::from_raw_for_execution(index))
    }
}

/// Structural failure detected before or during the shared execution walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedVisualExecutionStructureError {
    /// A valid prepared closure must contain one root.
    EmptyClosure,
    /// A node ID did not resolve inside its closure.
    MissingNode {
        /// Missing closure-local node.
        node: PreparedVisualFrameNodeId,
    },
    /// A binding was stored under a different parent.
    BindingParentMismatch {
        /// Node whose binding collection owns the edge.
        owner: PreparedVisualFrameNodeId,
        /// Parent identity declared by the edge.
        declared: PreparedVisualFrameNodeId,
    },
    /// Prepared closures are execution-instance trees, not shared DAGs.
    MultipleParents {
        /// Child reached from more than one parent edge.
        child: PreparedVisualFrameNodeId,
    },
    /// A nested binding formed a cycle.
    Cycle {
        /// Node that re-entered the active traversal path.
        node: PreparedVisualFrameNodeId,
    },
    /// A node was not reachable from the declared root.
    UnreachableNode {
        /// Detached closure-local node.
        node: PreparedVisualFrameNodeId,
    },
    /// Platform node index could not fit the typed closure identity.
    NodeIndexOverflow {
        /// Native index that exceeded the typed identity.
        index: usize,
    },
    /// A child did not finish before its parent Adapter call.
    ChildOutputUnavailable {
        /// Parent whose Adapter call was about to begin.
        parent: PreparedVisualFrameNodeId,
        /// Direct child with no retained output.
        child: PreparedVisualFrameNodeId,
    },
    /// The schedule's recorded inbound edge no longer resolves exactly.
    MissingInboundBinding {
        /// Parent recorded by the execution schedule.
        parent: PreparedVisualFrameNodeId,
        /// Child whose exact edge could not be recovered.
        child: PreparedVisualFrameNodeId,
    },
    /// The root Adapter call produced no retained output.
    RootOutputUnavailable {
        /// Declared closure root.
        root: PreparedVisualFrameNodeId,
    },
}

impl fmt::Display for PreparedVisualExecutionStructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClosure => write!(formatter, "prepared visual execution closure is empty"),
            Self::MissingNode { node } => write!(
                formatter,
                "prepared visual execution references missing node {}",
                node.index()
            ),
            Self::BindingParentMismatch { owner, declared } => write!(
                formatter,
                "prepared visual node {} owns a binding declared for parent {}",
                owner.index(),
                declared.index()
            ),
            Self::MultipleParents { child } => write!(
                formatter,
                "prepared visual execution node {} has multiple parents",
                child.index()
            ),
            Self::Cycle { node } => write!(
                formatter,
                "prepared visual execution cycle reaches node {}",
                node.index()
            ),
            Self::UnreachableNode { node } => write!(
                formatter,
                "prepared visual execution node {} is unreachable from the root",
                node.index()
            ),
            Self::NodeIndexOverflow { index } => {
                write!(
                    formatter,
                    "prepared visual execution node index {index} overflowed"
                )
            }
            Self::ChildOutputUnavailable { parent, child } => write!(
                formatter,
                "prepared visual child {} was unavailable before parent {}",
                child.index(),
                parent.index()
            ),
            Self::MissingInboundBinding { parent, child } => write!(
                formatter,
                "prepared visual node {} lost its inbound binding from parent {}",
                child.index(),
                parent.index()
            ),
            Self::RootOutputUnavailable { root } => write!(
                formatter,
                "prepared visual root {} produced no output",
                root.index()
            ),
        }
    }
}

impl std::error::Error for PreparedVisualExecutionStructureError {}

/// Shared driver failure without erasing consumer diagnostics.
#[derive(Debug)]
pub enum PreparedVisualExecutionError<E> {
    /// Canonical closure/schedule invariant failed.
    Structure(PreparedVisualExecutionStructureError),
    /// Preview- or Export-owned Adapter failed.
    Adapter(E),
}

impl<E: fmt::Display> fmt::Display for PreparedVisualExecutionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Structure(error) => error.fmt(formatter),
            Self::Adapter(error) => error.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PreparedVisualExecutionError<E> {}
