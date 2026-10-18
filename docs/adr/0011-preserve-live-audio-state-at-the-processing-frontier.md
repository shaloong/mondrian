---
status: proposed
---

# Preserve live audio state at the processing frontier

## Problem

A Host fader edit currently changes the complete audible dependency closure
and constructs a fresh Runtime, Session and native owner at the Engine's
consumer anchor A. Configuration state is not a checkpoint of arbitrary DSP
history. The owned zero-latency echo control demonstrates a missing 256-frame
history term despite correct new PCM outside that interval and complete cleanup.
See the current [qualification matrix](../dev/native-plugin-compatibility.md).

Keeping only the native object cannot repair this: its actual processing
frontier F can already be ahead of A. Continuing that object from A would replay
input with the wrong state. Reweighting queued final PCM cannot recover arbitrary
upstream signal or replace graph execution. Existing native-author Plan handoff
also carries retained proposal and exact Ack permissions that Host edits lack.

## Proposed decision

Apply an admitted live Host math edit at the original Session's next processing
frontier F. Retain already generated PCM, the Session, processor objects, delay
lines, epoch and exact cursors. Queued PCM keeps its original math; newly
processed PCM uses the new immutable Plan. The receipt reports the actual F.
This is bounded live control latency, not a new Timeline clock: Engine anchor A,
author time, event offsets and explicit seek/re-entry semantics remain distinct.

The first capability is restricted to one direct-root terminal Program Output
static fader with no post-fader processors or selected Notes. All other authored
math, automation, processors and opaque configuration, routing, ports, PDC,
media/source mappings, nested dependencies, audition and Render Contract must
remain identical. Unsupported changes and unsettled native obligations retain
explicit admission failures; they cannot borrow this capability.

The existing prepared author Session ticket remains the sole transaction.
Preparation validates the exact current Runtime/Source and real resource overlap
before publication. A new Plan identity requires matching native leases, targets
and the one Source claim; retaining a native object does not retain an old target.
Author/history and Source publication must finish under their existing guards
with only prevalidated infallible moves. No SDK call or native Ack is invented
inside that final boundary. Presentation must follow the actual new claim.

Playback and Export keep the same compiler, Plan and Session math. A fresh export
evaluates its accepted author snapshot through normal entry; it receives no live
owner or history-recovery permission. There is no output-side gain authority,
second parameter model, routing bypass or native-name compatibility branch.

## Implementation gates

1. Prove that preparing same-object bindings without a retained native proposal
   preserves failure custody. The current initial-binding API revokes records;
   it is not a usable atomic replacement preparation. Cancellation, partial Bind
   and dropped preparations need an explicit original-owner outcome before any
   author install. This gate is unresolved; no new Host handoff API is accepted.
2. Define exact-shape math validation, current frontier evidence, resource overlap
   and one Source publication over the existing Session. Keep native-author
   proposal/cohort/Ack validation unchanged and separate from Host admission.
3. Integrate the real author consumer before expanding supported edit entry
   points. Nongesture, gesture, history and Project replacement must each have
   an explicit policy; postcommit failure cannot be described as author rollback.
4. Add a distinct F-seam control: every old queued sample survives, new PCM from F
   matches the fader step and finite history formula, and cursors never rewind.
   Preserve the existing A/reconstruction failure as evidence of the old contract;
   changing a selector or expected generation cannot count as fixing it.
5. Verify Source/target invalidation, busy/held refusal, cancellation, parameter
   handoff regressions and consuming cleanup before enabling the capability.

## Consequences

This extends immutable Plan publication, not the Audio Graph or project schema.
Live edit response includes already queued audio; competitive latency and device
behavior require separate measurements. Until these gates pass, the finite DSP
history defect stays open and the current implementation gains no new capability.
