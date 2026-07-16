---
status: accepted
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

Mondrian Standard uses its v1 working-space contract: unbounded, scene-referred
Linear Rec.2020 with a D65
white point as its working RGB space. This is a video-first working identity:
it contains the Rec.709 and P3-D65 primaries, aligns directly with BT.2020 HLG
and PQ delivery primaries, and avoids making the Standard project model depend
on ACES AP0/AP1 roles. Floating-point RGB values are not constrained to the
BT.2020 chromaticity triangle; negative and greater-than-one components remain
valid and must survive input transforms, effects, compositing, caches, and
output planning. Stock OCIO may use its scene-reference hub internally and may
optimize adjacent matrices, but the project-visible working identity and the
`scene_linear` role are `Linear Rec.2020`.

ACEScg remains available to the ACES product mode and explicit VFX workflows.
It was not selected for Standard because its AP1/D60 identity would add a
product-visible ACES dependency and extra chromatic adaptation at the dominant
D65 video boundaries without providing a demonstrated compositing or GPU
benefit. Linear Rec.709 was rejected because it is too narrow for P3, BT.2020,
HDR, and common camera gamuts. Inventing new primaries was rejected because no
measured stability, coverage, interoperability, or performance benefit
justifies a new ecosystem contract.

Semantic stage count, OCIO transform-node count, and GPU pass count are not the
same thing. Performance gates measure the optimized processor and recorded GPU
work rather than penalizing correct domain separation.

Mondrian does not implement a native alternative to an OCIO processor. Renderer
optimizations may cache, bind, lower, fuse, or schedule OCIO-generated programs
and resources, but may not replace their color math. If stock OCIO cannot
express a required transform, the gap is documented and remains fail-closed;
it does not authorize a second MDRT or a maintained OCIO fork.

The current Standard v3 package assembles SDR v2 entirely in stock OCIO as a
target-linear conversion, HSV domain transform, a deterministic 61-cube gamut
surface, a 4096-entry 1D value-shoulder LUT baked from an OCIO grading curve,
range safety, target signal encoding, and display-reference conversion. New
sequences execute this package-pinned View by default; DisplayReferred remains
an explicit direct-colorimetric bypass. The legacy v2 package and its SDR v1
AgX formation graph remain addressable only through their exact persisted
identity. The legacy ACES Output Transform is not the Standard default.

The 1000-nit HDR View remains the stock-OCIO assembly with a pinned AgX HDR
formation resource, conversion to display-reference XYZ, and one HLG or PQ
display encoding. HLG and PQ share picture formation instead of duplicating or
borrowing an ACES Rendering Transform. The accepted implementation is protected
by package digests, SDR/P3/HLG/PQ quality corpus, CPU/GPU parity, preview/export
intent sharing, and a hardware-timestamp 4K performance gate.
