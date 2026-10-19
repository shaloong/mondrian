---
status: accepted
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

## Decision

Apply an admitted live Host math edit at the original Session's next processing
frontier F. Retain already generated PCM, the Session, processor objects, delay
lines, epoch and exact cursors. Queued PCM keeps its original math; newly
processed PCM uses the new immutable Plan. The receipt reports the actual F.
This is bounded live control latency, not a new Timeline clock: Engine anchor A,
author time, event offsets and explicit seek/re-entry semantics remain distinct.
Normal author binding synchronization may still advance A at its actual observed
time. F, watermarks and callback counts do not enter semantic Plan identity.

The first capability is restricted to one direct-root terminal Program Output
static fader with no post-fader processors or selected Notes. All other authored
math, automation, processors and opaque configuration, routing, ports, PDC,
media/source mappings, nested dependencies, audition and Render Contract must
remain identical. The initial capability also requires no nested Runtime,
active editor, in-flight PCM or native control work. The actual retained Session
then supplies F without inventing a queue fence. Unsupported changes and
unsettled native obligations retain explicit admission failures; they cannot
borrow this capability.

The existing prepared author Session ticket remains the sole transaction.
Preparation validates the exact current Runtime/Source and real resource overlap
before publication. A new Plan identity requires matching parent leases, targets
and the one Source claim. Pure Host math retains the actual native binding UUID,
object and parameter source: the Worker does not encode the parent Plan address.
It needs no foreign Bind, source consumption or native Ack. Parent targets still
change with the Plan and installed serial; presentation must follow the actual
new claim instead of relabeling an old observation.

Keep the actual Runtime and metadata exclusively borrowed across dependent
publication and the original infallible author ticket installation. Finish every
fallible check before the Source CAS. That short guard swaps the prepared Plan,
parent leases and claim, then ends before author mutation. The original Session
API explicitly permits installation after dependent publication. Install the
author once, obtain its now-current execution snapshot and update metadata before
releasing the Runtime. No new DSP or target operation can observe the intermediate
state through that Runtime. Source claim observation alone is not author or DSP
permission. Do not hold an author execution read guard across its writer.

Within dependent Source/author publication, no fallible business operation,
cancellation reversal or SDK call follows the successful CAS. Retired Plan/factory
owners move outside all Runtime, metadata and Source locks before destruction.
Snapshot allocation and the original author
retirement occur outside the Source guard on the control thread. This is not an
arbitrary-panic rollback guarantee; a trusted author installation closure cannot
reenter App state or treat an invariant panic as a recoverable refusal.

The later original App event and Playback-binding consumer retains its Result
and real clock observation. Its failure is postcommit: it does not roll back
Source/author state or permit replaying the edit. Retained admission proves the
current Sequence, grid and extent binding before publication, avoiding that
consumer's seek/rebuild branch for this static edit.

Playback and Export keep the same compiler, Plan and Session math. A fresh export
evaluates its accepted author snapshot through normal entry; it receives no live
owner or history-recovery permission. There is no output-side gain authority,
second parameter model, routing bypass or native-name compatibility branch.

## Implementation gates

1. Prepare parent-only lease migration from actual installed records, checking
   the original Source, identity, epoch and closed native sources. Preserve all
   pending/held and unknown-capability refusals. Neither the initial-binding API
   nor native-author foreign Bind preparation is a rollback-safe substitute;
   those operations can change the child before graph publication.
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
