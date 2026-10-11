# Native audio lane resource requests

The scoped native main/reader/DSP lane contributes a checked additional request
to the existing processor execution contract and sole Session admission. It does
not introduce a second budget authority or an installed-owner issuer.

`NativeWorkerLaneResourceProfile` describes seven fixed known owners using the
real concrete owning types' layouts. The SDK editor/process role Boxes need
complete SDK owning observers; the size of a trait-object pointer cannot describe
their allocations. Main-owned fixed slots are identified logical inline storage
obligations, separate from the requested reader and DSP stack reservations.

The requested stack extents are 2 MiB for the reader and 8 MiB for the DSP lane.
They are `thread::Builder::stack_size` requests, not committed physical RAM or
portable guard-page observations. Native platform and standard-library thread
initialization, TLS, synchronization/kernel bookkeeping and vendor memory remain
explicit accounting frontiers. A complete candidate grant cannot infer these
missing owners are zero.

Control overlap specifies admitted capacity ceilings for the one decoded input,
the correlated response and an additional staged seed. The adapter accounts
unchanged retained allocations once under their original identity. It compares
actual Vec capacities after allocation, and refuses an excess before Ready,
native execution or control publication. Nested metadata and error strings need
their own owner evidence; a fixed message slot does not make them inline.

The same adapter computes the preparation profile in the parent and child. The
child compares the exact validated profile before native load/thread creation;
the fixed owners are observed again after creation. The profile's additional
ceiling enters `with_additional_session_scratch_bytes`, alongside existing shared
mapping obligations. A profile request itself cannot install a graph, mint an
editor lease, reset continuity or override the root resource grant.

The source packet adds the request/observation API and new-API qualification
controls. Its concrete native consumer wiring, thread/Ready/refusal behavior and
full root candidate allocation union require separate actual qualification.
The standard allocator control measures this API's observation path only; it
does not establish that native DSP, IPC or the complete worker is allocation free.


The three dynamic control ceilings describe a union of possible owners, so their
alignment is the maximum required power-of-two alignment. Actual allocation
alignment must divide that ceiling and capacity bytes must not exceed it. A
decoded f64 parameter vector (alignment 8), byte seed and owned title (alignment
1) can use the same bounded command slot without falsifying either observation.
All seven fixed concrete owners still require exact size and alignment; this
union rule does not relax their measurement or whole-root resource admission.
The runtime must observe actual Vec capacity or complete Box layout, not rewrite
typed owners to alignment 1 or count only initialized payload lengths.
