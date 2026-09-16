# Linux graphics process startup

`mondrian-platform::prepare_graphics_process` establishes the graphics loader
policy at executable entry, before creating threads, logging, media workers or
graphics owners. Product and native validation entrypoints use this same
function. Pure protocol tests, runtime inspection and internal media-only worker
modes do not need a graphics bootstrap.

Linux Vulkan driver discovery can overlap instance/device destruction in
independent owners. On Ubuntu 24.04 with NVIDIA 580.173.02 and Vulkan loader
1.3.275, two independent native threads repeatedly creating and destroying
devices reproduced SIGSEGV inside the loader/ICD chain. A C-only reproducer also
failed, so changing Playback or adding a shared GPU context does not repair the
underlying problem.

The executable reexecutes `/proc/self/exe` with
`VK_LOADER_DISABLE_DYNAMIC_LIBRARY_UNLOADING=1` when the variable is absent.
This is the loader's documented option (supported from loader 1.3.259). An
explicit value other than `1` is a structured startup error, not silently
overridden. An already established value returns immediately. Other operating
systems do nothing. A loader predating that option is outside this workaround's
verified environment; setting the variable alone is not evidence of support.

`Command::env` builds the replacement environment without changing the running
process environment. `exec` preserves PID, arguments including argv[0], working
directory and standard streams. No parent/child supervisor pair is introduced.
Using the current mapped executable through `/proc/self/exe` also avoids
resolving a replacement file at the original executable pathname. Failure to
exec returns a typed error before resources open. This function is an entrypoint
operation and must never be invoked as a recovery action on an active session.

Driver and layer code libraries remain resident until process exit. This is an
explicit process-lifetime policy; it does not retain a `GpuContext`, instance,
device, queue, frame or callback, serialize rendering, bypass generation checks,
or make a shutdown receipt succeed. Device loss/reopen continues to consume and
replace the original production owners. Process termination is still the only
point at which all process-resident code mappings are gone. The workaround does
not repair the supplier's unload path or qualify it.

Applications embedding the renderer must establish the same policy in their
own process entrypoint before loading Vulkan. Calling `GpuContext::new` from a
library cannot safely reexec an embedding process. Direct libtest GPU stress
runs likewise need the variable supplied by their process launcher; they are
not product bootstrap entrypoints.

The subprocess regression tests verify one reexecution with unchanged PID,
argv, current directory and non-Unicode environment, no reexecution when the
policy already exists, and fail-closed handling of conflicting values. Native
validation additionally exercises independent GPU owners and surface/device
reopen. Neither those short runs nor this policy supplies HDR, SDI wire,
sealed-runtime or 72-hour commercial qualification.

Reference: [Khronos Vulkan loader environment variables](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderInterfaceArchitecture.md#environment-variables).
