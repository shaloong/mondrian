# Vendored VST3 host dependency

## Instance-local plug-in interface declarations

`PluginInterfaceSupport` is a typed immutable inline value, supplied by the
builder to the native host context before factory/component/controller
initialization. Embedded applications select only actual consumers; exposing a
vendor library API alone does not establish product support. The standalone
library's original declaration set is preserved exactly. A nondefault declaration
with the library's optional isolation helper is explicitly refused because that
helper wire carries no policy. Standalone safe discovery/probe APIs keep their
documented library-default context; Mondrian uses its own supervised discovery
and processing children with the same selected product policy.

The SDK contract is
<https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IPlugInterfaceSupport.html>.
No generated SDK ABI, native plug-in interface, license or shared audio domain
model changes. COM tests cover selected acceptance/refusal, unknown/null IIDs,
independent simultaneous contexts and the unchanged default declarations.

Source: https://github.com/HelgeSverre/rust-vst3-host
Revision: 49fc189177c72cf799d5034fcca81d8aacc3d7a6
Package: vst3-host 0.9.0, original author Helge Sverre, MIT license retained.

The existing selected-class native host remains the sole VST3 host implementation.
Vendoring permits querying audio bus roles/names from the actual loaded component,
after state restore, without a second host or inference from channel counts.
This top-level vendor dependency has its own workspace boundary and is excluded
from the Mondrian workspace; its inherited upstream
workspace dependencies are written explicitly. Upstream examples/dev-only GUI
tooling are omitted; library source and binary source are retained unchanged apart
from the documented adapter extension. No upstream Cargo cache is modified.

Local extension: typed, bounded actual-component audio-bus metadata query in the
existing Plugin/PluginInternal/PluginImpl seam. Unsupported isolation metadata
queries fail explicitly; Mondrian uses this API inside its own supervised child.
No process callback, mixer, state envelope or automation implementation is forked.

SDK contract: https://github.com/steinbergmedia/vst3_pluginterfaces/blob/master/vst/ivstcomponent.h

Three loader files only have upstream trailing whitespace removed for patch hygiene.

## Format-neutral Windows native editor parent

`WindowsNativeEditorContainer` borrows the same STA/main owner and reuses the
existing WndProc, physical client resize, bounded message service and close/DPI
observations. It owns no plugin/controller or author/install permission. The
format adapter must acknowledge its native GUI destruction before explicit
parent destruction. This lets the CLAP adapter borrow a real Win32 parent
without manufacturing an `IPlugView` or second window/event interpretation.
