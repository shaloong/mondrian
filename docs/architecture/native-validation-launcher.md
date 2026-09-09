# Native validation launcher

`mondrian-validation-launcher` is a validation-only binary/library with no Media,
FFmpeg, App or GPU dependency. The package also contains a clearly nonqualifying
`native_bootstrap_probe` diagnostic executable for native owner tests.

The trusted invocation locks the externally approved launcher executable and plan
before starting it. The launcher reads the exact run request and its machine-plan
binding, verifies the approved application, launcher and complete ordered runtime
closure, copies all application/DLL objects into one private namespace, retains
both source and staged file handles, and applies the same native protected DACL
implementation as Media's CLI capsule. Every ancestor remains held against
rename/delete. The namespace denies owner ACL replacement through OWNER RIGHTS;
the pre-acquired restoration handle remains private to its consuming owner.

The launcher creates the App suspended, assigns its native process to a kill-on-
close Job Object, verifies that its single native primary thread remains suspended,
and resumes it. A local nonblocking named pipe authenticates both actual process
IDs using kernel peer queries. The App checks the server executable against the
externally bound launcher digest, verifies full-file hashes/native file identities
for every staged object, reads back the namespace seal, and enumerates its actual
non-system modules. `owned_images` includes delay-loaded files;
`mapped_image_paths` records only mappings actually observed. Unknown mapped DLLs,
missing objects, a changed namespace, a mismatched peer/request, or an expired
bootstrap deadline cannot create `PreparedAuthority`.

The launcher is the trusted code entrypoint. Native peer path/hash checks are not
claimed to reconstruct an arbitrary already-running launcher's original mapped
section. This contract addresses filesystem races and ordinary namespace
mutation, and excludes malicious OS/system DLLs, process injection, privileged
handle theft, and replacement of the trusted invocation/verifier itself. No new
security principal, service, privileged broker, or partial-PE hash is introduced.

Media borrows the completed authority to map approved source DLL bindings onto the
objects that existed before this fresh App loaded. Demux and probe helpers reuse
the same sealed application image through typed commands restricted to their two
native worker protocols. Their retained child leases remain in the same FFmpeg
session ledger, while the outer Job independently owns every native descendant.
Arbitrary vendor `Command` execution remains unavailable in a qualified process
until that vendor receives its own explicit approved executable contract.

The App cannot attest cleanup that happens after it exits. Its run manifest closes
its phase, Surface, and CLI capsule owners. The launcher then observes root native
exit plus zero Job active processes, restores/removes its exact namespace, hashes
the completed child manifest, and writes a separate create-only `LaunchReport`.
The independent verifier requires an externally approved hash of that outer
report for every started phase and reconciles the same manifest, runtime image,
machine plan, object closure, and observed mapping set. Every native termination,
accounting, deadline, or cleanup failure remains a failed raw report.

`tests/native_bootstrap.rs` exercises real fresh-process handshake/cleanup, refused
bootstrap, and a root process exiting with a deliberately surviving descendant.
Unit tests cover DACL replacement/injection rejection, oversized/truncated pipe
frames, expired original deadlines, and full-file overlay/hash mismatches.

The complete approved DLL set also includes any packaged native Reference Output
bridge and its non-system dependencies. Because AJA discovers its bridge beside
the application executable, those files must be in the same approved package DLL
closure that the launcher stages. The loader never enables a late unapproved
provider directory to turn missing capability into a claimed hardware run.

Windows namespace sealing retains READ_CONTROL/WRITE_DAC handles for every direct
staged child before protection. The inherited OI/CI policy preserves fresh loader
read/execute access while denying mutation. Native DACL readback treats only the
historical SE_DACL_AUTO_INHERITED marker as non-authoritative metadata; protection
and every ACE/right remain exact. Cleanup restores each child through its
pre-acquired authority, because parent ACL propagation cannot reacquire denied
child WRITE_DAC after sealing. These handles are released before exact removal.

Bootstrap framing uses native ReadFile/WriteFile status under the original deadline.
Rust File read maps ERROR_NO_DATA to zero bytes; native reads preserve that empty-live
PIPE_NOWAIT condition while rejecting broken/disconnected peers. A delayed-peer native
regression verifies both eventual transfer and immediate closed-peer rejection.
