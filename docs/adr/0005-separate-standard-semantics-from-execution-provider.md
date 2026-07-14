---
status: proposed
---

# Use stock OCIO as the default Mondrian Standard execution infrastructure

Mondrian Standard is a Mondrian-owned, versioned color-management package
distributed as a bundled and immutable OpenColorIO configuration. The package
defines the supported roles, working spaces, input transforms, display/view
transforms, display color spaces, file rules, version manifest, and reference
corpus that together form the product default. An external config cannot
silently replace the meaning of a persisted Mondrian Standard version.

Mondrian exposes three product-level modes over one OCIO integration:

- Mondrian Standard selects the bundled, version-pinned Mondrian package;
- ACES selects a pinned official ACES package for workflows that require ACES;
- Custom OCIO selects an external show or facility config explicitly.

Stock OCIO is the default and authoritative execution infrastructure for all
three modes. Processor construction, optimization, CPU execution, GPU shader
extraction, resources, and cache invalidation remain shared. The renderer still
owns typed frame domains, working/output identities, target contracts,
scheduling, diagnostics, and preview/export parity; it does not infer product
color science from optional strings or implement a second implicit workflow.

The color pipeline keeps its semantic stages even when OCIO optimizes them into
one processor or one GPU pass:

1. input interpretation;
2. input-to-working conversion;
3. working-domain effects and compositing;
4. rendering/view transform;
5. display or delivery encoding;
6. monitor adaptation for presentation only.

Semantic stage count, OCIO transform-node count, and GPU pass count are not the
same thing. Performance gates measure the optimized processor and recorded GPU
work rather than penalizing correct domain separation.

A native specialization may be introduced only when an equivalent stock-OCIO
processor fails documented capability, fidelity, or performance gates. The
comparison must use the same mathematical transform, input/output domains,
precision contract, pass/fusion conditions, corpus, and target GPUs. A native
path must provide a material and repeatable p95/p99 benefit or implement
semantics stock OCIO cannot represent accurately. It remains an implementation
detail, must match the project-visible transform, and cannot become a separate
look or user-visible quality mode. Maintaining an OCIO fork is not the default
solution.

The existing ACES-backed production view remains unchanged until a lightweight
Mondrian candidate passes OCIO CPU/GPU conformance, representative SDR/HDR
image-corpus review, preview/export parity, version compatibility, and realtime
performance gates. This ADR becomes accepted only after the candidate and at
least one stock-OCIO production path provide that evidence. A same-math native
comparison is required only if later evidence indicates specialization may be
necessary.
