# Native audio thread scheduling

`mondrian-audio-scheduling` is a narrow native adapter consumed by execution
owners. It owns one physical thread's native registration and restoration; it
does not interpret author time, render modes, processor lifecycle, PDC, playback
continuity, editor transactions or device readiness. Media and Audio may depend
on this adapter without depending on each other or on desktop UI services.

## Windows admission and lifetime

The actual realtime PCM producer requests `PlaybackProducer` on its dedicated
thread, using the Windows MMCSS `Audio` task with relative normal priority.
A separate dedicated native DSP thread requests `NativeDsp`, using `Pro Audio`
with relative high priority. Main/editor/reader threads do not acquire a DSP
registration. Offline execution does not request native realtime scheduling.

The registration uses the documented Windows SDK APIs and the current physical
OS thread identity. Task admission and priority are both checked before producer
or DSP readiness is published. A refused priority request reverts its acquired
registration; a refused cleanup marks the physical thread unconfirmed and
requires that dedicated owner thread to terminate. A failed Windows request is
not changed into ordinary-scheduling success.

`RealtimeAudioThreadLease` is neither Send nor Sync. Its explicit `finish`
returns `RealtimeAudioThreadExitEvidence` only after a successful native revert
on the original physical thread. Entry facts and confirmed exit facts are
different types. Failed restoration forbids another registration on that thread,
even if Drop's single fallback retry later succeeds. Drop's diagnostics describe
implicit exits and failed native attempts; they do not manufacture an exit receipt.

The Media producer returns from construction only after its same-thread native
admission handshake. It retains entry/exit/refusal facts in its existing queue
owner for ordinary control reads. A failed restore cannot publish a normal
successful render-worker terminal receipt. Renderer unwind is caught while the
scheduling lease remains owned, then restoration is attempted before joining.

Native DSP consumers must acquire before their persistent scoped executor's
readiness/first Process, restore after the final Process, and join before native
main/editor/lifecycle borrowing ends. This adapter alone does not move existing
inline plugin processing onto such a DSP thread or qualify native GUI ownership.

## Platform and resource boundaries

The Windows adapter rejects unsupported direct calls with a typed error. Media's
unchanged non-Windows producer explicitly reports `OrdinaryPlatform`, so its
ordinary execution remains usable without a Windows or cross-platform RT claim.
Native macOS/Linux scheduling and their performance qualification remain separate
implementation work. Offline work remains ordinary on every platform.

The producer explicitly reserves a 2 MiB thread stack. A scoped DSP lane reserves
its own declared stack through its execution owner's existing resource profile.
The guard's `inline_layout` is observable actual inline storage and must not be
charged twice inside an already charged stack. The producer state is an inline
Mutex field of the existing queue owner; the one startup channel is ordinary
construction work, not a per-callback allocation. Its standard-library owning
allocation, queue Arc allocation, TLS/CRT/OS bookkeeping and native MMCSS resource
frontiers still require their corresponding exact owner proof. This adapter is
not a process-RSS or full candidate-resource grant.

## Qualification boundary

MMCSS accepts a scheduling responsibility and changes OS scheduling behavior.
It provides no hard realtime promise, callback deadline, device-delivery or
private DSP continuity guarantee. Registration success must not substitute for
measured parent/child callback distributions, root-window admission, physical
output quiescence, preroll or device-ready evidence.

The public 8-frame/48 kHz synchronous leaf deadline remains 166.7 microseconds.
The product's larger queued producer window is a different execution domain;
its device/root tests cannot relabel a rejected 8-frame leaf profile as a pass.
Whole-root pending-request IPC, native scoped-thread handoff, parameter events,
PCM copies and the actual scheduler must all be included in their real timed path.

Primary native contracts:

- [AvSetMmThreadCharacteristicsW](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avsetmmthreadcharacteristicsw)
- [AvSetMmThreadPriority](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avsetmmthreadpriority)
- [AvRevertMmThreadCharacteristics](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avrevertmmthreadcharacteristics)
- [Multimedia Class Scheduler Service](https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service)
