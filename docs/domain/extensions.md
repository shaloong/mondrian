# Extensions and Permissions

Accepted target vocabulary; the general platform is not implemented. See
[Extension architecture](../architecture/extension-system.md) and
[ADR 0011](../adr/0011-use-managed-extension-platform.md).

## Vocabulary

### Extension

The independently managed unit of installation, identity, version, dependencies,
activation, and removal. It contains contributions, resources, and optional Web
or Rust executable entries. Resource-only language packs and themes are valid
Extensions. One Extension may combine UI, automation, and processing contributions.
_Avoid_: Extension defined as a Rust crate, effect, native DLL, or mandatory
background process; treating format compatibility as package distribution.

### Contribution and Extension Point

A Contribution is a capability supplied by an Extension, such as a command,
panel, translation catalog, asset provider, or processing definition. An
Extension Point is its public host-owned registration and interaction contract.
It defines identity, applicable context, multiplicity, replacement selection,
and lifecycle. Definitions remain admitted by their owning domains.
_Avoid_: Arbitrary access to AppState, registration as permission to execute,
load-order-based replacement, or a second global processing authority.

### Execution Container

The restricted environment for executable Extension code. Web entries have real
HTML/CSS/DOM; Rust entries may use a WASM runtime or an OS-sandboxed native Worker.
The implementation is unresolved, while permission semantics are shared.
_Avoid_: SDK cooperation, panic catching, or a child-process deadline described
as a security sandbox; native Rust trait objects used as a stable cross-binary API.

### Permission Broker and Permission Grant

The host-owned authority that checks an authenticated caller, operation, target
scope, and current user policy before executing protected work. A Permission
Grant allows a scoped capability, such as access to a selected file or network
service; it does not grant a renderer memory budget, a domain mutation, or access
to another Extension's resources. Revocation affects subsequent admission.
_Avoid_: Self-declared caller identity, manifest declarations treated as consent,
raw ambient OS access, or any implied guarantee that in-flight external effects
can be undone by revocation.

### Permission Outcome

Granted operations execute without repeated prompts. Denied, revoked, and
out-of-scope operations fail before the protected action. Undecided requests
follow explicit user policy; a background request may report authorization
required without prompting. Repeated calls do not re-prompt a rejected request.
Error type names and serialized schema are not yet public APIs.
_Avoid_: Silent empty results, partial writes on authorization failure, implicit
privilege escalation, or an authorization check only in the Extension UI.

### Native-format Plugin

An OpenFX, VST3, CLAP, or other format-specific component admitted by its concrete
Adapter and domain execution contract. It may be user-installed independently
of the future Extension package manager. Its format identity and host ABI are
distinct from Extension package identity and the registered effect-definition key.
_Avoid_: Renaming native-format ABIs or saved keys to implement product terminology;
equating native-plugin crash containment with restricted host access.

### Registered Effect Definition

An effect-domain definition admitted through the existing Effect SDK or a native
Adapter. Its stable key, parameter schema and execution contract describe one
processing capability, not an installed Extension package. EffectExtension* and
EffectType::External are the source interfaces; authored keys and project wire
values have separate data contracts. None identifies a general Extension runtime
or a native-format ABI.
_Avoid_: Applying an effect-key naming convention to future Extension package IDs,
or treating source-integrated effect callbacks as sandboxed third-party code.

### Companion Application

A separately trusted local application used by an Extension. Launching or
connecting is a protected operation. An independently installed/running companion
has its own OS authority; any requests it makes through Mondrian still require
appropriate authorization. Host-managed children inherit the intended sandbox.
_Avoid_: Localhost assumed trusted, launching an unrestricted child to bypass
permissions, or claiming control of a companion's independent network activity.

### Extension Dependency in an Authored Project

A stable definition/resource reference with version/schema and state sufficient
to diagnose and recover missing or incompatible dependencies. It is distinct
from a machine-local installation, live Worker, and permission decision.
_Avoid_: Executable code or user permissions embedded as project authority,
deleting unknown state, silent substitution, or executing code merely by opening
a project that references it.
