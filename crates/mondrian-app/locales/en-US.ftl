app-name = Mondrian
about-version = Version: { $version }
about-renderer = Renderer: { $renderer }
about-rust = Rust: { $version }
about-os = OS: { $description }
about-gpu = GPU: { $name }
about-copy = Copy
about-close = Close
startup-heading = Get started
startup-new-project = New project
startup-open-project = Open project
startup-recoverable-projects = Recoverable projects
startup-recent-projects = Recent projects
startup-no-recent-projects = No recent projects
new-project-title = New project
new-project-untitled = Untitled
new-project-description = Choose production settings and create a timeline.
new-project-name = Name
new-project-name-placeholder = Project name
new-project-frame-size = Frame size
new-project-frame-rate = Frame rate
new-project-audio = Audio
new-project-color-mode = Project color mode
new-project-resolution-hd = HD 720p
new-project-resolution-fhd = Full HD 1080p
color-custom-ocio = Custom OpenColorIO
color-select-custom-ocio = Choose custom OpenColorIO…
new-project-create-proxies = Create proxies
new-project-preview-cache = Preview cache
new-project-cancel = Cancel
new-project-create = Create...
color-choose-ocio-config = Choose OpenColorIO configuration
color-ocio-config-filter = OpenColorIO configuration
project-settings-title = Project color engine
project-settings-description = Applies to every sequence. Incompatible existing sequences or new-sequence defaults reject the whole change; no sequence is rewritten.
project-settings-builtin-detail = Built-in package: { $package } · Current working spaces: { $workingSpaces }
project-settings-aces-detail = Built-in OCIO configuration: { $preset } · Current working spaces: { $workingSpaces }
project-settings-custom-detail =
    { $source }
    Output bindings: { $outputs }
    Configuration SHA-256: { $sha256 }
project-settings-cancel = Cancel
project-settings-apply = Apply
sequence-settings-title = Sequence settings
command-file-musical_time = Musical Time...
music-title = Musical time
music-description = Set a constant tempo and the sequence time of its first beat. Decimal or fraction input is exact. Leave the time signature blank if unknown.
music-enabled = Provide musical time to audio plug-ins
music-tempo = Tempo (BPM)
music-origin = First beat (seconds)
music-signature = Time signature (e.g. 7/8)
music-invalid-tempo = Enter a positive tempo within the supported exact time range.
music-invalid-origin = Enter a valid first-beat time in seconds.
music-invalid-signature = Enter a positive time signature, such as 7/8, or leave it blank.
sequence-settings-description = Adjust the active sequence's timeline format and preview settings.
sequence-tab-format = Format
sequence-tab-color = Color
sequence-tab-preview = Preview
sequence-name = Name
sequence-name-placeholder = Sequence name
sequence-format = Format
sequence-custom-frame-size = Custom frame size
sequence-timecode-start = Timecode start (actual frames)
sequence-audio = Audio
sequence-preview = Preview
sequence-color-management = Color management
sequence-width = Width
sequence-height = Height
sequence-start-frame = Start frame
sequence-edit-custom = Custom
sequence-resolution-hd = HD 720p
sequence-resolution-fhd = Full HD 1080p
sequence-pixel-square = Square pixels (1.0)
sequence-pixel-unknown = Unknown pixel aspect ratio
sequence-field-progressive = Progressive
sequence-field-upper = Upper field first
sequence-field-lower = Lower field first
sequence-display-frames = Sequence frames
sequence-audio-mono = Mono
sequence-audio-stereo = Stereo
sequence-audio-speakers = Custom speaker layout
sequence-audio-discrete = Discrete channels
sequence-audio-samples = Audio samples
sequence-audio-milliseconds = Milliseconds
sequence-color-display-referred = Display-referred
sequence-color-scene-referred = Scene-referred
sequence-metadata-assume-709 = Assume Rec. 709
sequence-metadata-reject = Reject media
sequence-range-full = Full range
sequence-range-legal = Legal range
sequence-tone-map-auto = Output mapping: Automatic
sequence-tone-map-always = Output mapping: Always
sequence-tone-map-never = Output mapping: Technical bypass
sequence-project-color-engine = Project color engine: { $engine }
sequence-preview-resolution = Preview resolution { $percent }
sequence-preview-cache = Preview cache
sequence-auto-tone-map = Automatically tone map media
sequence-static-hdr = Write static HDR metadata
sequence-cancel = Cancel
sequence-apply = Apply
sequence-name-required = Sequence name cannot be empty
menu-file = File
menu-edit = Edit
menu-view = View
menu-graphics = Graphics
menu-window = Window
menu-help = Help
menu-import = Import
menu-import-folder = Folder...
menu-install-openfx-filter = Install OpenFX Filter…
menu-install-openfx-generator = Install OpenFX Generator Effects…
menu-export = Export
menu-export-settings = Export Settings...
menu-workspace = Workspace
command-file-new_project = New Project...
command-file-open_project = Open Project...
command-file-import_media = Media...
command-file-save_project = Save
command-file-save_project_as = Save As...
command-file-export_portable_package = Package Project...
command-file-cancel_portable_package_export = Cancel Project Packaging
command-file-project_settings = Project Settings...
command-file-sequence_settings = Sequence Settings...
command-file-close_project = Close Project
command-edit-undo = Undo
command-edit-redo = Redo
command-edit-cut = Cut
command-edit-copy = Copy
command-edit-paste = Paste
command-edit-duplicate = Duplicate
command-edit-delete_selection = Delete Selection
command-edit-select_all = Select All
command-edit-deselect_all = Deselect All
command-app-preferences = Preferences...
command-viewer-capture_gallery_still = Capture Gallery Still
command-view-toggle_fullscreen = Toggle Fullscreen
command-timeline-create_basic_title = Basic Title
command-workspace-editing = Editing
command-workspace-color = Color
command-workspace-audio = Audio
command-workspace-compositing = Compositing
command-workspace-export = Export
command-app-about = About Mondrian
panel-assets = Assets
asset-library = Project library
asset-search = Search assets
asset-empty-title = Drop media here to start editing
asset-empty-description = Video, audio, images and sequences are supported
asset-no-results-title = No matching assets
asset-no-results-description = Try another search or clear the filter
asset-library-disconnected = No project library
asset-library-unavailable = Library unavailable
asset-delete-selected = Delete selected
asset-back = Back
asset-parent = Parent
asset-all = All assets
asset-all-badge = All
asset-item-count = { $count ->
    [one] 1 item
   *[other] { $count } items
}
asset-kind-video = Video
asset-kind-still = Still image
asset-kind-audio = Audio
asset-kind-adjustment = Adjustment layer
asset-kind-solid = Solid color
asset-offline = Offline
asset-proxy = Proxy
asset-interpret = Interpret asset...
asset-reveal = Show in File Explorer
asset-relink = Relink media...
asset-disable-proxy = Disable proxy mode
asset-enable-proxy = Enable proxy mode
asset-delete = Delete asset
asset-delete-folder = Delete folder
asset-import = Import media...
asset-new = New
asset-new-adjustment = Adjustment layer
asset-new-solid = Solid color
asset-new-folder = Folder
panel-viewer = Viewer
viewer-no-sequence = No sequence
viewer-no-signal = No signal
viewer-fit = Fit
viewer-no-sequence-loaded = No sequence loaded
viewer-frame-count = { $count ->
    [one] 1 frame
   *[other] { $count } frames
}
viewer-loading = Preparing preview
viewer-color-rejected = Color interpretation rejected
viewer-blocked = Preview blocked
viewer-failed = Preview failed
viewer-playing = Playing
viewer-ready = Ready
viewer-auxiliary-effects-skipped =
    { $count ->
        [one] One effect skipped
       *[other] { $count } effects skipped
    }
viewer-auxiliary-detail = { $location } · { $effect } · { $parameter } — { $reason }
viewer-auxiliary-unbound = Choose an input source.
viewer-auxiliary-unavailable = The input source is unavailable.
viewer-auxiliary-grade-before = Group pre-clip grade
viewer-auxiliary-grade-after = Group post-clip grade
viewer-auxiliary-transition-outgoing = Outgoing transition clip
viewer-auxiliary-transition-incoming = Incoming transition clip
viewer-color-rejection-detail =
    Color interpretation rejected
    Asset: { $asset }
    Policy: { $policy } / { $source }
    Detection: { $method } / { $confidence } / warnings { $warnings }
    Issues: { $issues }
    { $detail }
panel-scopes = Scopes
panel-timeline = Timeline
panel-inspector = Inspector
panel-mixer = Mixer
panel-effects = Effects
effect-search = Search effects
effect-empty = No effects available
effect-category-color = Color
effect-category-grading = Grading
effect-category-blur-sharpen = Blur and Sharpen
effect-category-stylize = Stylize
effect-category-transform = Transform
effect-category-keying = Keying
effect-category-plugins = Plugins
effect-category-external = External Effects
effect-category-generator = Generator
effect-basic-correction = Basic Correction
effect-white-balance = White Balance
effect-lut-3d = 3D LUT
effect-color-wheel = Primaries
effect-hdr-grading = HDR Grading
effect-asc-cdl = ASC CDL
effect-curves = Curves
effect-gamut-compression = Gamut Compression
effect-highlight-recovery = Highlight Recovery
effect-qualifier = Qualifier
effect-hue-saturation-lightness = Hue, Saturation and Lightness
effect-crop = Crop
effect-gaussian-blur = Gaussian Blur
effect-sharpen = Sharpen
effect-vignette = Vignette
effect-chromatic-aberration = Chromatic Aberration
effect-grain = Grain
effect-chroma-key = Chroma Key
effect-luma-key = Luma Key
panel-node-graph = Node Graph
panel-export = Export
preferences-language = Interface language
preferences-language-system = Follow system
preferences-language-zh-cn = 简体中文
preferences-language-en-us = English
preferences-language-pseudo = Pseudo locale (layout check)
notification-import-complete =
    { $count ->
        [one] Imported one media file
       *[other] Imported { $count } media files
    }
notification-import-partial =
    { $imported ->
        [one] Imported one media file
       *[other] Imported { $imported } media files
    }; { $failed ->
        [one] one failed
       *[other] { $failed } failed
    }
notification-import-failed = Import failed: { $reason }
notification-timeline-drop-failed = Could not place file on the timeline: { $reason }
notification-save-complete = Project saved
notification-save-warning = Project saved, but recovery cleanup failed: { $reason }
notification-save-failed = Could not save project: { $reason }
notification-autosave-warning = Autosave completed, but recovery cleanup failed: { $reason }
notification-autosave-failed = Autosave failed: { $reason }
notification-export-complete = Export complete: { $path }
notification-export-failed = Export failed: { $reason }
notification-package-complete = Portable project package ready: { $path }
notification-package-failed = Could not package project: { $reason }
notification-action-failed = Action failed: { $reason }
pending-close-title = Save project changes?
pending-close-body-close = Save your changes before you close the project?
pending-close-body-quit = Save your changes before you quit Mondrian?
pending-close-save-close = Save and Close
pending-close-save-quit = Save and Quit
pending-close-discard = Don't Save
pending-close-cancel = Cancel
recovery-title = Confirm project recovery
recovery-summary = This recovery point contains author generation { $generation } and document revision { $revision }. The manifest has { $count } verifiable recovery points.
recovery-time = { $exact } ({ $relative })
recovery-saved-at = Saved: { $time }
recovery-source = Recovery source: { $path }
recovery-target = Save destination: { $path }
recovery-target-state = Destination status: { $state }
recovery-target-missing = The destination file does not exist. The first save after recovery will create it at this path.
recovery-target-same = Document revision { $revision } exists at the destination. The recovery point contains later unsaved edits.
recovery-target-older = Earlier document revision { $revision } exists at the destination. The recovery point is revision { $snapshot }.
recovery-target-newer = Destination revision { $revision } is newer than recovery revision { $snapshot }. Recovery will not immediately overwrite the destination.
recovery-safety = Confirming verifies and opens this recovery point as an unsaved project. It does not immediately overwrite the destination. If the destination, manifest, or recovery file changed, recovery stops safely.
recovery-confirm = Recover this version
recovery-cancel = Cancel
recovery-age-seconds =
    { $count ->
        [one] One second ago
       *[other] { $count } seconds ago
    }
recovery-age-minutes =
    { $count ->
        [one] One minute ago
       *[other] { $count } minutes ago
    }
recovery-age-hours =
    { $count ->
        [one] One hour ago
       *[other] { $count } hours ago
    }
recovery-age-days =
    { $count ->
        [one] One day ago
       *[other] { $count } days ago
    }
recovery-row-single = { $age } · { $location }
recovery-row-multiple = { $age } · { $count } recovery points · { $location }
startup-recent-now = Just now
startup-recent-unknown-time = Unknown modification time
startup-recent-unknown-size = Unknown size
startup-recent-file-unavailable = File unavailable
startup-recent-detail = { $age } · { $size }
file-dialog-create-project = Create Mondrian project
file-dialog-open-project = Open Mondrian project
file-dialog-import-media = Import media
file-dialog-install-clap = Install CLAP plugin
file-dialog-install-vst3 = Install VST3 plugin
file-dialog-install-vst3-bundle = Select VST3 bundle folder
file-dialog-install-openfx-bundle = Select OpenFX .ofx.bundle
file-dialog-relink-media = Relink media
file-dialog-save-as-project = Save Mondrian project as
file-dialog-package-project = Package Mondrian project
file-dialog-export-output = Choose export destination
file-dialog-import-ancillary = Import ANC / broadcast captions
file-dialog-import-pse = Import regulatory PSE configuration
file-default-untitled = Untitled
file-default-untitled-project = Untitled Project
file-filter-project = Mondrian project
file-filter-package = Mondrian portable project package
file-filter-video = Video
file-filter-image = Image / Camera RAW
file-filter-audio = Audio
file-filter-clap = CLAP plugin
file-filter-vst3 = VST3 plugin
file-filter-media = Media
file-filter-export = Export
file-filter-ancillary = ANC JSON / SCC V1.0 / raw CDP
file-filter-pse = Regulatory PSE JSON
file-dialog-select-icc = Choose display ICC profile
file-filter-icc = ICC display profile
inspector-blend-mode = Blend Mode
blend-mode-inherit = Inherit Track
blend-mode-normal = Normal
blend-mode-dissolve = Dissolve
blend-mode-darken = Darken
blend-mode-multiply = Multiply
blend-mode-colorburn = Color Burn
blend-mode-linearburn = Linear Burn
blend-mode-darkercolor = Darker Color
blend-mode-lighten = Lighten
blend-mode-screen = Screen
blend-mode-colordodge = Color Dodge
blend-mode-lineardodge = Linear Dodge (Add)
blend-mode-lightercolor = Lighter Color
blend-mode-overlay = Overlay
blend-mode-softlight = Soft Light
blend-mode-hardlight = Hard Light
blend-mode-vividlight = Vivid Light
blend-mode-linearlight = Linear Light
blend-mode-pinlight = Pin Light
blend-mode-hardmix = Hard Mix
blend-mode-difference = Difference
blend-mode-exclusion = Exclusion
blend-mode-subtract = Subtract
blend-mode-divide = Divide
blend-mode-hue = Hue
blend-mode-saturation = Saturation
blend-mode-color = Color
blend-mode-luminosity = Luminosity
audio-rack-scope-title = Audio Processor Rack
audio-rack-prefader-title = Pre-Fader Processor Rack
audio-rack-postfader-title = Post-Fader Processor Rack
audio-rack-scope-unbound = Unbound Processing Scope
audio-rack-scope-single = Used by one component
audio-rack-scope-shared = Shared Processing Scope · { $count } components
audio-rack-scope-identity = Processing Scope · { $identity }
audio-rack-track-identity = Audio Track · { $identity }
audio-rack-locked-track = Track { $identity } is locked; unlock it to edit processors
audio-rack-locked-scope-track = Shared scope is bound to locked track { $identity }; unlock it to edit processors
audio-rack-invalid-state = Cannot inspect this processor rack: { $error }
audio-processor-gain = Gain
audio-processor-lookahead-limiter = Lookahead Limiter (Sample Peak)
audio-processor-sample-delay = Sample Delay
audio-parameter-gain = Gain
audio-parameter-ceiling = Ceiling
audio-parameter-lookahead = Lookahead
audio-parameter-release = Release
audio-parameter-delay-samples = Delay Samples
audio-mixer-empty = No active sequence. Open or create a sequence to mix tracks, buses, and program outputs.
audio-mixer-invalid-strip = Cannot inspect this channel strip: { $error }
audio-mixer-invalid-route = Cannot inspect this route: { $error }
audio-mixer-output = Program Output · { $identity }
audio-mixer-port-prefader = Pre-Fader
audio-mixer-port-postfader-premute = Post-Fader / Pre-Mute
audio-mixer-port-postmute = Post-Mute
audio-mixer-port-postfader = Post-Fader
audio-mixer-missing-bus = Missing Bus · { $identity }
audio-mixer-missing-output = Missing Program Output · { $identity }
audio-mixer-locked-strip = Track { $identity } is locked; processors, input trim, and fader are read-only
audio-mixer-locked-route = Track { $identity } is locked; its routes are read-only
interpret-title = Interpret Footage
interpret-current = Current interpretation
interpret-input-color-space = Input color space
interpret-signal-range = Signal range
interpret-yuv-matrix = YCbCr matrix
interpret-matrix-unknown = Unknown
interpret-matrix-auto = Auto (metadata: { $detected })
interpret-raw-exposure = RAW exposure
interpret-raw-white-balance = RAW white balance
interpret-debayer = Demosaic quality
interpret-apply = Apply
interpret-cancel = Cancel
interpret-range-full = Full range
interpret-range-limited = Limited (video) range
interpret-range-auto = Auto — { $range }
interpret-raw-as-shot = As Shot (camera metadata)
interpret-debayer-bilinear = Bilinear (fast)
interpret-debayer-edge-aware = Edge Aware (high quality)
interpret-auto-unmarked = Auto — untagged
interpret-auto-identified = Auto — identified as { $space }
interpret-auto-suggested = Auto — suggested { $space } (diagnostic only)
interpret-manual-color-space = Manual — { $space }
interpret-auto-details = { $base } ({ $details })
interpret-detail-separator = ,
interpret-warning-count = { $count ->
    [one] { $count } warning
   *[other] { $count } warnings
}
interpret-confidence-none = No confidence
interpret-confidence-low = Low confidence
interpret-confidence-medium = Medium confidence
interpret-confidence-high = High confidence
interpret-method-metadata-hint = Metadata hint
interpret-method-icc-profile = ICC profile
interpret-method-cicp-tags = CICP tags
interpret-method-missing-metadata = No metadata
interpret-method-unsupported-cicp = Unsupported CICP tags
interpret-method-decoder-unavailable = Decoder unavailable
interpret-diagnostics-no-context = Input diagnostics: project color context unavailable
interpret-decision-explicit = Explicit user override
interpret-inference-declared = Declared/high confidence
interpret-inference-suggested = Suggestion only; not applied
interpret-inference-unknown = Unknown
interpret-decision-auto = Auto · { $method } · { $confidence } · { $inference }
interpret-decision-unprobed = Auto · not probed
interpret-none = None
interpret-processor-unresolved = Unavailable: input color space unresolved; Unknown is handled by the project missing-metadata policy
interpret-processor-error = Unavailable: { $error }
interpret-diagnostics = Detection: { $decision }
    { $signal }
    Evidence: { $evidence }
    Warnings: { $warnings }
    Path: { $path } → { $working } ({ $engine })
    OCIO processor cache ID: { $processor }
interpret-signal-missing = Range/Primaries/Transfer/Matrix: no video signal
interpret-camera-untagged = Untagged camera
interpret-signal-camera-raw = Camera RAW: { $adapter } · { $width }x{ $height } · { $bits }-bit · CFA { $cfa } · compression { $compression } · { $camera } · ColorMatrix={ $matrix } · AsShotNeutral={ $neutral }
interpret-signal-range-auto = { $detected } (auto/detected)
interpret-signal-range-full = Full (user override; detected { $detected })
interpret-signal-range-limited = Limited (user override; detected { $detected })
interpret-signal-no-metadata = Range: { $range } · Primaries/Transfer/Matrix: unavailable
interpret-signal-metadata = Range: { $range } · Primaries: { $primaries } · Transfer: { $transfer } · Matrix: { $matrix }
interpret-evidence-metadata-hint = { $scope } { $authority } metadata { $key }={ $value } → { $space }
interpret-evidence-exact-cicp = Exact CICP { $tags } → { $space }
interpret-evidence-partial-cicp = Partial CICP { $tags } → { $space }
interpret-evidence-unsupported-cicp = Unsupported CICP { $tags }
interpret-evidence-icc = ICC { $profile } → { $space }
interpret-unnamed = Unnamed
interpret-unmapped = Unmapped
interpret-warning-conflicting-hints = Conflicting metadata hints
interpret-warning-hint-overrides-cicp = Metadata hint overrides conflicting CICP tags
interpret-warning-descriptive-hint = Inferred from descriptive metadata only
interpret-warning-lower-priority-hints = Conflicting lower-priority hints ignored
interpret-warning-partial-cicp = Partial CICP tags only
interpret-warning-missing-cicp = Missing CICP tags
interpret-warning-unsupported-cicp = Unsupported or conflicting CICP tags
interpret-warning-icc-unmapped = ICC profile could not be mapped
interpret-warning-icc-cicp-mismatch = ICC and CICP conflict
interpret-more-items = { $count ->
    [one] { $count } more item
   *[other] { $count } more items
}
interpret-entry-separator = ;{ " " }
audio-mapping-standard-unavailable = Standard mapping unavailable: { $error }; playback and export will not guess
audio-mapping-source-unproven = Exact source layout cannot be proven from current dependencies; playback and export will not guess
audio-mapping-layout-mismatch = Explicit matrix requires { $required }, but current dependencies provide { $observed }; playback and export reject mismatched signals
audio-mapping-explicit-source-unproven = Explicit matrix retained, but the source layout cannot be proven; execution remains fail-closed
audio-mapping-mode-standard = Standard (automatic)
audio-mapping-mode-explicit = Custom (explicit matrix)
audio-mapping-standard-option = Standard (automatic; fail-closed at execution)
audio-mapping-snapshot-option = Custom: snapshot the current standard matrix
audio-mapping-blank-option = Custom: empty matrix
audio-mapping-channel-row = Channel mapping
audio-mapping-layout-row = Signal layout
audio-mapping-unknown-source = Unknown → { $destination }
audio-mapping-diagnostic-row = Mapping diagnostics
audio-mapping-add-to = Add to { $destination }
audio-mapping-choose-source = Choose source channel…
mixer-title = Audio Mixer
mixer-create-bus = Create Bus
mixer-create-named-bus = Create { $name }
mixer-routing-graph = Routing Graph
mixer-track = Track
mixer-bus = Bus
mixer-program-output = Program Output
mixer-mute = Mute
mixer-solo = Solo
mixer-level = Level
mixer-input-trim = Input Trim
mixer-fader = Fader
mixer-fader-curve = Fader Curve
mixer-automation-keyframes = Automated · { $count } keyframes
mixer-read-only = Read Only
mixer-incoming-routes = Incoming Routes
mixer-route-count = { $count } routes
mixer-name = Name
mixer-bus-name = Bus name
mixer-add-route = Add Route
mixer-choose-route = Choose tap and destination…
mixer-remove-bus = Remove Bus
mixer-remove-bus-routes = Remove Bus and { $count } routes
mixer-remove-blocked = Removal Blocked
mixer-enabled = Enabled
mixer-pass-signal = Pass signal
mixer-level-curve = Level Curve
mixer-controls = Controls
mixer-remove-route = Remove Route
mixer-remove-route-tooltip = Remove this Route or Send
mixer-rewire = Rewire
mixer-choose-new-route = Choose new tap and destination…
mixer-meter-not-running = Not running
mixer-meter-invalid = { " " }· Non-finite { $count }
audio-rack-install-clap = Install CLAP plugin…
audio-rack-install-vst3-bundle = Install VST3 bundle…
audio-rack-install-vst3-file = Install VST3 plugin file…
audio-rack-scope-row = Scope
audio-rack-add-row = Add
audio-rack-add-processor = Add processor…
audio-rack-scope-input-curve = Scope Input Curve
audio-rack-bypass = Bypass
audio-rack-plugin-binding = Plugin Binding
audio-rack-rebind-installed = Rebind installed version
audio-rack-nonnumeric-parameter = This parameter has no numeric editing contract
audio-rack-parameter-curve = { $name } Curve
audio-rack-move-up = Up
audio-rack-move-up-tooltip = Move processor up
audio-rack-move-down = Down
audio-rack-move-down-tooltip = Move processor down
audio-rack-remove = Remove
audio-rack-remove-tooltip = Remove processor

interpret-evidence-h264-range = H.264 sequence range: { $range } ({ $source })
interpret-evidence-codec-range = { $codec } standard sample range: { $range }
interpret-evidence-range-convention = { $codec }: { $rule }
interpret-range-sd-dv = SD DV BT.601 coefficients and video levels (format convention)
interpret-range-dnx-clv = Per-picture VC-3 CLV levels; out-of-band needs interpretation
interpret-range-legacy-assumption = Assumed video levels for untagged legacy SDR; override if incorrect
interpret-range-auto-dv = Auto (Video, SD DV convention)
interpret-range-auto-dnx = Auto (per-picture VC-3 CLV)
interpret-range-auto-assumed = Auto (assumed Video; source untagged)
asset-thumbnail-alternate = First-frame preview
interpret-range-codec-default = Annex E default; no color-space inference
interpret-range-coded = encoded video_full_range_flag

# Preferences dialog
preferences-appearance = Appearance
preferences-workspace = Workspace
preferences-project = Project
preferences-preview = Preview
preferences-audio = Audio
preferences-export-draft = Export draft
preferences-diagnostics = Diagnostics
preferences-runtime = Runtime
preferences-monitor-output = Monitor output
preferences-icc-calibration = ICC device calibration
preferences-structured-diagnostics = Display diagnostics
preferences-waiting-display = Waiting for monitor and surface details
preferences-device-label = Output device
preferences-monitor-label = Monitor target
preferences-hdr-label = HDR policy
preferences-profile-label = Profile
preferences-intent-label = Rendering intent
preferences-theme-label = Theme
preferences-waveform-label = Waveform
preferences-canvas-label = Canvas background
preferences-close = Close
preferences-select-icc = Choose ICC…
preferences-waveform-rectified = Rectified
preferences-waveform-full = Full
preferences-canvas-checkerboard = Checkerboard
preferences-canvas-black = Black
preferences-reset = Reset
preferences-disable = Disable
preferences-search = Search commands or shortcuts…
preferences-unbound = Unbound
preferences-capture = Press shortcut
preferences-replace = Replace
preferences-cancel = Cancel
preferences-status-disabled = Disabled
preferences-status-conflict = Conflict
preferences-status-custom = Custom
preferences-tab-general = General
preferences-tab-media = Media
preferences-tab-display = Display
preferences-tab-shortcuts = Shortcuts
preferences-tab-developer = Developer
preferences-category-file = File
preferences-category-edit = Edit
preferences-category-view = View
preferences-category-window = Window
preferences-category-workspace = Workspace
preferences-category-timeline = Timeline
preferences-category-playback = Playback
preferences-category-help = Help
preferences-system = Follow system
preferences-dark = Dark
preferences-light = Light
preferences-enabled = Enabled
preferences-disabled = Disabled
preferences-no-project = No project open
preferences-no-sequence = No active sequence
preferences-not-selected = Not selected
preferences-tracing = Tracing enabled
preferences-range-entire = Entire sequence
preferences-range-in-out = Sequence In/Out
preferences-range-work = Work area
preferences-shared = Shared
preferences-prefer-exclusive = Prefer exclusive, allow shared fallback
preferences-require-exclusive = Require exclusive
preferences-system-default = System default
preferences-device-unavailable = Selected device unavailable
preferences-device-loading = Checking selected device…
preferences-device-failed = Cannot check selected device
preferences-follow-program = Follow Program Output
preferences-calibration-disabled = Disabled
preferences-os-default = OS default
preferences-perceptual = Perceptual
preferences-relative = Relative
preferences-saturation = Saturation
preferences-absolute = Absolute
preferences-follow-output = Follow output
preferences-refresh-devices = Refresh devices
preferences-discover-devices = Discovering output devices…
preferences-theme = Theme: { $value }
preferences-waveform = Waveform: { $value }
preferences-canvas = Canvas background: { $value }
preferences-current-workspace = Current workspace: { $value }
preferences-current-project = Project: { $value }
preferences-current-sequence = Sequence: { $value }
preferences-auto-proxy = Automatic proxies: { $value }
preferences-clock = Clock: { $value }
preferences-sample-rate = Sample rate: { $value }
preferences-range = Range: { $value }
preferences-output = Output: { $value }
preferences-runtime-diagnostics = Runtime diagnostics: { $value }
preferences-log-filter = Log filter: { $value }
preferences-workers = Background workers: { $value }
preferences-engine = Color engine: { $value }
preferences-confirm-binding = Confirm binding: { $binding }
preferences-binding-conflict = Used by { $owner } ({ $binding })
preferences-system-default-device = System default — { $name }
preferences-monitor-unavailable = Unavailable · { $display } / { $view }
preferences-current-profile = Current file · { $value }
preferences-default-shared = Follow system default · Shared
preferences-default-prefer-exclusive = Follow system default · Prefer exclusive, allow shared fallback
preferences-default-require-exclusive = Follow system default · Require exclusive
preferences-device-discovery-failed = Device discovery failed: { $detail }
preferences-device-no-identity = { $name } (identity unavailable)
preferences-display-status = Status: { $status } · Monitor: { $display } · Platform: { $platform }
preferences-display-output = Output: { $requested } → { $resolved } · Viewer: { $viewer }
preferences-warnings = Warnings
preferences-blockers = Blockers
command-app-quit = Quit
command-edit-ripple_delete_selection = Ripple Delete Selection
command-timeline-split_at_playhead = Split at Playhead
command-timeline-mark_in = Mark In
command-timeline-mark_out = Mark Out
command-transport-go_to_start = Go to Start
command-transport-shuttle_reverse = Shuttle Reverse
command-transport-shuttle_stop = Stop Shuttle
command-transport-shuttle_forward = Shuttle Forward
command-transport-go_to_end = Go to End
command-transport-step_back = Step Back
command-transport-step_forward = Step Forward
command-panel-assets = Assets Panel
command-panel-viewer = Viewer Panel
command-panel-timeline = Timeline Panel
command-panel-inspector = Inspector Panel
command-panel-effects = Effects Panel
command-panel-node_graph = Node Graph Panel
command-panel-export = Export Panel
mask-section = Power Windows
mask-add = Add
mask-shape-rectangle = Rectangle
mask-shape-ellipse = Ellipse
mask-shape-path = Path
mask-shape-bezier = Bézier
mask-controls = Controls
mask-enabled = Enabled
mask-move-up = Up
mask-move-up-tooltip = Move mask up
mask-move-down = Down
mask-move-down-tooltip = Move mask down
mask-remove = Remove
mask-remove-tooltip = Remove mask
mask-lock = Lock
mask-lock-editing = Lock editing
mask-shape = Shape
mask-shape-animation = Shape animation
mask-keyframes = Keyframes
mask-insert-point-hint = Alt-click a curve to insert a control point
mask-track-object-forward = Object · Forward
mask-track-object-backward = Object · Backward
mask-track-object-both = Object · Both directions
mask-track-planar-forward = Plane · Forward
mask-track-planar-backward = Plane · Backward
mask-track-planar-both = Plane · Both directions
mask-tracking = Tracking
mask-select-tracking = Choose model and direction…
mask-tracking-status = Tracking status
mask-tracking-controls = Tracking controls
mask-cancel = Cancel
mask-recompute = Recompute
mask-status-generated = Generated · Ready to recompute
mask-status-untracked = Not tracked
mask-status-queued = Queued
mask-status-analyzing = Analyzing { $completed }/{ $total }
mask-status-canceling = Canceling
mask-status-canceled = Canceled
mask-status-completed = Complete · Ready to recompute
mask-status-cache-hit = Cached result applied
mask-status-stale = Result is stale
mask-status-stale-detail = Stale · { $detail }
mask-status-failed = Failed
mask-status-failed-detail = Failed · { $detail }
inspector-title = Inspector
inspector-selected-clip = Selected clip
inspector-no-clip-selected = No clip selected
inspector-clip-style = Clip Style
inspector-enabled = Enabled
inspector-enable-effect = Enable effect
inspector-opacity = Opacity
inspector-color = Color
inspector-basic-title = Basic Title
inspector-transform = Transform
inspector-position-x = Position X (px)
inspector-position-y = Position Y (px)
inspector-scale-x = Scale X (%)
inspector-scale-y = Scale Y (%)
inspector-anchor-x = Anchor X (px)
inspector-anchor-y = Anchor Y (px)
inspector-rotation = Rotation (°)
inspector-timing = Timing
inspector-speed = Speed (%)
inspector-source-time = Source time
inspector-hold = Hold
inspector-resume-speed = Resume speed (%)
inspector-place-playhead-in-clip = Place the playhead inside the clip first
inspector-update-hold-frame = Update to playhead frame
inspector-create-hold-frame = Freeze at playhead
inspector-freeze-frame = Freeze frame
inspector-audio-component = Audio Component
inspector-audio-component-numbered = Audio Component { $number }
inspector-audio-in-mix = Include in mix
inspector-audio-volume = Volume (dB)
inspector-audio-pan = Pan / Balance
inspector-audio-volume-curve = Volume curve
inspector-audio-pan-curve = Pan curve
inspector-audio-logical-source = Logical source
inspector-audio-reprobe-file = Reprobe current file…
inspector-audio-stream-binding = Asset stream binding
inspector-audio-fade-in = Fade in (s)
inspector-audio-fade-in-curve = Fade in curve
inspector-audio-fade-out = Fade out (s)
inspector-audio-fade-out-curve = Fade out curve
inspector-audio-fade-constant-gain = Constant Gain
inspector-audio-fade-equal-power = Equal Power
inspector-grade-graph = Grade Graph
inspector-grade-clip = Clip
inspector-grade-create = Create Clip Grade
inspector-grade-group = Group
inspector-grade-pre-post = Group Pre/Post
inspector-grade-identity = Identity
inspector-grade-timeline = Timeline
inspector-grade-version-nodes = Version / Nodes
inspector-grade-summary = { $version } · { $versions } versions · { $nodes } nodes
inspector-grade-add-node = Add Node
inspector-grade-add-node-placeholder = Add grade node…
inspector-effect-controls = Controls
inspector-effect-move-up = Up
inspector-effect-move-up-tooltip = Move effect up
inspector-effect-move-down = Down
inspector-effect-move-down-tooltip = Move effect down
inspector-effect-remove = Remove
inspector-effect-remove-tooltip = Remove effect
inspector-effect-curve = { $name } curve
inspector-animation = Animation
inspector-curve = Curve
node-graph-title = Node Graph
node-graph-select-clip = Select a clip to inspect its render chain
node-graph-source = Source
node-graph-output = Output
node-graph-composite = Composite
node-graph-adjustment = Adjustment
node-graph-nested-sequence = Nested Sequence
node-graph-solid-color = Solid Color
node-graph-media-clip = Media Clip
node-graph-clip-effects = { $clip } / { $count } effects
export-no-sequence = No sequence
export-hdr-select-sequence = Select a sequence to configure Dynamic HDR delivery
export-anc-select-sequence = Select a sequence before attaching an ANC frame range
export-status-select-sequence = Open or select a sequence before exporting
export-status-no-presets = No export presets available
export-status-incompatible = Incompatible delivery settings: { $error }
export-status-anc-failed = ANC preflight failed: { $error }
export-status-error = Error: { $error }
export-status-select-output = Choose an output path to add this job to the queue
export-status-ready = Ready
export-hdr-omit = Omit Dynamic HDR
export-hdr-preserve = Preserve { $family } byte for byte
export-hdr-remake = Remake: { $program }
export-hdr-missing-program = Remake: missing Program
export-hdr-omit-readiness = Dynamic metadata is explicitly omitted; the sequence delivery policy controls static HDR independently
export-hdr-preserve-readiness = Only a complete source-file copy with byte-for-byte preservation and a new probe of { $family } is allowed; edits, remuxing, and rendering do not fall back
export-hdr-remake-readiness = { $standard } / { $shots } shots; requires qualified, authorized generation, independent verification, and human HDR/SDR QC adapters
export-hdr-missing-readiness = The referenced Dynamic HDR Program is missing; delivery is blocked
export-hdr-preserve-st2094 = Preserve ST 2094-40 App #4 byte for byte
export-hdr-preserve-dolby = Preserve Dolby Vision metadata byte for byte
export-hdr-open-sequence = { $readiness }; open this sequence to change its delivery intent
export-job-pending = Pending
export-job-rendering-progress = Rendering { $completed }/{ $total }
export-job-cancelling = Cancelling
export-job-completed = Completed
export-job-failed = Failed: { $detail }
export-job-cancelled = Cancelled
export-job-preparing = Preparing
export-job-rendering = Rendering
export-job-encoding = Encoding
export-job-packaging = Packaging
export-job-validating = Validating
export-job-publishing = Publishing
export-preset-modified = (modified)
export-image-sequence-directory = Image sequence directory
export-no-video = No video
export-follow-sequence-output = Follow sequence Program Output
export-legal-rgb-range = Clamp to legal RGB signal range
export-no-audio-image-sequence = No audio (image sequence)
export-delivery-title = Delivery title
export-issuer = Issuer
export-creator-system = Creating system
export-language = RFC 5646 language
export-browse = Browse…
export-output-path = Output path
export-add-to-queue = Add to queue
export-import-anc = Import ANC / captions…
export-remove = Remove
export-anc-none = No ANC / broadcast captions selected
export-import-pse = Import PSE configuration…
export-pse-none = No independent regulatory analyzer selected
export-clear-finished = Clear finished
export-no-exportable-sequence = No exportable sequence
export-format = Format
export-container = Container
export-video-codec = Video codec
export-resolution = Resolution
export-frame-rate = Frame rate
export-width = Width
export-height = Height
export-video-signal = Video signal
export-bit-depth = Bit depth
export-range = Range
export-chroma = Chroma sampling
export-color-output = Color output
export-processing-mode = Processing mode
export-target-space = Target space
export-encoding-parameters = Encoding parameters
export-max-bitrate = Max bitrate (kbps)
export-closed-gop = Closed GOP
export-scene-cut-keyframes = Insert keyframes at scene cuts
export-keyframe-interval = Keyframe interval (seconds)
export-max-b-frames = Maximum consecutive B frames
export-scene-cut = Scene cut
export-lookahead-frames = Lookahead frames
export-palette-dither = Enable palette dithering
export-palette-colors = Palette colors
export-dither = Dither
export-controls = Controls
export-fixed-profile = Determined by the qualified fixed profile
export-audio = Audio
export-codec = Codec
export-audio-bitrate = Bitrate (kbps)
export-sample-depth = Sample depth
export-queue = Queue
export-jobs = Jobs
export-no-jobs = No queued jobs
export-queued-jobs = { $count } queued { $count ->
    [one] job
   *[other] jobs
}
export-preset = Preset
export-details = Details
export-delivery-metadata = Delivery metadata
export-title = Title
export-creator = Creator
export-language-short = Language
export-broadcast-anc = Broadcast ANC
export-select = Select
export-source = Source
export-frame-range = Frame range
export-frame-rate-packets = Frame rate / packets
export-qualification = Qualification
export-regulatory-pse = Regulatory PSE
export-analyzer = Analyzer
export-status = Status
export-dynamic-hdr-delivery = Dynamic HDR delivery
export-intent = Intent
export-hdr-program-count = { $count } analyzed { $count ->
    [one] Program
   *[other] Programs
}
export-qualification-status = Qualification status
export-input = Input
export-sequence = Sequence
export-summary = Summary
export-output = Output
export-path = Path
export-cancel = Cancel
export-job = Job
export-anc-frame-range = 0…{ $last } · { $count } frames · source start { $start }
export-anc-cadence = { $rate } fps · { $packets } packets
export-anc-source-scc = SCC V1.0 · 608 Field 1
export-anc-source-cdp = Raw CDP · ST334-2:2015
export-anc-caption-readiness = { $format } · importer v{ $version } · 608 channel bitmap { $channels } · 708 services { $services }. Transport checks passed; publish after reading the delivered file back. Caption display and SDI hardware are not qualified.
export-anc-structure-readiness = ST436 structure and export range verified; publish after reading the delivered file back. SDI hardware is not qualified.
export-anc-guidance = AS-11: ANC JSON, SCC V1.0 (29.97→59.94 / Field 1), or raw CDP; up to 8 MiB / 100000 caption frames on VANC line 20. Align to the sequence start timecode and export range.
export-pse-configured = Configuration frozen; verify analyzer identity on queueing and rescan the delivered file before determining the regulatory result
export-pse-guidance = Import a JSON configuration for the provider and broadcast QC profile
export-sequence-summary = Timeline render (V{ $video } / A{ $audio })
export-follow-sequence = Follow sequence
export-no-audio = No audio
export-image-png8 = PNG 8-bit (lossless)
export-image-png16 = PNG 16-bit (lossless)
export-image-exr-half = OpenEXR Half (ZIP16)
export-image-exr-float = OpenEXR Float32 (ZIP16)
export-image-dpx16 = DPX 16-bit RGB
export-image-tiff16 = TIFF 16-bit (Deflate)
export-image-tiff-float = TIFF Float32 (lossless)
export-alpha-flatten-black = Composite over black
export-alpha-preserve = Preserve straight alpha
export-color-follow-sequence = Follow sequence Program Output
export-color-rendering-view = Project engine Rendering View
export-color-colorimetric = Direct colorimetric transform
export-range-in-out = Sequence In/Out
export-range-entire = Entire sequence
export-range-work-area = Work area
export-directory-package = directory package
export-op1a-mxf-file = OP1a MXF file
export-summary-professional = { $resolution } / { $profile } / { $essence } / { $frame_rate } / PCM 24-bit 48 kHz stereo / { $layout } / .{ $extension }
export-summary-audio-stems = Program Output stems / PCM 24-bit WAV / one file per output
export-summary-image-sequence = { $resolution } / { $format } / { $color } / { $alpha } / { $frame_rate } / .{ $extension } directory
export-summary-media = { $resolution } / { $codec } / { $bitrate } / { $color } / { $bit_depth } / { $range } / { $chroma } / { $alpha } / { $audio } / .{ $extension }
export-fixed-profile-class = Fixed Profile/Class representation
export-on = on
export-off = off
export-gif-rate-control = { $colors } colors / dither { $dither }
export-rate-control-max = CRF { $crf } / max { $max } kbps
export-rate-control = CRF { $crf }

# Export diagnostic summaries retain provider codes as values.
export-diagnostic-none = None
export-diagnostic-pass = Passed
export-diagnostic-warn = Warning
export-diagnostic-fail = Failed
export-diagnostic-complete = Complete
export-diagnostic-incomplete = Incomplete
export-diagnostic-warning-code = Warning: { $code }
export-broadcast-qc-summary = Broadcast QC: { $profile }@{ $edition } / { $verdict } / { $frames } frames ({ $coverage }) / findings { $findings }, retained { $retained }, overflow { $overflow } / obligations { $obligations } / { $complete }
export-color-assets-summary = assets { $count } / issues { $issues }
export-color-health-summary = Color: { $verdict } / { $frames } frames / metadata { $metadata } / overrides { $overrides } / policy { $policy } / data { $data } / rejected { $rejected } / { $assets } / checks { $checks } / causes { $causes } / actions { $actions } / stages CPU input { $cpu_in }, CPU output { $cpu_out }, GPU { $gpu }, blockers { $blockers }, transfer { $transfer } / GPU blockers shader { $shader }, resource { $resource }, wrapper { $wrapper }, pipeline { $pipeline } / composites float { $float }, legacy { $legacy }, reasons { $reasons }

# Runtime status bar and retained notification history
status-ready = Ready
status-history-title = Recent notifications · ↑/↓ · PgUp/PgDn · Home/End · Esc
status-history-empty = No notifications
status-history-accessibility = Recent notifications. Current status: { $status }
status-export-pending = Export queued ({ $count })
status-export-frames = Exporting frame { $completed }/{ $total } ({ $count } queued)
status-export-phase = Export: { $phase } ({ $count } queued)
status-export-cancelling = Cancelling export ({ $count } queued)
status-export-active = Export in progress

# Runtime status messages
status-saving = Saving project…
status-open-preparing = Opening and validating project…
status-open-cancel-closing = Cancelling project open and safely closing project…
status-open-failed = Could not open project: { $reason }
status-opened = Project opened
status-save-as-failed = Could not start Save As: { $reason }
status-save-as-started = Saving project as { $path }…
status-create-project-failed = Could not create project: { $reason }
status-created-project = Project created: { $path }
status-recovery-failed = Could not recover autosave: { $reason }
status-recovered = Recovered autosave: { $path }
status-import-failed = Import failed: { $reason }
status-relink-failed = Could not relink asset: { $reason }
status-relink-analyzing = Analyzing relinked media…
status-audio-probe-failed = Could not probe audio components: { $reason }
status-audio-probe-started = Refreshing available audio streams…
status-audio-rebind-failed = Could not rebind audio component: { $reason }
status-audio-rebind-started = Validating audio component binding…
status-rename-asset-failed = Could not rename asset: { $reason }
status-renamed-asset = Asset renamed: { $name }
status-interpret-failed = Could not interpret asset: { $reason }
status-interpreted = Asset interpretation updated: { $asset_name }
status-rename-folder-failed = Could not rename folder: { $reason }
status-renamed-folder = Folder renamed: { $name }
status-proxy-mode-failed = Could not set proxy mode: { $reason }
status-proxy-enable-failed = Could not enable proxy: { $reason }
status-proxy-enabled = Proxy mode enabled: { $name }
status-proxy-disabled = Proxy mode disabled: { $name }
status-remove-assets-failed = Could not remove library selection: { $reason }
status-removed-asset = Asset removed from library: { $name }
status-removed-folder = Folder deleted: { $name }
status-removed-selection = Removed { $assets } assets and deleted { $folders } folders
status-move-assets-failed = Could not move library selection: { $reason }
status-moved-asset = Asset moved: { $name } → { $target_name }
status-moved-folder = Folder moved: { $name } → { $target_name }
status-moved-selection = Moved { $assets } assets and { $folders } folders → { $target_name }
status-cross-dissolve-created = Cross dissolve created
status-cross-dissolve-failed = Could not create cross dissolve: { $error }
status-transition-update-failed = Could not update video transition: { $error }
status-preview-resolution = Preview resolution: { $scale }
status-asset-prepare-failed = Could not prepare asset: { $reason }
status-drag-ready = Ready to drag { $name } (drop onto { $lane })
status-clap-installed = CLAP processors installed: { $count }
status-vst3-installed = VST3 processors installed: { $count }
status-title-created = Basic title created
status-export-failed = Export failed: { $reason }
status-export-queued = Added to export queue
status-export-complete = Export completed: { $path }
status-export-canceled = Export canceled
status-relinked = Asset relinked: { $name } → { $path }
status-audio-streams-refreshed = Audio streams refreshed: { $name }
status-audio-rebound = Audio component of { $name } mapped to stream #{ $stream }
status-proxy-failed = Proxy generation failed: { $detail }
status-proxy-switch-failed = Could not switch proxy mode: { $error }
status-proxy-binding-failed = Proxy mode changed, but playback binding failed: { $error }
status-native-audio-unavailable = Native audio host unavailable; saved plugins could not be restored
status-native-audio-restore-failed = Could not start native audio plugin restore: { $error }
status-audio-restored-partial = Audio plugins restored: { $available } libraries available, { $failed } failed; select failed plugin files to retry
status-audio-restored = Audio plugin libraries restored: { $count }
status-openfx-scanning = Scanning the selected OpenFX effects…
status-openfx-restore-failed = Could not start OpenFX restore: { $error }
status-openfx-restore-queue-failed = Could not queue OpenFX restore
status-openfx-installed = OpenFX effects installed: { $installed }
status-openfx-installed-partial = Installed { $installed } OpenFX effects; { $unsupported } plugins do not support the selected type or current host contract
status-openfx-install-failed = OpenFX installation failed: { $reason }
status-openfx-restored-partial = OpenFX restored: { $available } filters available, { $failed } failed; select installation files to retry
status-openfx-restored = OpenFX effects restored: { $count }
status-openfx-scan-failed = OpenFX scan stopped unexpectedly; select the plugin again
status-audio-prepare-failed = Could not prepare audio processing: { $error }
status-persistence-failed = Could not finish project persistence: { $error }
status-closing = Finishing save and safely closing project…
status-close-failed = Could not close project: { $reason }
status-close-save-canceled = Close after save canceled: { $reason }
status-saved = Project saved
status-saved-cleanup-failed = Project saved; recovery cleanup failed: { $reason }
status-autosaved-cleanup-failed = Autosave completed; recovery cleanup failed: { $reason }
status-save-failed = Could not save project: { $error }
status-autosave-failed = Autosave failed: { $error }
status-autosave-start-failed = Could not start autosave: { $error }
status-packaging = Packaging project…
status-package-canceling = Canceling project packaging…
status-packaged = Project packaged: { $path }
status-package-canceled = Project packaging canceled
status-package-failed = Project packaging failed: { $reason }
status-package-worker-failed = Project packaging worker stopped unexpectedly
status-package-copying = Copying project resources… { $percent }%
status-package-validating = Validating project resources… { $percent }%
status-package-writing = Writing project snapshot…
status-package-publishing = Publishing project package…
status-package-analyzing = Analyzing project dependencies…
status-drop-probing = Probing media dropped onto track…
status-drop-placed = File imported into library and placed on track
status-placement-failed = Could not place asset: { $error }
status-asset-placed = Asset added to timeline
status-history-retired = Edit committed without undo history; earlier history was cleared to avoid crossing an unrecorded edit
status-folder-created = Folder created: { $name }
status-asset-created = Asset created: { $name }
status-adjustment-created = Adjustment layer created: { $name }
status-solid-created = Solid color layer created: { $name }
status-precomposed = Nested sequence created: “{ $name }”
status-clips-linked = Selected clips linked
status-clips-unlinked = Selected clips unlinked
status-cross-dissolve-shortened = Cross dissolve shortened to available source handles
status-transition-shortened = Video transition shortened to available source handles
status-tracking-failed = Tracking failed: { $detail }
status-tracking-cached = Tracking cache applied
status-tracking-complete = Tracking completed
status-plugin-preferences-failed = Plugin installed; restart recovery information could not be saved: { $error }
status-reference-release-failed = Project closed; reference output device release could not be confirmed: { $reason }
status-shell-action-failed = UI action failed: { $err }
status-gallery-no-viewer = No project Viewer available to capture
status-gallery-captured = Gallery still captured
status-gallery-capture-failed = Could not capture gallery still: { $error }
status-gallery-preparing = Preparing exact gallery still…
status-workspace-save-failed = Could not save workspace preference: { $err }
status-startup-action-failed = Startup action failed: { $err }
status-action-failed = Action failed: { $err }
status-recent-save-failed = Could not save recent projects: { $err }
status-shortcut-preference-invalid = Shortcut preference is no longer valid
status-shortcut-key-invalid = Shortcut key is no longer valid
status-preferences-save-failed = Could not save preferences: { $err }
status-preferences-action-failed = Preferences action failed: { $err }
status-asset-browser-action-failed = Asset browser action failed: { $err }
status-close-abandon-failed = Could not abandon failed project close: { $err }
status-close-start-failed = Could not safely close project: { $err }
status-import-started =
    { $count ->
        [one] Importing one media file…
       *[other] Importing { $count } media files…
    }
status-import-canceled = Media import canceled
status-import-progress = Importing media file { $completed }/{ $total }…
status-drop-canceled = File placement canceled
status-drop-failed = File placement failed: { $reason }

status-proxy-generating = Proxy mode enabled: { $name } (generating in background)
status-proxy-regenerating = Proxy mode enabled: { $name } (stale proxy; regenerating in background)
status-audio-track = audio track
status-video-track = video track
status-import-summary =
    { $imported ->
        [one] Imported one media file
       *[other] Imported { $imported } media files
    }
status-import-summary-proxies =
    { status-import-summary }; { $proxies ->
        [one] generating one proxy in background
       *[other] generating { $proxies } proxies in background
    }
status-import-summary-partial =
    { status-import-summary }; { $failed ->
        [one] one failed
       *[other] { $failed } failed
    }
status-import-summary-proxies-partial =
    { status-import-summary-proxies }; { $failed ->
        [one] one failed
       *[other] { $failed } failed
    }
status-range-lifted = Lift complete: { $removed } clips removed, { $trimmed } trimmed, { $split } split, { $shifted } moved
status-range-extracted = Extract complete: { $removed } clips removed, { $trimmed } trimmed, { $split } split, { $shifted } moved
status-opaque-detail = { $detail }

status-no-project = No project

# Typed core failure projection
error-media-open = Could not open media: { $path } — { $reason }
error-decode-failed = Decode failed (asset { $asset_id }): { $reason }
error-unsupported-format = Unsupported media format: { $format }
error-proxy-generation = Proxy generation failed: { $reason }
error-track-not-found = Track not found: { $track_id }
error-clip-not-found = Clip not found: { $clip_id }
error-time-out-of-range = Time position out of range: { $timecode }
error-track-locked = Track is locked; editing is unavailable: { $track_id }
error-action-not-executed = Action was not executed ({ $action }): { $reason }
error-gpu-init = GPU initialization failed: { $reason }
error-shader-compile = Shader compilation failed ({ $shader_name }): { $reason }
error-texture-upload = Texture upload failed: { $reason }
error-effect-evaluation = Effect graph evaluation failed: { $reason }
error-asset-not-found = Asset not found: { $asset_id }
error-asset-file-missing = Asset file moved or deleted; relink the media: { $path }
error-asset-database = Asset library database error: { $reason }
error-ai-provider = AI provider failed ({ $provider }): { $reason }
error-ai-rate-limit = AI rate limit reached; retry later ({ $provider })
error-workflow-step = Workflow step failed ({ $step_id }): { $reason }
error-workflow-parse = Workflow parsing failed: { $reason }
error-export = Export encoding failed: { $reason }
error-hardware-encoder = Hardware encoder unavailable: { $encoder }
error-source-revision-unavailable = Could not verify media file revision: { $path } — { $reason }
error-source-revision-changed = Media file changed; refresh or relink the source: { $path }
error-media-resource = Media execution resource unavailable ({ $operation }): { $source }
error-media-resource-cleanup = Media execution resource unavailable ({ $operation }): { $source }; cleanup failed: { $cleanup }
error-decode-timeout = Decode timed out (asset { $asset_id }, mode { $access_mode }, frame { $frame }, source { $secs } s, budget { $budget_ms } ms)
error-decode-budget = Decode scan budget exhausted (asset { $asset_id }, mode { $access_mode }, decoded { $decoded_frames } frames, budget { $budget_frames } frames, target PTS { $target_pts })
error-decode-temporal = Decoded frame does not cover requested time (asset { $asset_id }, mode { $access_mode }, requested PTS { $requested_pts }, frame PTS { $selected_pts }, duration { $selected_duration_pts })
error-timeline-time = Invalid exact timeline time: { $reason }
error-io = File I/O failed: { $reason }
error-serialization = Data serialization failed: { $reason }
error-canceled = Operation canceled
error-other = Operation failed: { $reason }
error-timestamp-unknown = not supplied

error-context = { $context }: { $source }

# Product admission and exact-time reasons
reason-no-project-save-as = No project is available for Save As
reason-empty-media-path = Choose a media file; the path is empty
reason-library-disconnected = The project asset library is not connected
reason-proxy-video-only = Proxy mode is available only for video assets
reason-proxy-source-missing = Source media is missing; relink it before enabling proxies
reason-proxy-project-disabled = Proxy workflow is disabled; enable it in Project Settings first
reason-probe-evidence-missing = Media probe information is missing or stale; refresh or relink the asset
reason-probe-duration-missing = A valid media duration could not be established; the clip cannot be placed
reason-time-zero-denominator = Time denominator must not be zero
reason-time-overflow = Time arithmetic exceeds the supported range
reason-time-invalid-base = Invalid frame time base: { $numerator }/{ $denominator }
reason-time-invalid-rate = Invalid frame rate: { $numerator }/{ $denominator }
reason-time-domain-mismatch = Times belong to different domains; an explicit time mapping is required
reason-time-non-invertible = Time mapping is not invertible
reason-time-negative-duration = Time range duration must not be negative
reason-time-non-finite = Time input must be finite

reason-import-folder-missing = The destination asset folder does not exist: { $folder }
reason-import-project-unavailable = Open a project before importing media
reason-import-worker-unavailable = Media import workers are unavailable; restart the application and try again
reason-import-empty-batch = Choose at least one media file
reason-import-batch-capacity = Too many import batches are queued (limit: { $capacity }); wait for an import to finish or cancel one
reason-import-batch-too-large = This import contains { $files } files; select no more than { $capacity } files per batch
reason-import-file-capacity = This import needs { $files } file slots, but only { $available } are available; wait for an import to finish or select fewer files

inspector-track-locked = The selected clip's track is locked
inspector-audio-asset-unavailable = Component { $identity } (asset unavailable)
inspector-audio-component-missing = Component { $identity } (missing from catalog)
inspector-audio-output-unavailable = Nested output { $identity } (unavailable)
inspector-audio-primary = Primary · { $label }
inspector-audio-stream-rebind = Stream #{ $index } · { $layout } (rebind required)
inspector-audio-stream-rebind-short = Stream #{ $index } (rebind required)
inspector-audio-binding-unavailable = No valid physical mapping
inspector-audio-stream = Stream #{ $index }
inspector-audio-stream-default = Default
inspector-audio-layout-unspecified = Unspecified { $count }ch
inspector-audio-layout-unsupported = Unsupported named layout { $count }ch

# Stable parameter presentation; author keys remain language-neutral.
parameter-mondrian-title-text-label = Text
parameter-mondrian-title-font-family-label = Font family
parameter-mondrian-title-font-weight-label = Font weight
parameter-mondrian-title-font-style-label = Font style
parameter-mondrian-title-font-size-label = Font size
parameter-mondrian-title-fill-label = Fill
parameter-mondrian-title-tracking-label = Tracking
parameter-mondrian-title-line-height-label = Line height
parameter-mondrian-title-horizontal-align-label = Horizontal alignment
parameter-mondrian-title-vertical-align-label = Vertical alignment
parameter-mondrian-mask-feather-label = Feather
parameter-mondrian-mask-opacity-label = Opacity
parameter-mondrian-mask-expansion-label = Expansion
parameter-mondrian-mask-invert-label = Invert
parameter-mondrian-mask-operation-label = Operation
parameter-mondrian-title-option-normal = Regular
parameter-mondrian-title-option-italic = Italic
parameter-mondrian-title-option-oblique = Oblique
parameter-mondrian-title-option-left = Left
parameter-mondrian-title-option-center = Center
parameter-mondrian-title-option-right = Right
parameter-mondrian-title-option-top = Top
parameter-mondrian-title-option-bottom = Bottom
parameter-mondrian-mask-operation-Add-label = Add
parameter-mondrian-mask-operation-Subtract-label = Subtract
parameter-mondrian-mask-operation-Intersect-label = Intersect
parameter-mondrian-mask-operation-Difference-label = Difference
parameter-mondrian-effect-curves-mode-rgb = RGB
parameter-mondrian-effect-curves-mode-yrgb = YRGB
parameter-mondrian-effect-qualifier-mode-hsl = HSL
parameter-builtin-lut-3d-processing-space-unassigned = Unassigned
parameter-builtin-lut-3d-processing-space-scene-linear = Scene linear
parameter-builtin-lut-3d-processing-space-rec709 = Rec. 709
parameter-builtin-lut-3d-processing-space-srgb = sRGB
parameter-builtin-lut-3d-processing-space-rec2020 = Rec. 2020
parameter-builtin-lut-3d-processing-space-display-p3 = Display P3
parameter-builtin-lut-3d-processing-space-rec2100-hlg = Rec. 2100 HLG
parameter-builtin-lut-3d-processing-space-rec2100-pq = Rec. 2100 PQ
parameter-builtin-lut-3d-processing-space-aces-cct = ACEScct
parameter-builtin-lut-3d-processing-space-apple-log-bt2020 = Apple Log / BT.2020
parameter-builtin-lut-3d-processing-space-sony-slog2-sgamut = Sony S-Log2 / S-Gamut
parameter-builtin-lut-3d-processing-space-sony-slog3-sgamut3 = Sony S-Log3 / S-Gamut3
parameter-builtin-lut-3d-processing-space-sony-slog3-sgamut3-cine = Sony S-Log3 / S-Gamut3.Cine
parameter-builtin-lut-3d-processing-space-arri-logc3-wide-gamut3 = ARRI LogC3 / Wide Gamut 3
parameter-builtin-lut-3d-processing-space-arri-logc4-wide-gamut4 = ARRI LogC4 / Wide Gamut 4
parameter-builtin-lut-3d-processing-space-canon-log2-cinema-gamut-d55 = Canon Log 2 / Cinema Gamut D55
parameter-builtin-lut-3d-processing-space-canon-log3-cinema-gamut-d55 = Canon Log 3 / Cinema Gamut D55
parameter-builtin-lut-3d-processing-space-panasonic-vlog-vgamut = Panasonic V-Log / V-Gamut
parameter-builtin-lut-3d-processing-space-red-log3g10-wide-gamut-rgb = RED Log3G10 / Wide Gamut RGB
parameter-builtin-lut-3d-processing-space-blackmagic-film-wide-gamut-gen5 = Blackmagic Film / Wide Gamut Gen 5
parameter-builtin-lut-3d-processing-space-dji-dlog-dgamut = DJI D-Log / D-Gamut
parameter-builtin-lut-3d-processing-space-davinci-intermediate-wide-gamut = DaVinci Intermediate / Wide Gamut
parameter-mondrian-effect-builtin-asc-cdl-offset-label = Offset
parameter-mondrian-effect-builtin-asc-cdl-power-label = Power
parameter-mondrian-effect-builtin-asc-cdl-saturation-label = Saturation
parameter-mondrian-effect-builtin-asc-cdl-slope-label = Slope
parameter-mondrian-effect-builtin-basic-correction-contrast-label = Contrast
parameter-mondrian-effect-builtin-basic-correction-exposure-label = Exposure
parameter-mondrian-effect-builtin-basic-correction-saturation-label = Saturation
parameter-mondrian-effect-builtin-chroma-key-blend-label = Edge blend
parameter-mondrian-effect-builtin-chroma-key-invert-label = Keep key color
parameter-mondrian-effect-builtin-chroma-key-key-color-label = Key color
parameter-mondrian-effect-builtin-chroma-key-similarity-label = Similarity
parameter-mondrian-effect-builtin-chromatic-aberration-amount-label = Chromatic aberration amount
parameter-mondrian-effect-builtin-color-wheel-gain-label = Gain
parameter-mondrian-effect-builtin-color-wheel-gamma-label = Gamma
parameter-mondrian-effect-builtin-color-wheel-lift-label = Lift
parameter-mondrian-effect-builtin-color-wheel-offset-label = Offset
parameter-mondrian-effect-builtin-crop-bottom-label = Bottom
parameter-mondrian-effect-builtin-crop-left-label = Left
parameter-mondrian-effect-builtin-crop-right-label = Right
parameter-mondrian-effect-builtin-crop-top-label = Top
parameter-mondrian-effect-builtin-curves-blue-label = Blue
parameter-mondrian-effect-builtin-curves-green-label = Green
parameter-mondrian-effect-builtin-curves-hue-vs-hue-label = Hue vs Hue
parameter-mondrian-effect-builtin-curves-hue-vs-luma-label = Hue vs Luma
parameter-mondrian-effect-builtin-curves-hue-vs-saturation-label = Hue vs Saturation
parameter-mondrian-effect-builtin-curves-luma-vs-saturation-label = Luma vs Saturation
parameter-mondrian-effect-builtin-curves-master-label = Master
parameter-mondrian-effect-builtin-curves-mode-label = Master mode
parameter-mondrian-effect-builtin-curves-red-label = Red
parameter-mondrian-effect-builtin-curves-saturation-vs-luma-label = Saturation vs Luma
parameter-mondrian-effect-builtin-curves-saturation-vs-saturation-label = Saturation vs Saturation
parameter-mondrian-effect-builtin-gamut-compression-amount-label = Amount
parameter-mondrian-effect-builtin-gaussian-blur-radius-label = Blur radius
parameter-mondrian-effect-builtin-grain-amount-label = Grain amount
parameter-mondrian-effect-builtin-grain-size-label = Grain size
parameter-mondrian-effect-builtin-hdr-grading-blacks-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-blacks-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-blacks-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-blacks-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-blacks-width-label = Width
parameter-mondrian-effect-builtin-hdr-grading-dark-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-dark-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-dark-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-dark-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-dark-width-label = Width
parameter-mondrian-effect-builtin-hdr-grading-global-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-global-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-global-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-highlights-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-highlights-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-highlights-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-highlights-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-highlights-width-label = Width
parameter-mondrian-effect-builtin-hdr-grading-light-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-light-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-light-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-light-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-light-width-label = Width
parameter-mondrian-effect-builtin-hdr-grading-shadows-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-shadows-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-shadows-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-shadows-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-shadows-width-label = Width
parameter-mondrian-effect-builtin-hdr-grading-specular-balance-label = Color balance
parameter-mondrian-effect-builtin-hdr-grading-specular-center-label = Center
parameter-mondrian-effect-builtin-hdr-grading-specular-exposure-label = Exposure
parameter-mondrian-effect-builtin-hdr-grading-specular-saturation-label = Saturation
parameter-mondrian-effect-builtin-hdr-grading-specular-width-label = Width
parameter-mondrian-effect-builtin-highlight-recovery-rolloff-label = Rolloff width
parameter-mondrian-effect-builtin-highlight-recovery-strength-label = Recovery strength
parameter-mondrian-effect-builtin-highlight-recovery-threshold-label = Threshold
parameter-mondrian-effect-builtin-hue-saturation-lightness-hue-label = Hue
parameter-mondrian-effect-builtin-hue-saturation-lightness-lightness-label = Lightness
parameter-mondrian-effect-builtin-hue-saturation-lightness-saturation-label = Saturation
parameter-mondrian-effect-builtin-luma-key-invert-label = Preserve shadows
parameter-mondrian-effect-builtin-luma-key-softness-label = Softness
parameter-mondrian-effect-builtin-luma-key-threshold-label = Threshold
parameter-mondrian-effect-builtin-lut-3d-intensity-label = LUT intensity
parameter-mondrian-effect-builtin-lut-3d-path-label = LUT file
parameter-mondrian-effect-builtin-lut-3d-processing-space-label = Processing color space
parameter-mondrian-effect-builtin-qualifier-blur-radius-label = Matte feather
parameter-mondrian-effect-builtin-qualifier-clean-black-label = Clean Black
parameter-mondrian-effect-builtin-qualifier-clean-white-label = Clean White
parameter-mondrian-effect-builtin-qualifier-denoise-radius-label = Denoise radius
parameter-mondrian-effect-builtin-qualifier-hue-center-label = Hue center
parameter-mondrian-effect-builtin-qualifier-hue-softness-label = Hue softness
parameter-mondrian-effect-builtin-qualifier-hue-width-label = Hue width
parameter-mondrian-effect-builtin-qualifier-invert-label = Invert matte
parameter-mondrian-effect-builtin-qualifier-luminance-high-label = Luma high
parameter-mondrian-effect-builtin-qualifier-luminance-low-label = Luma low
parameter-mondrian-effect-builtin-qualifier-luminance-softness-label = Luma softness
parameter-mondrian-effect-builtin-qualifier-matte-preview-label = Matte preview
parameter-mondrian-effect-builtin-qualifier-mode-label = Selection mode
parameter-mondrian-effect-builtin-qualifier-samples-label = Sampled colors
parameter-mondrian-effect-builtin-qualifier-saturation-high-label = Saturation high
parameter-mondrian-effect-builtin-qualifier-saturation-low-label = Saturation low
parameter-mondrian-effect-builtin-qualifier-saturation-softness-label = Saturation softness
parameter-mondrian-effect-builtin-qualifier-three-d-softness-label = 3D softness
parameter-mondrian-effect-builtin-qualifier-three-d-tolerance-label = 3D tolerance
parameter-mondrian-effect-builtin-sharpen-amount-label = Sharpen amount
parameter-mondrian-effect-builtin-vignette-feather-label = Vignette feather
parameter-mondrian-effect-builtin-vignette-intensity-label = Vignette intensity
parameter-mondrian-effect-builtin-white-balance-temperature-label = Temperature
parameter-mondrian-effect-builtin-white-balance-tint-label = Tint
parameter-mondrian-effect-qualifier-mode-three-dimensional = 3D color
parameter-group-asc-cdl = ASC CDL
parameter-group-hdr-blacks = HDR · Blacks
parameter-group-hdr-dark = HDR · Dark
parameter-group-hdr-global = HDR · Global
parameter-group-hdr-highlights = HDR · Highlights
parameter-group-hdr-light = HDR · Light
parameter-group-hdr-shadows = HDR · Shadows
parameter-group-hdr-specular = HDR · Specular
parameter-group-hsl = HSL
parameter-group-lut = LUT
parameter-group-primaries = Primaries
parameter-group-qualifier = Qualifier
parameter-group-qualifier-3d = Qualifier · 3D
parameter-group-qualifier-matte = Qualifier · Matte
parameter-group-luma-key = Luma key
parameter-group-basic-correction = Basic correction
parameter-group-chroma-key = Chroma key
parameter-group-vignette = Vignette
parameter-group-curves = Curves
parameter-group-blur = Blur
parameter-group-white-balance = White balance
parameter-group-gamut-compression = Gamut compression
parameter-group-chromatic-aberration = Chromatic aberration
parameter-group-crop = Crop
parameter-group-sharpen = Sharpen
parameter-group-grain = Grain
parameter-group-highlight-recovery = Highlight recovery

# Qualifier sample editing
inspector-qualifier-include = Include
inspector-qualifier-exclude = Exclude
inspector-qualifier-remove-sample = Remove
inspector-qualifier-add-include = + Include
inspector-qualifier-add-exclude = + Exclude

resource-unbound = No resource
resource-file = External file or folder
resource-file-only = External file
resource-folder-only = External folder
resource-unavailable-asset = Unavailable asset
resource-project-asset = Project asset
resource-uri = URI
resource-value = Resource value
resource-search-assets = Search project assets
resource-no-assets-match = No assets match this search.
resource-select-asset = Select an asset
resource-browse-file = Browse file…
resource-browse-folder = Browse folder…
resource-apply = Apply
resource-cancel = Cancel
resource-path-required = Choose or enter a file or folder path.
resource-asset-required = Select a project asset.
resource-asset-missing = This asset is no longer available. Select another asset.
visual-source-asset-not-picture = Select a color-managed video or still image asset.
visual-source-selected-asset = Selected picture asset
visual-source-selected-clip = Selected clip
visual-source-selected-sequence = Selected sequence
visual-source-layer-kind = Clip or sequence
visual-source-search-layers = Search clips and sequences
visual-source-select-layer = Select a clip or sequence
visual-source-unavailable-layer = Selected clip or sequence is unavailable
visual-source-no-layers-match = No matching clips or sequences
visual-source-clip-kind = Clip
visual-source-sequence-kind = Sequence
resource-uri-invalid = Enter an absolute URI with a scheme and no spaces or control characters.
resource-browse-failed = The native picker could not open. You can enter a path or try again.
resource-select-one-file = Select one file for this parameter.
resource-not-admitted = This parameter does not accept this resource type, file extension, path format or path length.

keyframe-interpolation-hold = Hold
keyframe-interpolation-linear = Linear
keyframe-interpolation-bezier = Bezier
keyframe-interpolation-auto-bezier = Auto Bezier
keyframe-interpolation-continuous-bezier = Continuous Bezier
keyframe-interpolation-ease-in = Ease In
keyframe-interpolation-ease-out = Ease Out

resource-browse-existing-file = Browse existing…
resource-path-existing = Enter an existing path
resource-path-existing-or-new = Enter an existing or new path
resource-path-checking = Checking this path… You can edit it or cancel.
resource-path-timeout = Checking this path took too long. Verify its availability and try again.
resource-path-confirm-again = Finish text input, then confirm the path again.
resource-owner-unavailable = This parameter can no longer be edited.
resource-path-missing = This path does not exist. Choose an existing file or folder.
resource-path-wrong-kind = This path does not have the required file or folder type.
resource-path-unavailable = This path cannot be checked. Verify its availability and permissions.

resource-path-owner-required = Open or save a project before entering a relative resource path.
resource-path-ambiguous = Enter a full path or a path relative to the project directory; drive-relative paths are ambiguous.
curve-point-count-label = { $label } ({ $count }/{ $maximum })
export-alpha = Alpha
export-legalizer = Legalizer
export-vbv-buffer = VBV buffer (kbit)
export-depth-value = { $bits }-bit
export-integer-depth = { $bits }-bit integer
export-pcm-depth = PCM { $bits }-bit
export-video-range-full = Full
export-video-range-legal = Legal / Video
export-container-imf = IMF package
export-container-dcp = SMPTE DCP package
export-professional-dcp-video = JPEG 2000 / ST 428-1 XYZ 12-bit
export-professional-pcm = PCM 24-bit / 48 kHz / stereo
export-essence-prores = ProRes 422 HQ 10-bit 4:2:2
export-essence-avc = AVC High 4:2:2 10-bit intra
export-essence-jpeg2000 = JPEG 2000 XYZ 12-bit

curve-editor-name = Curve editor
curve-editor-keyboard-help = Home/End: first/last point; Ctrl/Cmd + Left/Right: previous/next point; Alt + Left/Right: incoming/outgoing handle; Alt + Up: return to point; Left/Right: one frame; Shift + Left/Right: five frames; Up/Down: value; Shift + F10: interpolation menu. Ctrl/Cmd + wheel: time zoom; add Shift: value zoom; middle drag: pan; Ctrl/Cmd + PageUp/PageDown: zoom; add Shift: value zoom; Ctrl/Cmd + Shift + arrows: pan; Ctrl/Cmd + 0: reset view.
curve-editor-shape-keyboard-help = Home/End: first/last point; Ctrl/Cmd + Left/Right: previous/next point; arrows: adjust; Shift + arrows: larger adjustment. Ctrl/Cmd + wheel: time zoom; add Shift: value zoom; middle drag: pan; Ctrl/Cmd + PageUp/PageDown: zoom; add Shift: value zoom; Ctrl/Cmd + Shift + arrows: pan; Ctrl/Cmd + 0: reset view.

curve-editor-selection-keyboard-help = { curve-editor-keyboard-help } Shift or Ctrl/Cmd + click a key: toggle selection; Ctrl/Cmd + A: select authored keys; Delete/Backspace: delete selection in one Undo; Escape: clear selection. Left/Right: move selected keys by one frame (Shift: five frames). Up/Down: move selected values together by one increment (Shift: five), stopping together at limits. Right-click a selected key or Shift+F10: apply interpolation to the whole selection. Drag a selected key to move the whole group; Escape cancels the drag. Drag empty space: select keys; Shift: add; Ctrl/Cmd+Shift: toggle. Ctrl/Cmd+click empty space: insert a key. Escape restores the selection during a marquee.

audio-rack-occurrence-row = Time reference
audio-rack-occurrence-choose = Choose an audio component
audio-rack-occurrence-option = Audio component { $index } · Start { $time } s

# Focused curve clipboard
error-curve-clipboard-owner-changed = The curve selection changed. Select the target curve again.
error-curve-clipboard-parameter-missing = The target parameter changed. Select the curve again.
error-curve-clipboard-not-animated = This parameter cannot contain keyframes.
error-curve-clipboard-invalid-selection = The key selection changed. Select the complete key group again.
error-curve-clipboard-incompatible = Copy keys from one compatible parameter before pasting into this curve.
error-curve-clipboard-empty = The keyframe clipboard is empty.
error-curve-clipboard-missing-playhead = Select a sequence and position the playhead before pasting.
error-curve-clipboard-out-of-bounds = The whole key group must fit inside the target clip. Move the playhead or select fewer keys.

curve-editor-clipboard-keyboard-help = { curve-editor-selection-keyboard-help } Ctrl/Cmd+C: copy the selected keys; Ctrl/Cmd+X: cut the selected keys in one Undo; Ctrl/Cmd+V: paste into this curve at the playhead in one Undo.

curve-editor-read-only-keyboard-help = Read-only curve. Click to inspect points; Home/End and Ctrl/Cmd + Left/Right navigate. Ctrl/Cmd + wheel zooms; middle drag pans; Ctrl/Cmd + 0 resets the view.
curve-editor-read-only-clipboard-keyboard-help = { curve-editor-read-only-keyboard-help } Shift or Ctrl/Cmd + click a key toggles selection; Ctrl/Cmd + A selects authored keys; Ctrl/Cmd + C copies. Editing is unavailable. Drag empty space to select; Shift adds, Ctrl/Cmd+Shift toggles; Escape restores the prior selection.
status-history-accessibility-open = Recent notifications. Current status: { $status }. Showing { $first }–{ $last } of { $total }, newest first.
    { $entries }
    Up/Down: newer/older. Page Up/Down: previous/next page. Home/End: newest/oldest. Escape: close.
status-history-accessibility-empty = Recent notifications. Current status: { $status }. No notifications. Escape: close.
status-history-entry-error = Error: { $message }

parameter-reset-to-default = Reset

audio-input-row = Auxiliary inputs
audio-input-status = Input status
audio-input-query = Inspect inputs
audio-input-requery = Refresh inputs
audio-input-cancel = Cancel inspection
audio-input-pending = Inspecting inputs…
audio-input-none = This processor has no auxiliary inputs
audio-input-ready = { $count ->
    [one] One auxiliary input available
   *[other] { $count } auxiliary inputs available
    }
audio-input-failed = Inputs unavailable: { $error }
audio-input-not-inspected = Not inspected

audio-sidechain-input = Auxiliary input · { $identity }
audio-sidechain-bus = Bus · { $identity }
audio-sidechain-add-source = Add source…
audio-sidechain-level = Sidechain level
audio-sidechain-level-curve = Sidechain level curve
parameter-unit-samples = samples
parameter-unit-stops = stops
parameter-unit-nits = nits
audio-sidechain-status = Sidechain status
audio-sidechain-controls = Sidechain controls
audio-sidechain-disconnect = Disconnect source
audio-sidechain-missing-track = Missing track · { $identity }

audio-sidechain-source = Source

## Timeline / Inspector owned empty and invalid states
timeline-empty-no-sequence = No active sequence
    Open a project or create a sequence to start editing
timeline-empty-no-tracks = This sequence has no tracks
    Add a video or audio track to start editing
timeline-invalid-display = Invalid sequence time display settings
    { $reason }
inspector-empty-selection = No clip selected
    Select a clip, layer or effect to adjust its parameters here.
notification-native-processor = Processor: { $message }

# Authenticated native audio diagnostics, never lifecycle/author authority.
notification-native-audio-warning = Native audio plugin warning ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-error = Native audio plugin error ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-fatal = Native audio plugin fatal diagnostic ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-host-misbehavior = Native audio plugin reports host misbehavior ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-plugin-misbehavior = Native audio plugin reports plugin misbehavior ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-unknown-severity = Native audio plugin reports unknown severity { $severity } ({ $instance }, { $process }): { $encoding ->
        [hex] Native bytes (hex): { $detail }
       *[text] { $detail }
    } { $truncated ->
        [yes] (Message truncated)
       *[no] { "" }
    } { $null ->
        [yes] (Invalid null native message)
       *[no] { "" }
    }
notification-native-audio-log-incomplete = Native audio diagnostics are incomplete ({ $instance }, { $process }). { $capacity ->
        [yes] Queue capacity reached.
       *[no] { "" }
    } { $contention ->
        [yes] Concurrent producers exhausted their bounded attempts.
       *[no] { "" }
    } { $exhausted ->
        [yes] Native message sequence exhausted.
       *[no] { "" }
    }

viewer-native-progress = { $message } · { $percent }%

# Native replies are opaque local Widget callbacks, never authoring actions.
native-visual-question-title = Plug-in question
native-visual-question-yes = Yes
native-visual-question-no = No
native-visual-question-dismiss = Dismiss

# Product-owned native editor status; plugin labels remain native metadata.
native-editor-row = Native editor
native-editor-open = Open editor
native-editor-close = Close editor
native-editor-opening = Native editor request queued
native-editor-opened = Native editor opened
native-editor-closed = Native editor closed
native-editor-unavailable = Native editor is unavailable for this adapter
native-editor-no-view = This processor has no native editor
native-editor-control-pending = Native editor changes are pending
native-editor-owner-changed = The native editor owner changed; reopen the editor
native-editor-close-current = Close the current native editor first
native-editor-cleanup-pending = Native editor cleanup is pending
native-editor-failed = The native editor request failed

audio-rack-native-controlled-parameter = Controlled by plugin
audio-rack-adopt-displayed-value = Adopt displayed value
audio-rack-query-native-parameter = Read current value
native-parameter-query-unavailable = Current plugin value is unavailable

## Flip 翻转
effect-flip = Flip
parameter-group-flip = Flip
effect-category-transform-and-size = Transform and Size
parameter-mondrian-effect-builtin-flip-axis-label = Axis
parameter-mondrian-effect-flip-axis-horizontal = Horizontal
parameter-mondrian-effect-flip-axis-vertical = Vertical
parameter-mondrian-effect-flip-axis-both = Both
parameter-mondrian-effect-builtin-flip-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-flip-centre-x-label = Center X
parameter-mondrian-effect-builtin-flip-centre-y-label = Center Y

## Ripple 波纹
effect-ripple = Ripple
parameter-group-ripple = Ripple
effect-category-distort = Distort
parameter-mondrian-effect-builtin-ripple-centre-x-label = Ripple Center X
parameter-mondrian-effect-builtin-ripple-centre-y-label = Ripple Center Y
parameter-mondrian-effect-builtin-ripple-phase-label = Ripple Phase
parameter-mondrian-effect-builtin-ripple-radius-label = Radius
parameter-mondrian-effect-builtin-ripple-wave-height-label = Wave Height
parameter-mondrian-effect-builtin-ripple-wave-speed-label = Wave Speed
parameter-mondrian-effect-builtin-ripple-wave-type-label = Wave Type
parameter-mondrian-effect-ripple-wave-type-asymmetric = Asymmetric
parameter-mondrian-effect-ripple-wave-type-symmetric = Symmetric
parameter-mondrian-effect-builtin-ripple-wave-width-label = Wave Width

## Wave Warp 波形变形
effect-wave-warp = Wave Warp
parameter-group-wave-warp = Wave Warp
parameter-mondrian-effect-builtin-wave-warp-direction-label = Direction
parameter-mondrian-effect-builtin-wave-warp-phase-label = Phase
parameter-mondrian-effect-builtin-wave-warp-pin-label = Pin
parameter-mondrian-effect-wave-warp-pin-none = None
parameter-mondrian-effect-wave-warp-pin-all-edges = All Edges
parameter-mondrian-effect-wave-warp-pin-center = Center
parameter-mondrian-effect-wave-warp-pin-left-edge = Left Edge
parameter-mondrian-effect-wave-warp-pin-top-edge = Top Edge
parameter-mondrian-effect-wave-warp-pin-right-edge = Right Edge
parameter-mondrian-effect-wave-warp-pin-bottom-edge = Bottom Edge
parameter-mondrian-effect-wave-warp-pin-horizontal-edges = Horizontal Edges
parameter-mondrian-effect-wave-warp-pin-vertical-edges = Vertical Edges
parameter-mondrian-effect-builtin-wave-warp-quality-label = Antialiasing (Best Quality)
parameter-mondrian-effect-wave-warp-quality-low = Low
parameter-mondrian-effect-wave-warp-quality-normal = Normal
parameter-mondrian-effect-wave-warp-quality-high = High
parameter-mondrian-effect-builtin-wave-warp-wave-height-label = Wave Height
parameter-mondrian-effect-builtin-wave-warp-wave-speed-label = Wave Speed
parameter-mondrian-effect-builtin-wave-warp-wave-type-label = Wave Type
parameter-mondrian-effect-wave-warp-wave-type-sine = Sine
parameter-mondrian-effect-wave-warp-wave-type-square = Square
parameter-mondrian-effect-wave-warp-wave-type-triangle = Triangle
parameter-mondrian-effect-wave-warp-wave-type-sawtooth = Sawtooth
parameter-mondrian-effect-wave-warp-wave-type-circle = Circle
parameter-mondrian-effect-wave-warp-wave-type-semicircle = Semicircle
parameter-mondrian-effect-wave-warp-wave-type-non-circle = Non-Circle
parameter-mondrian-effect-wave-warp-wave-type-noise = Noise
parameter-mondrian-effect-wave-warp-wave-type-smooth-noise = Smooth Noise
parameter-mondrian-effect-builtin-wave-warp-wave-width-label = Wave Width

## Transform 变换
effect-transform = Transform
parameter-group-transform = Transform
parameter-mondrian-effect-builtin-transform-anchor-x-label = Anchor Point X
parameter-mondrian-effect-builtin-transform-anchor-y-label = Anchor Point Y
parameter-mondrian-effect-builtin-transform-opacity-label = Opacity
parameter-mondrian-effect-builtin-transform-position-x-label = Position X
parameter-mondrian-effect-builtin-transform-position-y-label = Position Y
parameter-mondrian-effect-builtin-transform-rotation-label = Rotation
parameter-mondrian-effect-builtin-transform-sampling-label = Sampling
parameter-mondrian-effect-transform-sampling-low = Low
parameter-mondrian-effect-transform-sampling-normal = Normal
parameter-mondrian-effect-transform-sampling-high = High
parameter-mondrian-effect-builtin-transform-scale-height-label = Scale Height
parameter-mondrian-effect-builtin-transform-scale-width-label = Scale Width
parameter-mondrian-effect-builtin-transform-shutter-angle-label = Shutter Angle (Reserved: motion blur needs per-frame velocity)
parameter-mondrian-effect-builtin-transform-skew-label = Skew
parameter-mondrian-effect-builtin-transform-skew-axis-label = Skew Axis
parameter-mondrian-effect-builtin-transform-uniform-scale-label = Uniform Scale
parameter-mondrian-effect-builtin-transform-use-composition-shutter-label = Use Composition Shutter Angle (Reserved: motion blur needs per-frame velocity)
parameter-mondrian-effect-transform-use-composition-shutter-off = Off
parameter-mondrian-effect-transform-use-composition-shutter-on = On

## Corner Pin 边角定位
effect-corner-pin = Corner Pin
parameter-group-corner-pin = Corner Pin
parameter-mondrian-effect-builtin-corner-pin-bottom-left-x-label = Bottom Left X
parameter-mondrian-effect-builtin-corner-pin-bottom-left-y-label = Bottom Left Y
parameter-mondrian-effect-builtin-corner-pin-bottom-right-x-label = Bottom Right X
parameter-mondrian-effect-builtin-corner-pin-bottom-right-y-label = Bottom Right Y
parameter-mondrian-effect-builtin-corner-pin-top-left-x-label = Top Left X
parameter-mondrian-effect-builtin-corner-pin-top-left-y-label = Top Left Y
parameter-mondrian-effect-builtin-corner-pin-top-right-x-label = Top Right X
parameter-mondrian-effect-builtin-corner-pin-top-right-y-label = Top Right Y

## Turbulent Displace 湍流置换
effect-turbulent-displace = Turbulent Displace
parameter-group-turbulent-displace = Turbulent Displace
parameter-mondrian-effect-builtin-turbulent-displace-amount-label = Amount
parameter-mondrian-effect-builtin-turbulent-displace-antialias-label = Antialiasing
parameter-mondrian-effect-turbulent-displace-antialias-low = Low
parameter-mondrian-effect-turbulent-displace-antialias-high = High
parameter-mondrian-effect-builtin-turbulent-displace-complexity-label = Complexity
parameter-mondrian-effect-builtin-turbulent-displace-cycle-label = Cycle
parameter-mondrian-effect-builtin-turbulent-displace-cycle-evolution-label = Cycle Evolution
parameter-mondrian-effect-builtin-turbulent-displace-displacement-label = Displacement
parameter-mondrian-effect-turbulent-displace-displacement-turbulent = Turbulent
parameter-mondrian-effect-turbulent-displace-displacement-bulge = Bulge
parameter-mondrian-effect-turbulent-displace-displacement-twist = Twist
parameter-mondrian-effect-turbulent-displace-displacement-turbulent-smoother = Turbulent Smoother
parameter-mondrian-effect-turbulent-displace-displacement-bulge-smoother = Bulge Smoother
parameter-mondrian-effect-turbulent-displace-displacement-twist-smoother = Twist Smoother
parameter-mondrian-effect-turbulent-displace-displacement-vertical = Vertical Displacement
parameter-mondrian-effect-turbulent-displace-displacement-horizontal = Horizontal Displacement
parameter-mondrian-effect-turbulent-displace-displacement-cross = Cross Displacement
parameter-mondrian-effect-builtin-turbulent-displace-evolution-label = Evolution
parameter-mondrian-effect-builtin-turbulent-displace-offset-x-label = Offset X
parameter-mondrian-effect-builtin-turbulent-displace-offset-y-label = Offset Y
parameter-mondrian-effect-builtin-turbulent-displace-pin-label = Pin
parameter-mondrian-effect-turbulent-displace-pin-none = None
parameter-mondrian-effect-turbulent-displace-pin-all-edges = All Edges
parameter-mondrian-effect-turbulent-displace-pin-center = Center
parameter-mondrian-effect-turbulent-displace-pin-left-edge = Left Edge
parameter-mondrian-effect-turbulent-displace-pin-top-edge = Top Edge
parameter-mondrian-effect-turbulent-displace-pin-right-edge = Right Edge
parameter-mondrian-effect-turbulent-displace-pin-bottom-edge = Bottom Edge
parameter-mondrian-effect-turbulent-displace-pin-horizontal-edges = Horizontal Edges
parameter-mondrian-effect-turbulent-displace-pin-vertical-edges = Vertical Edges
parameter-mondrian-effect-builtin-turbulent-displace-random-seed-label = Random Seed
parameter-mondrian-effect-builtin-turbulent-displace-resize-layer-label = Resize Layer
parameter-mondrian-effect-builtin-turbulent-displace-size-label = Size

## Deband 去色带
effect-deband = Deband
parameter-group-deband = Deband
effect-category-restore = Restore
parameter-mondrian-effect-builtin-deband-edge-threshold-label = Edge Threshold
parameter-mondrian-effect-builtin-deband-post-optimization-label = Color Dithering
parameter-mondrian-effect-builtin-deband-radius-label = Radius

## Ramp 渐变
effect-ramp = Ramp
parameter-group-ramp = Ramp
parameter-mondrian-effect-builtin-ramp-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-ramp-end-label = End of Ramp
parameter-mondrian-effect-builtin-ramp-end-color-label = End Color
parameter-mondrian-effect-builtin-ramp-scatter-label = Ramp Scatter
parameter-mondrian-effect-builtin-ramp-shape-label = Ramp Shape
parameter-mondrian-effect-ramp-shape-linear = Linear
parameter-mondrian-effect-ramp-shape-radial = Radial
parameter-mondrian-effect-builtin-ramp-start-label = Start of Ramp
parameter-mondrian-effect-builtin-ramp-start-color-label = Start Color

## Brightness & Contrast 亮度对比度
effect-brightness-contrast = Brightness & Contrast
parameter-group-brightness-contrast = Brightness & Contrast
parameter-mondrian-effect-builtin-brightness-contrast-brightness-label = Brightness
parameter-mondrian-effect-builtin-brightness-contrast-contrast-label = Contrast
parameter-mondrian-effect-builtin-brightness-contrast-legacy-label = Use Legacy (Supports HDR)
parameter-mondrian-effect-brightness-contrast-legacy-off = Off
parameter-mondrian-effect-brightness-contrast-legacy-on = On

## Levels 色阶
effect-levels = Levels
parameter-group-levels = Levels
parameter-mondrian-effect-builtin-levels-channel-label = Channel
parameter-mondrian-effect-levels-channel-rgb = RGB
parameter-mondrian-effect-levels-channel-red = Red
parameter-mondrian-effect-levels-channel-green = Green
parameter-mondrian-effect-levels-channel-blue = Blue
parameter-mondrian-effect-levels-channel-alpha = Alpha
parameter-mondrian-effect-builtin-levels-clip-output-black-label = Clip to Output Black
parameter-mondrian-effect-levels-clip-output-black-off = Off
parameter-mondrian-effect-levels-clip-output-black-clamp-output = Clamp Output
parameter-mondrian-effect-levels-clip-output-black-clamp-display = Clamp to Display
parameter-mondrian-effect-builtin-levels-clip-output-white-label = Clip to Output White
parameter-mondrian-effect-levels-clip-output-white-off = Off
parameter-mondrian-effect-levels-clip-output-white-clamp-output = Clamp Output
parameter-mondrian-effect-levels-clip-output-white-clamp-display = Clamp to Display
parameter-mondrian-effect-builtin-levels-gamma-label = Gamma
parameter-mondrian-effect-builtin-levels-input-black-label = Input Black
parameter-mondrian-effect-builtin-levels-input-white-label = Input White
parameter-mondrian-effect-builtin-levels-output-black-label = Output Black
parameter-mondrian-effect-builtin-levels-output-white-label = Output White

## Glow 发光
effect-glow = Glow
parameter-group-glow = Glow
effect-category-light = Light
parameter-mondrian-effect-builtin-glow-color-a-label = Color A
parameter-mondrian-effect-builtin-glow-color-b-label = Color B
parameter-mondrian-effect-builtin-glow-color-loops-label = Color Loops
parameter-mondrian-effect-builtin-glow-color-phase-label = Color Phase
parameter-mondrian-effect-builtin-glow-colors-label = Glow Colors
parameter-mondrian-effect-builtin-glow-composite-original-label = Composite Original
parameter-mondrian-effect-builtin-glow-dimensions-label = Glow Dimensions
parameter-mondrian-effect-builtin-glow-midpoint-label = A/B Midpoint
parameter-mondrian-effect-builtin-glow-operation-label = Glow Operation
parameter-mondrian-effect-builtin-glow-radius-label = Radius
parameter-mondrian-effect-builtin-glow-strength-label = Strength
parameter-mondrian-effect-builtin-glow-threshold-label = Threshold

## Four-Color Gradient 四色渐变
effect-four-color-gradient = 4-Color Gradient
parameter-group-four-color-gradient = 4-Color Gradient
parameter-mondrian-effect-builtin-four-color-gradient-blend-mode-label = Blending Mode
parameter-mondrian-effect-four-color-gradient-blend-mode-normal = Normal
parameter-mondrian-effect-four-color-gradient-blend-mode-darken = Darken
parameter-mondrian-effect-four-color-gradient-blend-mode-multiply = Multiply
parameter-mondrian-effect-four-color-gradient-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-four-color-gradient-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-four-color-gradient-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-four-color-gradient-blend-mode-lighten = Lighten
parameter-mondrian-effect-four-color-gradient-blend-mode-screen = Screen
parameter-mondrian-effect-four-color-gradient-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-four-color-gradient-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-four-color-gradient-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-four-color-gradient-blend-mode-overlay = Overlay
parameter-mondrian-effect-four-color-gradient-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-four-color-gradient-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-four-color-gradient-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-four-color-gradient-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-four-color-gradient-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-four-color-gradient-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-four-color-gradient-blend-mode-difference = Difference
parameter-mondrian-effect-four-color-gradient-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-four-color-gradient-blend-mode-subtract = Subtract
parameter-mondrian-effect-four-color-gradient-blend-mode-divide = Divide
parameter-mondrian-effect-four-color-gradient-blend-mode-hue = Hue
parameter-mondrian-effect-four-color-gradient-blend-mode-saturation = Saturation
parameter-mondrian-effect-four-color-gradient-blend-mode-color = Color
parameter-mondrian-effect-four-color-gradient-blend-mode-luminosity = Luminosity
parameter-mondrian-effect-builtin-four-color-gradient-blend-label = Blending
parameter-mondrian-effect-builtin-four-color-gradient-color-1-label = Color 1
parameter-mondrian-effect-builtin-four-color-gradient-color-2-label = Color 2
parameter-mondrian-effect-builtin-four-color-gradient-color-3-label = Color 3
parameter-mondrian-effect-builtin-four-color-gradient-color-4-label = Color 4
parameter-mondrian-effect-builtin-four-color-gradient-dither-label = Dither
parameter-mondrian-effect-builtin-four-color-gradient-opacity-label = Opacity
parameter-mondrian-effect-builtin-four-color-gradient-point-1-label = Point 1
parameter-mondrian-effect-builtin-four-color-gradient-point-2-label = Point 2
parameter-mondrian-effect-builtin-four-color-gradient-point-3-label = Point 3
parameter-mondrian-effect-builtin-four-color-gradient-point-4-label = Point 4

## Fractal Noise 分形噪波
effect-fractal-noise = Fractal Noise
parameter-group-fractal-noise = Fractal Noise
parameter-mondrian-effect-builtin-fractal-noise-blend-mode-label = Blend Mode
parameter-mondrian-effect-fractal-noise-blend-mode-normal = Normal
parameter-mondrian-effect-fractal-noise-blend-mode-dissolve = Dissolve
parameter-mondrian-effect-fractal-noise-blend-mode-darken = Darken
parameter-mondrian-effect-fractal-noise-blend-mode-multiply = Multiply
parameter-mondrian-effect-fractal-noise-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-fractal-noise-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-fractal-noise-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-fractal-noise-blend-mode-lighten = Lighten
parameter-mondrian-effect-fractal-noise-blend-mode-screen = Screen
parameter-mondrian-effect-fractal-noise-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-fractal-noise-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-fractal-noise-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-fractal-noise-blend-mode-overlay = Overlay
parameter-mondrian-effect-fractal-noise-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-fractal-noise-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-fractal-noise-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-fractal-noise-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-fractal-noise-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-fractal-noise-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-fractal-noise-blend-mode-difference = Difference
parameter-mondrian-effect-fractal-noise-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-fractal-noise-blend-mode-subtract = Subtract
parameter-mondrian-effect-fractal-noise-blend-mode-divide = Divide
parameter-mondrian-effect-fractal-noise-blend-mode-hue = Hue
parameter-mondrian-effect-fractal-noise-blend-mode-saturation = Saturation
parameter-mondrian-effect-fractal-noise-blend-mode-color = Color
parameter-mondrian-effect-fractal-noise-blend-mode-luminosity = Luminosity
parameter-mondrian-effect-builtin-fractal-noise-brightness-label = Brightness
parameter-mondrian-effect-builtin-fractal-noise-centre-auxiliary-label = Auxiliary Center
parameter-mondrian-effect-builtin-fractal-noise-complexity-label = Complexity
parameter-mondrian-effect-builtin-fractal-noise-contrast-label = Contrast
parameter-mondrian-effect-builtin-fractal-noise-cycle-label = Cycle
parameter-mondrian-effect-builtin-fractal-noise-cycle-evolution-label = Cycle Evolution
parameter-mondrian-effect-builtin-fractal-noise-evolution-label = Evolution
parameter-mondrian-effect-builtin-fractal-noise-fractal-type-label = Fractal Type
parameter-mondrian-effect-fractal-noise-fractal-type-basic = Basic
parameter-mondrian-effect-fractal-noise-fractal-type-turbulent-smooth = Turbulent Smooth
parameter-mondrian-effect-fractal-noise-fractal-type-turbulent-sharp = Turbulent Sharp
parameter-mondrian-effect-fractal-noise-fractal-type-turbulent-basic = Turbulent Basic
parameter-mondrian-effect-fractal-noise-fractal-type-max = Max
parameter-mondrian-effect-fractal-noise-fractal-type-swirly = Swirly
parameter-mondrian-effect-fractal-noise-fractal-type-rock = Rock
parameter-mondrian-effect-fractal-noise-fractal-type-threads = Threads
parameter-mondrian-effect-fractal-noise-fractal-type-multiply = Multiply
parameter-mondrian-effect-builtin-fractal-noise-invert-label = Invert
parameter-mondrian-effect-builtin-fractal-noise-noise-type-label = Noise Type
parameter-mondrian-effect-fractal-noise-noise-type-soft-linear = Soft Linear
parameter-mondrian-effect-fractal-noise-noise-type-linear = Linear
parameter-mondrian-effect-fractal-noise-noise-type-spline = Spline
parameter-mondrian-effect-builtin-fractal-noise-offset-turbulence-label = Turbulence Offset
parameter-mondrian-effect-builtin-fractal-noise-opacity-label = Opacity
parameter-mondrian-effect-builtin-fractal-noise-overflow-label = Overflow
parameter-mondrian-effect-fractal-noise-overflow-clip = Clip
parameter-mondrian-effect-fractal-noise-overflow-soft-clamp = Soft Clamp
parameter-mondrian-effect-fractal-noise-overflow-wrap = Wrap
parameter-mondrian-effect-builtin-fractal-noise-perspective-offset-label = Perspective Offset
parameter-mondrian-effect-builtin-fractal-noise-random-seed-label = Random Seed
parameter-mondrian-effect-builtin-fractal-noise-rotation-label = Rotation
parameter-mondrian-effect-builtin-fractal-noise-scale-label = Scale
parameter-mondrian-effect-builtin-fractal-noise-scale-height-label = Scale Height
parameter-mondrian-effect-builtin-fractal-noise-scale-width-label = Scale Width
parameter-mondrian-effect-builtin-fractal-noise-sub-influence-label = Sub Influence
parameter-mondrian-effect-builtin-fractal-noise-sub-offset-label = Sub Offset
parameter-mondrian-effect-builtin-fractal-noise-sub-rotation-label = Sub Rotation
parameter-mondrian-effect-builtin-fractal-noise-sub-scale-label = Sub Scale
parameter-mondrian-effect-builtin-fractal-noise-uniform-scale-label = Uniform Scaling

## Keylight 专业抠像
effect-keylight = Keylight
parameter-mondrian-effect-builtin-keylight-despill-label = Despill
parameter-mondrian-effect-builtin-keylight-despill-bias-label = Despill Bias
parameter-mondrian-effect-builtin-keylight-display-label = Display
parameter-mondrian-effect-keylight-display-final = Final
parameter-mondrian-effect-keylight-display-matte = Matte
parameter-mondrian-effect-keylight-display-foreground = Foreground
parameter-mondrian-effect-keylight-display-background = Background
parameter-mondrian-effect-builtin-keylight-edge-feather-label = Edge Feather
parameter-mondrian-effect-builtin-keylight-edge-shrink-label = Edge Shrink
parameter-mondrian-effect-builtin-keylight-inner-matte-label = Inner Matte
parameter-mondrian-effect-builtin-keylight-outer-matte-label = Outer Matte
parameter-mondrian-effect-builtin-keylight-preblur-label = Pre-blur
parameter-mondrian-effect-builtin-keylight-screen-balance-label = Screen Balance
parameter-mondrian-effect-builtin-keylight-screen-color-label = Screen Color
parameter-mondrian-effect-builtin-keylight-screen-gain-label = Screen Gain

## Echo 残影
effect-echo = Echo
parameter-group-echo = Echo
effect-category-time = Time
parameter-mondrian-effect-builtin-echo-decay-label = Decay
parameter-mondrian-effect-builtin-echo-echo-count-label = Number of Echoes
parameter-mondrian-effect-builtin-echo-echo-time-label = Echo Time
parameter-mondrian-effect-builtin-echo-operator-label = Operator
parameter-mondrian-effect-echo-operator-add = Add
parameter-mondrian-effect-echo-operator-screen = Screen
parameter-mondrian-effect-echo-operator-multiply = Multiply
parameter-mondrian-effect-echo-operator-overlay = Overlay
parameter-mondrian-effect-echo-operator-darken = Darken
parameter-mondrian-effect-echo-operator-lighten = Lighten
parameter-mondrian-effect-echo-operator-normal = Normal
parameter-mondrian-effect-builtin-echo-starting-intensity-label = Starting Intensity

## Kaleidoscope 万花筒
effect-kaleidoscope = Kaleidoscope
parameter-group-kaleidoscope = Kaleidoscope
parameter-mondrian-effect-builtin-kaleidoscope-center-x-label = Center X
parameter-mondrian-effect-builtin-kaleidoscope-center-y-label = Center Y
parameter-mondrian-effect-builtin-kaleidoscope-floating-center-label = Floating Center
parameter-mondrian-effect-builtin-kaleidoscope-mirroring-label = Mirroring
parameter-mondrian-effect-builtin-kaleidoscope-rotation-label = Rotation
parameter-mondrian-effect-builtin-kaleidoscope-size-label = Size

## Tritone 三色调
effect-tritone = Tritone
parameter-group-tritone = Tritone
effect-category-color-grading = Color & Grading
parameter-mondrian-effect-builtin-tritone-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-tritone-highlights-label = Highlights
parameter-mondrian-effect-builtin-tritone-midtones-label = Midtones
parameter-mondrian-effect-builtin-tritone-shadows-label = Shadows

## Offset 偏移
effect-offset = Offset
parameter-group-offset = Offset
parameter-mondrian-effect-builtin-offset-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-offset-shift-x-label = Shift X
parameter-mondrian-effect-builtin-offset-shift-y-label = Shift Y

## Bulge 凸出
effect-bulge = Bulge
parameter-group-bulge = Bulge
parameter-mondrian-effect-builtin-bulge-antialias-label = Anti-aliasing
parameter-mondrian-effect-bulge-antialias-off = Off
parameter-mondrian-effect-bulge-antialias-on = On
parameter-mondrian-effect-builtin-bulge-centre-x-label = Center X
parameter-mondrian-effect-builtin-bulge-centre-y-label = Center Y
parameter-mondrian-effect-builtin-bulge-height-label = Bulge Height
parameter-mondrian-effect-builtin-bulge-pinned-label = Pinned
parameter-mondrian-effect-bulge-pinned-off = Off
parameter-mondrian-effect-bulge-pinned-on = On
parameter-mondrian-effect-builtin-bulge-radius-x-label = Horizontal Radius
parameter-mondrian-effect-builtin-bulge-radius-y-label = Vertical Radius
parameter-mondrian-effect-builtin-bulge-taper-radius-label = Taper Radius

## Split 分离
effect-split = Split
parameter-group-split = Split
parameter-mondrian-effect-builtin-split-point-a-x-label = Point A X
parameter-mondrian-effect-builtin-split-point-a-y-label = Point A Y
parameter-mondrian-effect-builtin-split-point-b-x-label = Point B X
parameter-mondrian-effect-builtin-split-point-b-y-label = Point B Y
parameter-mondrian-effect-builtin-split-split-label = Split Amount

## Motion Tile 动态拼贴
effect-motion-tile = Motion Tile
parameter-group-motion-tile = Tile
parameter-group-output = Output
parameter-mondrian-effect-builtin-motion-tile-center-x-label = Tile Center X
parameter-mondrian-effect-builtin-motion-tile-center-y-label = Tile Center Y
parameter-mondrian-effect-builtin-motion-tile-horizontal-phase-label = Horizontal Phase
parameter-mondrian-effect-builtin-motion-tile-mirror-edges-label = Mirror Edges
parameter-mondrian-effect-builtin-motion-tile-output-height-label = Output Height
parameter-mondrian-effect-builtin-motion-tile-output-width-label = Output Width
parameter-mondrian-effect-builtin-motion-tile-phase-label = Phase
parameter-mondrian-effect-builtin-motion-tile-tile-height-label = Tile Height
parameter-mondrian-effect-builtin-motion-tile-tile-width-label = Tile Width

## Convolution Kernel 卷积核
effect-convolution = Convolution Kernel
parameter-group-convolution = Convolution Kernel
parameter-mondrian-effect-builtin-convolution-absolute-values-label = Absolute Values
parameter-mondrian-effect-builtin-convolution-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-convolution-cell-1-label = 1 - L1
parameter-mondrian-effect-builtin-convolution-cell-2-label = 2 - L1
parameter-mondrian-effect-builtin-convolution-cell-3-label = 3 - L1
parameter-mondrian-effect-builtin-convolution-cell-4-label = 1 - L2
parameter-mondrian-effect-builtin-convolution-cell-5-label = 2 - L2
parameter-mondrian-effect-builtin-convolution-cell-6-label = 3 - L2
parameter-mondrian-effect-builtin-convolution-cell-7-label = 1 - L3
parameter-mondrian-effect-builtin-convolution-cell-8-label = 2 - L3
parameter-mondrian-effect-builtin-convolution-cell-9-label = 3 - L3

## Invert 反转
effect-invert = Invert
parameter-group-invert = Invert
effect-category-channel = Channel
parameter-mondrian-effect-builtin-invert-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-invert-channels-label = Channel
parameter-builtin-invert-channels-rgb = RGB
parameter-builtin-invert-channels-red = Red
parameter-builtin-invert-channels-green = Green
parameter-builtin-invert-channels-blue = Blue
parameter-builtin-invert-channels-alpha = Alpha
parameter-builtin-invert-channels-red-green = Red Green
parameter-builtin-invert-channels-red-blue = Red Blue
parameter-builtin-invert-channels-green-blue = Green Blue
parameter-builtin-invert-channels-red-green-alpha = Red Green Alpha
parameter-builtin-invert-channels-red-blue-alpha = Red Blue Alpha
parameter-builtin-invert-channels-green-blue-alpha = Green Blue Alpha
parameter-builtin-invert-channels-red-green-blue-alpha = RGB + Alpha
parameter-builtin-invert-channels-hls = HLS
parameter-builtin-invert-channels-hue = Hue
parameter-builtin-invert-channels-lightness = Lightness
parameter-builtin-invert-channels-saturation = Saturation

## Noise 噪点
effect-noise = Noise
parameter-group-noise = Noise
effect-category-film-look = Film Look
parameter-mondrian-effect-builtin-noise-amount-label = Amount of Noise
parameter-mondrian-effect-builtin-noise-clip-label = Clip Result Values
parameter-mondrian-effect-builtin-noise-noise-type-label = Noise Type
parameter-mondrian-effect-noise-type-rgb = RGB Noise
parameter-mondrian-effect-noise-type-hls = HLS Noise

## Radial Blur 径向模糊
effect-radial-blur = Radial Blur
parameter-group-radial-blur = Radial Blur
parameter-mondrian-effect-builtin-radial-blur-amount-label = Amount
parameter-mondrian-effect-builtin-radial-blur-antialiasing-label = Anti-aliasing (Best Quality)
parameter-builtin-radial-blur-antialiasing-fast = Fast
parameter-builtin-radial-blur-antialiasing-best = Best Quality
parameter-mondrian-effect-builtin-radial-blur-centre-x-label = Center X
parameter-mondrian-effect-builtin-radial-blur-centre-y-label = Center Y
parameter-mondrian-effect-builtin-radial-blur-mode-label = Blur Method
parameter-builtin-radial-blur-mode-spin = Spin
parameter-builtin-radial-blur-mode-zoom = Zoom
parameter-mondrian-effect-builtin-radial-blur-seed-label = Random Seed

## Drop Shadow 投影
effect-drop-shadow = Drop Shadow
parameter-group-drop-shadow = Drop Shadow
effect-category-glow = Light Effects
parameter-mondrian-effect-builtin-drop-shadow-color-label = Shadow Color
parameter-mondrian-effect-builtin-drop-shadow-direction-label = Direction
parameter-mondrian-effect-builtin-drop-shadow-distance-label = Distance
parameter-mondrian-effect-builtin-drop-shadow-opacity-label = Opacity
parameter-mondrian-effect-builtin-drop-shadow-shadow-only-label = Shadow Only
parameter-mondrian-effect-builtin-drop-shadow-softness-label = Softness

## Extract 提取
effect-extract = Extract
parameter-group-extract = Extract
parameter-mondrian-effect-builtin-extract-black-point-label = Black Point
parameter-mondrian-effect-builtin-extract-black-softness-label = Black Softness
parameter-mondrian-effect-builtin-extract-channel-label = Channel
parameter-mondrian-effect-extract-channel-luma = Luminance
parameter-mondrian-effect-extract-channel-red = Red
parameter-mondrian-effect-extract-channel-green = Green
parameter-mondrian-effect-extract-channel-blue = Blue
parameter-mondrian-effect-extract-channel-alpha = Alpha
parameter-mondrian-effect-builtin-extract-invert-label = Invert
parameter-mondrian-effect-extract-invert-off = Off
parameter-mondrian-effect-extract-invert-on = On
parameter-mondrian-effect-builtin-extract-white-point-label = White Point
parameter-mondrian-effect-builtin-extract-white-softness-label = White Softness

## Scatter 散射
effect-scatter = Scatter
parameter-group-scatter = Scatter
parameter-mondrian-effect-builtin-scatter-amount-label = Scatter Amount
parameter-mondrian-effect-builtin-scatter-copies-label = Copies
parameter-mondrian-effect-builtin-scatter-randomness-label = Scatter Randomness

## Bevel and Emboss 斜面与浮雕
effect-bevel = Bevel and Emboss
parameter-group-bevel = Bevel and Emboss
effect-category-edge = Edge Effects
parameter-mondrian-effect-builtin-bevel-angle-label = Light Angle
parameter-mondrian-effect-builtin-bevel-intensity-label = Light Intensity
parameter-mondrian-effect-builtin-bevel-light-color-label = Light Color
parameter-mondrian-effect-builtin-bevel-thickness-label = Edge Thickness

## Directional Blur 方向模糊
effect-directional-blur = Directional Blur
parameter-group-directional-blur = Directional Blur
parameter-mondrian-effect-builtin-directional-blur-direction-label = Direction
parameter-mondrian-effect-builtin-directional-blur-length-label = Blur Length

## Box Blur 方框模糊
effect-box-blur = Box Blur
parameter-group-box-blur = Box Blur
parameter-mondrian-effect-builtin-box-blur-direction-label = Blur Direction
parameter-builtin-box-blur-direction-horizontal-and-vertical = Horizontal and Vertical
parameter-builtin-box-blur-direction-horizontal = Horizontal
parameter-builtin-box-blur-direction-vertical = Vertical
parameter-mondrian-effect-builtin-box-blur-iterations-label = Iterations
parameter-mondrian-effect-builtin-box-blur-radius-label = Blur Radius
parameter-mondrian-effect-builtin-box-blur-repeat-edge-pixels-label = Repeat Edge Pixels

## Twirl 旋转扭曲
effect-twirl = Twirl
parameter-group-twirl = Twirl
parameter-mondrian-effect-builtin-twirl-angle-label = Angle
parameter-mondrian-effect-builtin-twirl-centre-x-label = Center X
parameter-mondrian-effect-builtin-twirl-centre-y-label = Center Y
parameter-mondrian-effect-builtin-twirl-radius-label = Twirl Radius

## Polar Coordinates 极坐标
effect-polar-coordinates = Polar Coordinates
parameter-group-polar-coordinates = Polar Coordinates
parameter-mondrian-effect-builtin-polar-coordinates-conversion-label = Conversion Type
parameter-mondrian-effect-polar-coordinates-conversion-rect-to-polar = Rect to Polar
parameter-mondrian-effect-polar-coordinates-conversion-polar-to-rect = Polar to Rect
parameter-mondrian-effect-builtin-polar-coordinates-interpolation-label = Interpolation
parameter-mondrian-effect-polar-coordinates-interpolation-nearest = Nearest
parameter-mondrian-effect-polar-coordinates-interpolation-bilinear = Bilinear

## Find Edges 查找边缘
effect-find-edges = Find Edges
parameter-group-find-edges = Find Edges
parameter-mondrian-effect-builtin-find-edges-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-find-edges-invert-label = Invert

## Emboss 浮雕
effect-emboss = Emboss
parameter-group-emboss = Emboss
parameter-mondrian-effect-builtin-emboss-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-emboss-contrast-label = Contrast
parameter-mondrian-effect-builtin-emboss-direction-label = Direction
parameter-mondrian-effect-builtin-emboss-relief-label = Relief

## Spherize 球面化
effect-spherize = Spherize
parameter-group-spherize = Spherize
parameter-mondrian-effect-builtin-spherize-centre-x-label = Center X
parameter-mondrian-effect-builtin-spherize-centre-y-label = Center Y
parameter-mondrian-effect-builtin-spherize-radius-label = Radius

## Magnify 放大
effect-magnify = Magnify
parameter-group-magnify = Magnify
parameter-mondrian-effect-builtin-magnify-blend-mode-label = Blend Mode
parameter-mondrian-effect-builtin-magnify-centre-x-label = Center X
parameter-mondrian-effect-builtin-magnify-centre-y-label = Center Y
parameter-mondrian-effect-builtin-magnify-feather-label = Feather
parameter-mondrian-effect-builtin-magnify-link-label = Link
parameter-mondrian-effect-builtin-magnify-magnification-label = Magnification
parameter-mondrian-effect-builtin-magnify-opacity-label = Opacity
parameter-mondrian-effect-builtin-magnify-resize-layer-label = Resize Layer
parameter-mondrian-effect-builtin-magnify-scaling-label = Scaling
parameter-mondrian-effect-builtin-magnify-shape-label = Shape
parameter-mondrian-effect-builtin-magnify-size-label = Size
parameter-mondrian-effect-magnify-blend-mode-normal = Normal
parameter-mondrian-effect-magnify-blend-mode-multiply = Multiply
parameter-mondrian-effect-magnify-blend-mode-screen = Screen
parameter-mondrian-effect-magnify-blend-mode-overlay = Overlay
parameter-mondrian-effect-magnify-blend-mode-darken = Darken
parameter-mondrian-effect-magnify-blend-mode-lighten = Lighten
parameter-mondrian-effect-magnify-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-magnify-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-magnify-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-magnify-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-magnify-blend-mode-difference = Difference
parameter-mondrian-effect-magnify-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-magnify-blend-mode-subtract = Subtract
parameter-mondrian-effect-magnify-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-magnify-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-magnify-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-magnify-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-magnify-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-magnify-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-magnify-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-magnify-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-magnify-blend-mode-divide = Divide
parameter-mondrian-effect-magnify-link-follow-magnification = Follow Magnification
parameter-mondrian-effect-magnify-link-independent = Independent
parameter-mondrian-effect-magnify-link-inverse-magnification = Inverse Magnification
parameter-mondrian-effect-magnify-scaling-bilinear = Bilinear
parameter-mondrian-effect-magnify-scaling-supersample-2 = 2×2 Supersample
parameter-mondrian-effect-magnify-scaling-supersample-4 = 4×4 Supersample
parameter-mondrian-effect-magnify-shape-circle = Circle
parameter-mondrian-effect-magnify-shape-square = Square

## Brush Strokes 画笔描边
effect-brush-strokes = Brush Strokes
parameter-group-brush-strokes = Brush Strokes
parameter-mondrian-effect-builtin-brush-strokes-angle-label = Stroke Direction
parameter-mondrian-effect-builtin-brush-strokes-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-brush-strokes-density-label = Brush Density
parameter-mondrian-effect-builtin-brush-strokes-length-label = Brush Length
parameter-mondrian-effect-builtin-brush-strokes-randomness-label = Randomness
parameter-mondrian-effect-builtin-brush-strokes-size-label = Brush Size
parameter-mondrian-effect-builtin-brush-strokes-surface-label = Brush Surface

## Linear Color Key 线性颜色键
effect-linear-color-key = Linear Color Key
parameter-group-linear-color-key = Linear Color Key
parameter-mondrian-effect-builtin-linear-color-key-key-colour-label = Key Color
parameter-mondrian-effect-builtin-linear-color-key-match-colours-label = Match Colors
parameter-mondrian-effect-linear-color-key-match-rgb = RGB
parameter-mondrian-effect-linear-color-key-match-green = Green
parameter-mondrian-effect-linear-color-key-match-blue = Blue
parameter-mondrian-effect-linear-color-key-match-chroma = Chroma
parameter-mondrian-effect-builtin-linear-color-key-operation-label = Key Operation
parameter-mondrian-effect-linear-color-key-operation-key = Key Colors
parameter-mondrian-effect-linear-color-key-operation-keep = Keep Colors
parameter-mondrian-effect-builtin-linear-color-key-softness-label = Matching Softness
parameter-mondrian-effect-builtin-linear-color-key-tolerance-label = Matching Tolerance

## Vibrance 自然饱和度
effect-vibrance = Vibrance
parameter-group-vibrance = Vibrance
parameter-mondrian-effect-builtin-vibrance-saturation-label = Saturation
parameter-mondrian-effect-builtin-vibrance-vibrance-label = Vibrance

## Color Emboss 色彩浮雕
effect-color-emboss = Color Emboss
parameter-group-color-emboss = Color Emboss
parameter-mondrian-effect-builtin-color-emboss-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-color-emboss-contrast-label = Contrast
parameter-mondrian-effect-builtin-color-emboss-direction-label = Direction
parameter-mondrian-effect-builtin-color-emboss-relief-label = Relief

## Hue/Saturation 色相饱和度
effect-hue-saturation = Hue/Saturation
parameter-group-hue-saturation = Hue/Saturation
parameter-mondrian-effect-builtin-hue-saturation-band-label = Channel Control
parameter-builtin-hue-saturation-band-master = Master
parameter-builtin-hue-saturation-band-reds = Reds
parameter-builtin-hue-saturation-band-yellows = Yellows
parameter-builtin-hue-saturation-band-greens = Greens
parameter-builtin-hue-saturation-band-cyans = Cyans
parameter-builtin-hue-saturation-band-blues = Blues
parameter-builtin-hue-saturation-band-magentas = Magentas
parameter-mondrian-effect-builtin-hue-saturation-colorize-label = Colorize
parameter-mondrian-effect-builtin-hue-saturation-hue-label = Hue
parameter-mondrian-effect-builtin-hue-saturation-lightness-label = Lightness
parameter-mondrian-effect-builtin-hue-saturation-saturation-label = Saturation
parameter-mondrian-effect-builtin-hue-saturation-tint-hue-label = Tint Hue
parameter-mondrian-effect-builtin-hue-saturation-tint-lightness-label = Tint Lightness
parameter-mondrian-effect-builtin-hue-saturation-tint-saturation-label = Tint Saturation

## Tint 色调
effect-tint = Tint
parameter-group-tint = Tint
parameter-mondrian-effect-builtin-tint-amount-label = Amount to Tint
parameter-mondrian-effect-builtin-tint-map-black-label = Map Black To
parameter-mondrian-effect-builtin-tint-map-white-label = Map White To

## Posterize 色调分离
effect-posterize = Posterize
parameter-group-posterize = Posterize
parameter-mondrian-effect-builtin-posterize-levels-label = Levels

## Shift Channels 转换通道
effect-shift-channels = Shift Channels
parameter-group-shift-channels = Shift Channels
parameter-mondrian-effect-builtin-shift-channels-alpha-label = Take Alpha From
parameter-builtin-shift-channels-alpha-red = Red
parameter-builtin-shift-channels-alpha-green = Green
parameter-builtin-shift-channels-alpha-blue = Blue
parameter-builtin-shift-channels-alpha-alpha = Alpha
parameter-builtin-shift-channels-alpha-luminance = Luminance
parameter-builtin-shift-channels-alpha-hue = Hue
parameter-builtin-shift-channels-alpha-saturation = Saturation
parameter-builtin-shift-channels-alpha-lightness = Lightness
parameter-builtin-shift-channels-alpha-maximum = Maximum
parameter-builtin-shift-channels-alpha-minimum = Minimum
parameter-mondrian-effect-builtin-shift-channels-blue-label = Take Blue From
parameter-builtin-shift-channels-blue-red = Red
parameter-builtin-shift-channels-blue-green = Green
parameter-builtin-shift-channels-blue-blue = Blue
parameter-builtin-shift-channels-blue-alpha = Alpha
parameter-builtin-shift-channels-blue-luminance = Luminance
parameter-builtin-shift-channels-blue-hue = Hue
parameter-builtin-shift-channels-blue-saturation = Saturation
parameter-builtin-shift-channels-blue-lightness = Lightness
parameter-builtin-shift-channels-blue-maximum = Maximum
parameter-builtin-shift-channels-blue-minimum = Minimum
parameter-mondrian-effect-builtin-shift-channels-green-label = Take Green From
parameter-builtin-shift-channels-green-red = Red
parameter-builtin-shift-channels-green-green = Green
parameter-builtin-shift-channels-green-blue = Blue
parameter-builtin-shift-channels-green-alpha = Alpha
parameter-builtin-shift-channels-green-luminance = Luminance
parameter-builtin-shift-channels-green-hue = Hue
parameter-builtin-shift-channels-green-saturation = Saturation
parameter-builtin-shift-channels-green-lightness = Lightness
parameter-builtin-shift-channels-green-maximum = Maximum
parameter-builtin-shift-channels-green-minimum = Minimum
parameter-mondrian-effect-builtin-shift-channels-red-label = Take Red From
parameter-builtin-shift-channels-red-red = Red
parameter-builtin-shift-channels-red-green = Green
parameter-builtin-shift-channels-red-blue = Blue
parameter-builtin-shift-channels-red-alpha = Alpha
parameter-builtin-shift-channels-red-luminance = Luminance
parameter-builtin-shift-channels-red-hue = Hue
parameter-builtin-shift-channels-red-saturation = Saturation
parameter-builtin-shift-channels-red-lightness = Lightness
parameter-builtin-shift-channels-red-maximum = Maximum
parameter-builtin-shift-channels-red-minimum = Minimum

## Channel Combiner 通道合成器
effect-channel-combiner = Channel Combiner
parameter-group-channel-combiner = Channel Combiner
parameter-group-second-layer = Second Layer
parameter-mondrian-effect-builtin-channel-combiner-from-label = From
parameter-mondrian-effect-channel-combiner-from-alpha = Alpha
parameter-mondrian-effect-channel-combiner-from-red = Red
parameter-mondrian-effect-channel-combiner-from-green = Green
parameter-mondrian-effect-channel-combiner-from-blue = Blue
parameter-mondrian-effect-channel-combiner-from-luminance = Luminance
parameter-mondrian-effect-channel-combiner-from-lightness = Lightness
parameter-mondrian-effect-channel-combiner-from-saturation = Saturation
parameter-mondrian-effect-channel-combiner-from-hue = Hue
parameter-mondrian-effect-channel-combiner-from-red-green = Red Green
parameter-mondrian-effect-channel-combiner-from-red-blue = Red Blue
parameter-mondrian-effect-channel-combiner-from-alpha-red = Alpha Red
parameter-mondrian-effect-channel-combiner-from-green-blue = Green Blue
parameter-mondrian-effect-channel-combiner-from-alpha-green = Alpha Green
parameter-mondrian-effect-channel-combiner-from-alpha-blue = Alpha Blue
parameter-mondrian-effect-channel-combiner-from-red-green-blue = Red Green Blue
parameter-mondrian-effect-channel-combiner-from-alpha-red-green = Alpha Red Green
parameter-mondrian-effect-channel-combiner-from-alpha-red-blue = Alpha Red Blue
parameter-mondrian-effect-channel-combiner-from-alpha-green-blue = Alpha Green Blue
parameter-mondrian-effect-channel-combiner-from-red-green-blue-alpha = Red Green Blue Alpha
parameter-mondrian-effect-builtin-channel-combiner-invert-label = Invert
parameter-mondrian-effect-builtin-channel-combiner-solid-alpha-label = Solid Alpha
parameter-mondrian-effect-builtin-channel-combiner-source-layer-label = Source Layer
parameter-mondrian-effect-builtin-channel-combiner-to-label = To
parameter-mondrian-effect-channel-combiner-to-alpha = Alpha
parameter-mondrian-effect-channel-combiner-to-red = Red
parameter-mondrian-effect-channel-combiner-to-green = Green
parameter-mondrian-effect-channel-combiner-to-blue = Blue
parameter-mondrian-effect-channel-combiner-to-luminance = Luminance
parameter-mondrian-effect-channel-combiner-to-lightness = Lightness
parameter-mondrian-effect-channel-combiner-to-red-green = Red Green
parameter-mondrian-effect-channel-combiner-to-red-blue = Red Blue
parameter-mondrian-effect-channel-combiner-to-alpha-red = Alpha Red
parameter-mondrian-effect-channel-combiner-to-green-blue = Green Blue
parameter-mondrian-effect-channel-combiner-to-alpha-green = Alpha Green
parameter-mondrian-effect-channel-combiner-to-alpha-blue = Alpha Blue
parameter-mondrian-effect-channel-combiner-to-red-green-blue = Red Green Blue
parameter-mondrian-effect-channel-combiner-to-alpha-red-green = Alpha Red Green
parameter-mondrian-effect-channel-combiner-to-alpha-red-blue = Alpha Red Blue
parameter-mondrian-effect-channel-combiner-to-alpha-green-blue = Alpha Green Blue
parameter-mondrian-effect-channel-combiner-to-red-green-blue-alpha = Red Green Blue Alpha
parameter-mondrian-effect-builtin-channel-combiner-use-second-layer-label = Use Second Layer

## Channel Mixer 通道混合器
effect-channel-mixer = Channel Mixer
parameter-group-channel-mixer = Channel Mixer
parameter-mondrian-effect-builtin-channel-mixer-blue-blue-label = Blue - Blue
parameter-mondrian-effect-builtin-channel-mixer-blue-constant-label = Blue - Constant
parameter-mondrian-effect-builtin-channel-mixer-blue-green-label = Blue - Green
parameter-mondrian-effect-builtin-channel-mixer-blue-red-label = Blue - Red
parameter-mondrian-effect-builtin-channel-mixer-green-blue-label = Green - Blue
parameter-mondrian-effect-builtin-channel-mixer-green-constant-label = Green - Constant
parameter-mondrian-effect-builtin-channel-mixer-green-green-label = Green - Green
parameter-mondrian-effect-builtin-channel-mixer-green-red-label = Green - Red
parameter-mondrian-effect-builtin-channel-mixer-monochrome-label = Monochrome
parameter-mondrian-effect-builtin-channel-mixer-red-blue-label = Red - Blue
parameter-mondrian-effect-builtin-channel-mixer-red-constant-label = Red - Constant
parameter-mondrian-effect-builtin-channel-mixer-red-green-label = Red - Green
parameter-mondrian-effect-builtin-channel-mixer-red-red-label = Red - Red

## Unsharp Mask 钝化蒙版
effect-unsharp-mask = Unsharp Mask
parameter-group-unsharp-mask = Unsharp Mask
parameter-mondrian-effect-builtin-unsharp-mask-amount-label = Amount
parameter-mondrian-effect-builtin-unsharp-mask-radius-label = Radius
parameter-mondrian-effect-builtin-unsharp-mask-threshold-label = Threshold

## Mirror 镜像
effect-mirror = Mirror
parameter-group-mirror = Mirror
parameter-mondrian-effect-builtin-mirror-angle-label = Reflection Angle
parameter-mondrian-effect-builtin-mirror-centre-x-label = Reflection Center X
parameter-mondrian-effect-builtin-mirror-centre-y-label = Reflection Center Y

## Lens Flare 镜头光晕
effect-lens-flare = Lens Flare
parameter-group-lens-flare = Lens Flare
parameter-mondrian-effect-builtin-lens-flare-blend-with-original-label = Blend With Original
parameter-mondrian-effect-builtin-lens-flare-brightness-label = Flare Brightness
parameter-mondrian-effect-builtin-lens-flare-centre-x-label = Flare Center X
parameter-mondrian-effect-builtin-lens-flare-centre-y-label = Flare Center Y
parameter-mondrian-effect-builtin-lens-flare-lens-type-label = Lens Type
parameter-mondrian-effect-lens-flare-lens-type-zoom-50-300 = 50-300mm Zoom
parameter-mondrian-effect-lens-flare-lens-type-prime-35 = 35mm Prime
parameter-mondrian-effect-lens-flare-lens-type-prime-105 = 105mm Prime

## Lens Distortion 镜头畸变
effect-lens-distortion = Lens Distortion
parameter-group-lens-distortion = Lens Distortion
parameter-mondrian-effect-builtin-lens-distortion-best-pixels-label = Best Pixels
parameter-mondrian-effect-lens-distortion-best-pixels-off = Off
parameter-mondrian-effect-lens-distortion-best-pixels-on = On
parameter-mondrian-effect-builtin-lens-distortion-centre-x-label = View Center X
parameter-mondrian-effect-builtin-lens-distortion-centre-y-label = View Center Y
parameter-mondrian-effect-builtin-lens-distortion-fov-label = Field of View (FOV)
parameter-mondrian-effect-builtin-lens-distortion-fov-direction-label = FOV Direction
parameter-mondrian-effect-lens-distortion-direction-horizontal = Horizontal
parameter-mondrian-effect-lens-distortion-direction-vertical = Vertical
parameter-mondrian-effect-lens-distortion-direction-diagonal = Diagonal
parameter-mondrian-effect-builtin-lens-distortion-invert-label = Invert Lens Distortion
parameter-mondrian-effect-lens-distortion-invert-off = Off
parameter-mondrian-effect-lens-distortion-invert-on = On
parameter-mondrian-effect-builtin-lens-distortion-resize-label = Resize
parameter-mondrian-effect-lens-distortion-resize-none = None
parameter-mondrian-effect-lens-distortion-resize-expand = Expand
parameter-mondrian-effect-lens-distortion-resize-contract = Contract
parameter-mondrian-effect-lens-distortion-resize-auto = Auto

## Threshold 阈值
effect-threshold = Threshold
parameter-group-threshold = Threshold
parameter-mondrian-effect-builtin-threshold-level-label = Level

## Color Offset 颜色偏移
effect-color-offset = Color Offset
parameter-group-color-offset = Color Offset
parameter-mondrian-effect-builtin-color-offset-blue-phase-label = Blue Phase
parameter-mondrian-effect-builtin-color-offset-green-phase-label = Green Phase
parameter-mondrian-effect-builtin-color-offset-overflow-label = Overflow
parameter-mondrian-effect-color-offset-overflow-wrap = Wrap
parameter-mondrian-effect-color-offset-overflow-clamp = Clamp
parameter-mondrian-effect-color-offset-overflow-mirror = Mirror
parameter-mondrian-effect-builtin-color-offset-red-phase-label = Red Phase

## Mosaic 马赛克
effect-mosaic = Mosaic
parameter-group-mosaic = Mosaic
parameter-mondrian-effect-builtin-mosaic-horizontal-label = Horizontal Blocks
parameter-mondrian-effect-builtin-mosaic-sharp-colors-label = Sharp Colors
parameter-mondrian-effect-builtin-mosaic-vertical-label = Vertical Blocks

## Black & White 黑白
effect-black-and-white = Black & White
parameter-group-black-and-white = Black & White
parameter-mondrian-effect-builtin-black-and-white-blue-label = Blues
parameter-mondrian-effect-builtin-black-and-white-cyan-label = Cyans
parameter-mondrian-effect-builtin-black-and-white-green-label = Greens
parameter-mondrian-effect-builtin-black-and-white-magenta-label = Magentas
parameter-mondrian-effect-builtin-black-and-white-red-label = Reds
parameter-mondrian-effect-builtin-black-and-white-tint-amount-label = Tint Amount
parameter-mondrian-effect-builtin-black-and-white-tint-color-label = Tint Color
parameter-mondrian-effect-builtin-black-and-white-yellow-label = Yellows

## Choker 阻塞收缩
effect-choker = Choker
parameter-group-choker = Choker
parameter-mondrian-effect-builtin-choker-choke-label = Choke Matte

## Refine Matte 遮罩精修
effect-matte-refine = Refine Matte
parameter-group-matte-refine = Refine Matte
parameter-mondrian-effect-builtin-matte-refine-better-quality-label = Better Quality
parameter-mondrian-effect-builtin-matte-refine-clean-amount-label = Clean Amount
parameter-mondrian-effect-builtin-matte-refine-clean-edge-color-label = Clean Edge Color
parameter-mondrian-effect-builtin-matte-refine-contrast-label = Contrast
parameter-mondrian-effect-builtin-matte-refine-edge-detail-label = Calculate Edge Detail
parameter-mondrian-effect-builtin-matte-refine-expand-radius-label = Expand Radius
parameter-mondrian-effect-builtin-matte-refine-extend-smooth-label = Extend Smooth
parameter-mondrian-effect-builtin-matte-refine-feather-label = Feather
parameter-mondrian-effect-builtin-matte-refine-more-motion-blur-label = More Motion Blur
parameter-mondrian-effect-builtin-matte-refine-other-edge-radius-label = Other Edge Radius
parameter-mondrian-effect-builtin-matte-refine-per-frame-samples-label = Samples Per Frame
parameter-mondrian-effect-builtin-matte-refine-shift-edge-label = Shift Edge
parameter-mondrian-effect-builtin-matte-refine-shutter-angle-degrees-label = Shutter Angle
parameter-mondrian-effect-builtin-matte-refine-smooth-label = Smooth
parameter-mondrian-effect-builtin-matte-refine-tremor-amount-label = Reduce Tremor
parameter-mondrian-effect-builtin-matte-refine-tremor-reduction-label = Tremor Reduction
parameter-mondrian-effect-builtin-matte-refine-view-edge-region-label = View Edge Region
parameter-mondrian-effect-builtin-matte-refine-view-map-label = View Map

## Arithmetic 算术
effect-arithmetic = Arithmetic
parameter-group-channel = Channel
parameter-mondrian-effect-builtin-arithmetic-blue-label = Blue Value
parameter-mondrian-effect-builtin-arithmetic-clipping-label = Clip Result Values
parameter-mondrian-effect-builtin-arithmetic-green-label = Green Value
parameter-mondrian-effect-builtin-arithmetic-operator-label = Operator
parameter-mondrian-effect-builtin-arithmetic-red-label = Red Value
parameter-mondrian-effect-arithmetic-operator-add = Add
parameter-mondrian-effect-arithmetic-operator-and = And
parameter-mondrian-effect-arithmetic-operator-block-above = Block Above
parameter-mondrian-effect-arithmetic-operator-block-below = Block Below
parameter-mondrian-effect-arithmetic-operator-difference = Difference
parameter-mondrian-effect-arithmetic-operator-max = Maximum
parameter-mondrian-effect-arithmetic-operator-min = Minimum
parameter-mondrian-effect-arithmetic-operator-multiply = Multiply
parameter-mondrian-effect-arithmetic-operator-or = Or
parameter-mondrian-effect-arithmetic-operator-screen = Screen
parameter-mondrian-effect-arithmetic-operator-slice = Slice
parameter-mondrian-effect-arithmetic-operator-subtract = Subtract
parameter-mondrian-effect-arithmetic-operator-xor = Xor

## Leave Color 保留颜色
effect-leave-color = Leave Color
parameter-group-leave-color = Leave Color
parameter-mondrian-effect-builtin-leave-color-amount-label = Amount
parameter-mondrian-effect-builtin-leave-color-colour-label = Kept Color
parameter-mondrian-effect-builtin-leave-color-edge-softness-label = Edge Softness
parameter-mondrian-effect-builtin-leave-color-match-label = Match
parameter-mondrian-effect-builtin-leave-color-tolerance-label = Tolerance
parameter-mondrian-effect-leave-color-match-hue = Use Hue
parameter-mondrian-effect-leave-color-match-rgb = Use RGB

## Change to Color 定向改色
effect-change-to-color = Change to Color
parameter-group-change-to-color = Change to Color
parameter-mondrian-effect-builtin-change-to-color-change-by-label = Change By
parameter-mondrian-effect-builtin-change-to-color-change-label = Change
parameter-mondrian-effect-builtin-change-to-color-from-label = From
parameter-mondrian-effect-builtin-change-to-color-hue-label = Hue
parameter-mondrian-effect-builtin-change-to-color-lightness-label = Lightness
parameter-mondrian-effect-builtin-change-to-color-saturation-label = Saturation
parameter-mondrian-effect-builtin-change-to-color-softness-label = Softness
parameter-mondrian-effect-builtin-change-to-color-to-label = To
parameter-mondrian-effect-builtin-change-to-color-view-matte-label = View Correction Mask
parameter-mondrian-effect-change-to-color-change-by-absolute = Replace with To Colour
parameter-mondrian-effect-change-to-color-change-by-relative = Relative Offset
parameter-mondrian-effect-change-to-color-change-hue = Hue
parameter-mondrian-effect-change-to-color-change-hue-lightness = Hue & Lightness
parameter-mondrian-effect-change-to-color-change-hue-lightness-saturation = Hue, Lightness & Saturation
parameter-mondrian-effect-change-to-color-change-hue-saturation = Hue & Saturation

## Broadcast Safe 广播安全
effect-broadcast-safe = Broadcast Safe
parameter-group-broadcast-safe = Broadcast Safe
parameter-mondrian-effect-builtin-broadcast-safe-ire-label = Maximum Signal Amplitude (IRE)
parameter-mondrian-effect-builtin-broadcast-safe-locale-label = Broadcast Locale
parameter-mondrian-effect-builtin-broadcast-safe-mode-label = How to Make Colors Safe
parameter-mondrian-effect-broadcast-safe-locale-ntsc = NTSC
parameter-mondrian-effect-broadcast-safe-locale-pal = PAL
parameter-mondrian-effect-broadcast-safe-mode-clamp-rgb = Clamp Each Channel
parameter-mondrian-effect-broadcast-safe-mode-key-out-safe = Key Out Safe
parameter-mondrian-effect-broadcast-safe-mode-key-out-unsafe = Key Out Unsafe
parameter-mondrian-effect-broadcast-safe-mode-reduce-luminance = Reduce Luminance
parameter-mondrian-effect-broadcast-safe-mode-reduce-saturation = Reduce Saturation

## Test Signals 彩条与测试信号
effect-test-signals = Test Signals
parameter-group-test-signals = Test Signals
parameter-mondrian-effect-builtin-test-signals-display-labels-label = Show Calibration Marks
parameter-mondrian-effect-builtin-test-signals-intensity-label = Intensity
parameter-mondrian-effect-builtin-test-signals-pattern-label = Test Pattern
parameter-mondrian-effect-builtin-test-signals-safe-frames-label = Show Safe Frames
parameter-mondrian-effect-test-signals-pattern-bars = Color Bars
parameter-mondrian-effect-test-signals-pattern-checkerboard = Checkerboard
parameter-mondrian-effect-test-signals-pattern-concentric-rings = Concentric Rings
parameter-mondrian-effect-test-signals-pattern-grayscale = Grayscale
parameter-mondrian-effect-test-signals-pattern-hdr-staircase = HDR Staircase
parameter-mondrian-effect-test-signals-pattern-zone-plate = Resolution Chart

## Color Balance 颜色平衡
effect-color-balance = Color Balance
parameter-group-color-balance = Color Balance
parameter-mondrian-effect-builtin-color-balance-highlight-blue-label = Highlight Blue Balance
parameter-mondrian-effect-builtin-color-balance-highlight-green-label = Highlight Green Balance
parameter-mondrian-effect-builtin-color-balance-highlight-red-label = Highlight Red Balance
parameter-mondrian-effect-builtin-color-balance-midtone-blue-label = Midtone Blue Balance
parameter-mondrian-effect-builtin-color-balance-midtone-green-label = Midtone Green Balance
parameter-mondrian-effect-builtin-color-balance-midtone-red-label = Midtone Red Balance
parameter-mondrian-effect-builtin-color-balance-preserve-luminosity-label = Preserve Luminosity
parameter-mondrian-effect-builtin-color-balance-shadow-blue-label = Shadow Blue Balance
parameter-mondrian-effect-builtin-color-balance-shadow-green-label = Shadow Green Balance
parameter-mondrian-effect-builtin-color-balance-shadow-red-label = Shadow Red Balance

## Minimax 最小/最大
effect-minimax = Minimax
parameter-group-minimax = Minimax
parameter-mondrian-effect-builtin-minimax-channel-label = Channel
parameter-mondrian-effect-builtin-minimax-direction-label = Direction
parameter-mondrian-effect-builtin-minimax-dont-shrink-edges-label = Don't Shrink Edges
parameter-mondrian-effect-builtin-minimax-operation-label = Operation
parameter-mondrian-effect-builtin-minimax-radius-label = Radius
parameter-mondrian-effect-minimax-channel-alpha = Alpha
parameter-mondrian-effect-minimax-channel-all = All
parameter-mondrian-effect-minimax-channel-blue = Blue
parameter-mondrian-effect-minimax-channel-color = Red Green Blue
parameter-mondrian-effect-minimax-channel-green = Green
parameter-mondrian-effect-minimax-channel-red = Red
parameter-mondrian-effect-minimax-direction-both = Both
parameter-mondrian-effect-minimax-direction-horizontal = Horizontal
parameter-mondrian-effect-minimax-direction-vertical = Vertical
parameter-mondrian-effect-minimax-operation-maximum = Maximum
parameter-mondrian-effect-minimax-operation-maximum-then-minimum = Maximum then Minimum
parameter-mondrian-effect-minimax-operation-minimum = Minimum
parameter-mondrian-effect-minimax-operation-minimum-then-maximum = Minimum then Maximum

## Spill Suppressor 溢色抑制
effect-spill-suppressor = Spill Suppressor
parameter-group-spill-suppressor = Spill Suppressor
parameter-mondrian-effect-builtin-spill-suppressor-colour-correction-label = Colour Correction
parameter-mondrian-effect-builtin-spill-suppressor-desaturation-label = Desaturation
parameter-mondrian-effect-builtin-spill-suppressor-key-colour-label = Key Colour
parameter-mondrian-effect-builtin-spill-suppressor-luminance-correction-label = Luminance Correction
parameter-mondrian-effect-builtin-spill-suppressor-method-label = Method
parameter-mondrian-effect-builtin-spill-suppressor-spill-range-label = Spill Range
parameter-mondrian-effect-builtin-spill-suppressor-suppression-label = Suppression
parameter-mondrian-effect-builtin-spill-suppressor-tolerance-label = Tolerance
parameter-mondrian-effect-spill-suppressor-method-color-only = Colour Only
parameter-mondrian-effect-spill-suppressor-method-final-result = Final Result

## Channel Blur 通道模糊
effect-channel-blur = Channel Blur
parameter-group-channel-blur = Channel Blur
parameter-mondrian-effect-builtin-channel-blur-alpha-blur-label = Alpha Blur
parameter-mondrian-effect-builtin-channel-blur-blue-blur-label = Blue Blur
parameter-mondrian-effect-builtin-channel-blur-direction-label = Direction
parameter-mondrian-effect-builtin-channel-blur-edge-behavior-label = Edge Behavior
parameter-mondrian-effect-builtin-channel-blur-green-blur-label = Green Blur
parameter-mondrian-effect-builtin-channel-blur-red-blur-label = Red Blur
parameter-mondrian-effect-channel-blur-direction-both = Horizontal and Vertical
parameter-mondrian-effect-channel-blur-direction-horizontal = Horizontal
parameter-mondrian-effect-channel-blur-direction-vertical = Vertical

## Dent 凹痕
effect-dent = Dent
parameter-group-dent = Dent
parameter-mondrian-effect-builtin-dent-kind-label = Dent Type
parameter-mondrian-effect-dent-kind-dent = Pit
parameter-mondrian-effect-dent-kind-bulge = Bump
parameter-mondrian-effect-builtin-dent-centre-x-label = Position X
parameter-mondrian-effect-builtin-dent-centre-y-label = Position Y
parameter-mondrian-effect-builtin-dent-size-label = Size
parameter-mondrian-effect-builtin-dent-strength-label = Strength
parameter-mondrian-effect-builtin-dent-edge-label = Edge Handling
parameter-mondrian-effect-dent-edge-smooth = Smooth
parameter-mondrian-effect-dent-edge-hard = Hard

## Median 中间值
effect-median = Median
parameter-group-median = Median
parameter-mondrian-effect-builtin-median-radius-label = Radius
parameter-mondrian-effect-builtin-median-operate-on-alpha-label = Operate on Alpha Channel

## Dust & Scratches 蒙尘与划痕
effect-dust-scratches = Dust & Scratches
parameter-group-dust-scratches = Dust & Scratches
parameter-mondrian-effect-builtin-dust-scratches-radius-label = Radius
parameter-mondrian-effect-builtin-dust-scratches-threshold-label = Threshold
parameter-mondrian-effect-builtin-dust-scratches-operate-on-alpha-label = Operate on Alpha Channel

## Key Cleaner 键控清理
effect-key-cleaner = Key Cleaner
parameter-group-key-cleaner = Key Cleaner
effect-category-edge-repair = Edge Repair
parameter-mondrian-effect-builtin-key-cleaner-other-edge-radius-label = Other Edge Radius
parameter-mondrian-effect-builtin-key-cleaner-reduce-chatter-label = Reduce Chatter
parameter-mondrian-effect-builtin-key-cleaner-alpha-contrast-label = Alpha Contrast
parameter-mondrian-effect-builtin-key-cleaner-strength-label = Strength

## Warp 弯曲
effect-warp = Warp
parameter-group-warp = Warp
parameter-mondrian-effect-builtin-warp-style-label = Bend Style
parameter-mondrian-effect-warp-style-arc = Arc
parameter-mondrian-effect-warp-style-arc-lower = Arc Lower
parameter-mondrian-effect-warp-style-arc-upper = Arc Upper
parameter-mondrian-effect-warp-style-arch = Arch
parameter-mondrian-effect-warp-style-bulge = Bulge
parameter-mondrian-effect-warp-style-shell-lower = Shell Lower
parameter-mondrian-effect-warp-style-shell-upper = Shell Upper
parameter-mondrian-effect-warp-style-flag = Flag
parameter-mondrian-effect-warp-style-wave = Wave
parameter-mondrian-effect-warp-style-fish = Fish
parameter-mondrian-effect-warp-style-rise = Rise
parameter-mondrian-effect-warp-style-fisheye = Fisheye
parameter-mondrian-effect-warp-style-inflate = Inflate
parameter-mondrian-effect-warp-style-squeeze = Squeeze
parameter-mondrian-effect-warp-style-twist = Twist
parameter-mondrian-effect-builtin-warp-axis-label = Bend Axis
parameter-mondrian-effect-warp-axis-horizontal = Horizontal
parameter-mondrian-effect-warp-axis-vertical = Vertical
parameter-mondrian-effect-builtin-warp-bend-label = Bend
parameter-mondrian-effect-builtin-warp-horizontal-distortion-label = Horizontal Distortion
parameter-mondrian-effect-builtin-warp-vertical-distortion-label = Vertical Distortion

## Grid Warp 网格变形
effect-grid-warp = Grid Warp
parameter-group-grid-warp = Grid Warp
parameter-mondrian-effect-builtin-grid-warp-row-count-label = Rows
parameter-mondrian-effect-builtin-grid-warp-column-count-label = Columns
parameter-mondrian-effect-builtin-grid-warp-quality-label = Quality
parameter-mondrian-effect-grid-warp-quality-1 = 1
parameter-mondrian-effect-grid-warp-quality-2 = 2
parameter-mondrian-effect-grid-warp-quality-3 = 3
parameter-mondrian-effect-grid-warp-quality-4 = 4
parameter-mondrian-effect-grid-warp-quality-5 = 5
parameter-mondrian-effect-grid-warp-quality-6 = 6
parameter-mondrian-effect-grid-warp-quality-7 = 7
parameter-mondrian-effect-grid-warp-quality-8 = 8
parameter-mondrian-effect-grid-warp-quality-9 = 9
parameter-mondrian-effect-grid-warp-quality-10 = 10

effect-wire-removal = Wire Removal
parameter-group-wire-removal = Wire Removal
parameter-mondrian-effect-builtin-wire-removal-point-a-x-label = Point A X
parameter-mondrian-effect-builtin-wire-removal-point-a-y-label = Point A Y
parameter-mondrian-effect-builtin-wire-removal-point-b-x-label = Point B X
parameter-mondrian-effect-builtin-wire-removal-point-b-y-label = Point B Y
parameter-mondrian-effect-builtin-wire-removal-removal-style-label = Removal Style
parameter-mondrian-effect-builtin-wire-removal-thickness-label = Thickness
parameter-mondrian-effect-builtin-wire-removal-slope-label = Slope
parameter-mondrian-effect-builtin-wire-removal-mirror-blend-label = Mirror Blend
parameter-mondrian-effect-builtin-wire-removal-frame-offset-label = Frame Offset (not implemented)
parameter-mondrian-effect-wire-removal-removal-style-left = Left
parameter-mondrian-effect-wire-removal-removal-style-right = Right
parameter-mondrian-effect-wire-removal-removal-style-both = Both sides
parameter-mondrian-effect-wire-removal-removal-style-cross = Cross
## Selective Color 可选颜色
effect-selective-color = Selective Color
parameter-group-selective-color = Selective Color
parameter-mondrian-effect-builtin-selective-color-method-label = Method
parameter-mondrian-effect-builtin-selective-color-colors-label = Colors
parameter-mondrian-effect-builtin-selective-color-trim-cyan-label = Cyan
parameter-mondrian-effect-builtin-selective-color-trim-magenta-label = Magenta
parameter-mondrian-effect-builtin-selective-color-trim-yellow-label = Yellow
parameter-mondrian-effect-builtin-selective-color-trim-black-label = Black
parameter-mondrian-effect-builtin-selective-color-reds-cyan-label = Reds Cyan
parameter-mondrian-effect-builtin-selective-color-reds-magenta-label = Reds Magenta
parameter-mondrian-effect-builtin-selective-color-reds-yellow-label = Reds Yellow
parameter-mondrian-effect-builtin-selective-color-reds-black-label = Reds Black
parameter-mondrian-effect-builtin-selective-color-yellows-cyan-label = Yellows Cyan
parameter-mondrian-effect-builtin-selective-color-yellows-magenta-label = Yellows Magenta
parameter-mondrian-effect-builtin-selective-color-yellows-yellow-label = Yellows Yellow
parameter-mondrian-effect-builtin-selective-color-yellows-black-label = Yellows Black
parameter-mondrian-effect-builtin-selective-color-greens-cyan-label = Greens Cyan
parameter-mondrian-effect-builtin-selective-color-greens-magenta-label = Greens Magenta
parameter-mondrian-effect-builtin-selective-color-greens-yellow-label = Greens Yellow
parameter-mondrian-effect-builtin-selective-color-greens-black-label = Greens Black
parameter-mondrian-effect-builtin-selective-color-cyans-cyan-label = Cyans Cyan
parameter-mondrian-effect-builtin-selective-color-cyans-magenta-label = Cyans Magenta
parameter-mondrian-effect-builtin-selective-color-cyans-yellow-label = Cyans Yellow
parameter-mondrian-effect-builtin-selective-color-cyans-black-label = Cyans Black
parameter-mondrian-effect-builtin-selective-color-blues-cyan-label = Blues Cyan
parameter-mondrian-effect-builtin-selective-color-blues-magenta-label = Blues Magenta
parameter-mondrian-effect-builtin-selective-color-blues-yellow-label = Blues Yellow
parameter-mondrian-effect-builtin-selective-color-blues-black-label = Blues Black
parameter-mondrian-effect-builtin-selective-color-magentas-cyan-label = Magentas Cyan
parameter-mondrian-effect-builtin-selective-color-magentas-magenta-label = Magentas Magenta
parameter-mondrian-effect-builtin-selective-color-magentas-yellow-label = Magentas Yellow
parameter-mondrian-effect-builtin-selective-color-magentas-black-label = Magentas Black
parameter-mondrian-effect-builtin-selective-color-whites-cyan-label = Whites Cyan
parameter-mondrian-effect-builtin-selective-color-whites-magenta-label = Whites Magenta
parameter-mondrian-effect-builtin-selective-color-whites-yellow-label = Whites Yellow
parameter-mondrian-effect-builtin-selective-color-whites-black-label = Whites Black
parameter-mondrian-effect-builtin-selective-color-neutrals-cyan-label = Neutrals Cyan
parameter-mondrian-effect-builtin-selective-color-neutrals-magenta-label = Neutrals Magenta
parameter-mondrian-effect-builtin-selective-color-neutrals-yellow-label = Neutrals Yellow
parameter-mondrian-effect-builtin-selective-color-neutrals-black-label = Neutrals Black
parameter-mondrian-effect-builtin-selective-color-blacks-cyan-label = Blacks Cyan
parameter-mondrian-effect-builtin-selective-color-blacks-magenta-label = Blacks Magenta
parameter-mondrian-effect-builtin-selective-color-blacks-yellow-label = Blacks Yellow
parameter-mondrian-effect-builtin-selective-color-blacks-black-label = Blacks Black
parameter-mondrian-effect-selective-color-method-relative = Relative
parameter-mondrian-effect-selective-color-method-absolute = Absolute
parameter-mondrian-effect-selective-color-colors-reds = Reds
parameter-mondrian-effect-selective-color-colors-yellows = Yellows
parameter-mondrian-effect-selective-color-colors-greens = Greens
parameter-mondrian-effect-selective-color-colors-cyans = Cyans
parameter-mondrian-effect-selective-color-colors-blues = Blues
parameter-mondrian-effect-selective-color-colors-magentas = Magentas
parameter-mondrian-effect-selective-color-colors-whites = Whites
parameter-mondrian-effect-selective-color-colors-neutrals = Neutrals
parameter-mondrian-effect-selective-color-colors-blacks = Blacks

effect-roughen-edges = Roughen Edges
parameter-group-roughen-edges = Roughen Edges
parameter-mondrian-effect-builtin-roughen-edges-edge-type-label = Edge Type
parameter-mondrian-effect-builtin-roughen-edges-edge-color-label = Edge Color
parameter-mondrian-effect-builtin-roughen-edges-border-label = Border
parameter-mondrian-effect-builtin-roughen-edges-sharpness-label = Edge Sharpness
parameter-mondrian-effect-builtin-roughen-edges-influence-label = Fractal Influence
parameter-mondrian-effect-builtin-roughen-edges-scale-label = Scale
parameter-mondrian-effect-builtin-roughen-edges-stretch-label = Stretch Width or Height
parameter-mondrian-effect-builtin-roughen-edges-offset-label = Offset (Turbulence)
parameter-mondrian-effect-builtin-roughen-edges-complexity-label = Complexity
parameter-mondrian-effect-builtin-roughen-edges-evolution-label = Evolution
parameter-mondrian-effect-builtin-roughen-edges-cycle-evolution-label = Cycle Evolution
parameter-mondrian-effect-builtin-roughen-edges-cycle-label = Cycle (Revolutions)
parameter-mondrian-effect-builtin-roughen-edges-random-seed-label = Random Seed
parameter-mondrian-effect-roughen-edges-edge-type-roughen = Roughen
parameter-mondrian-effect-roughen-edges-edge-type-roughen-color = Roughen Color
parameter-mondrian-effect-roughen-edges-edge-type-rusty = Rusty
parameter-mondrian-effect-roughen-edges-edge-type-rusty-color = Rusty Color
parameter-mondrian-effect-roughen-edges-edge-type-photocopy = Photocopy
parameter-mondrian-effect-roughen-edges-edge-type-photocopy-color = Photocopy Color
parameter-mondrian-effect-roughen-edges-edge-type-cutout = Cutout
parameter-mondrian-effect-roughen-edges-edge-type-cutout-color = Cutout Color
effect-eyedropper-fill = Eyedropper Fill
parameter-group-eyedropper-fill = Eyedropper Fill
parameter-mondrian-effect-builtin-eyedropper-fill-sample-x-label = Sample Point X
parameter-mondrian-effect-builtin-eyedropper-fill-sample-y-label = Sample Point Y
parameter-mondrian-effect-builtin-eyedropper-fill-sample-radius-label = Sample Radius
parameter-mondrian-effect-builtin-eyedropper-fill-average-mode-label = Average Pixel Colour
parameter-mondrian-effect-builtin-eyedropper-fill-preserve-alpha-label = Preserve Original Alpha
parameter-mondrian-effect-builtin-eyedropper-fill-blend-with-original-label = Blend with Original
parameter-mondrian-effect-eyedropper-fill-average-mode-mean = Mean
parameter-mondrian-effect-eyedropper-fill-average-mode-median = Median
parameter-mondrian-effect-eyedropper-fill-average-mode-mode = Mode
parameter-mondrian-effect-eyedropper-fill-average-mode-luminance = Luminance

effect-checkerboard = Checkerboard
parameter-group-checkerboard = Checkerboard
parameter-mondrian-effect-builtin-checkerboard-anchor-label = Anchor
parameter-mondrian-effect-builtin-checkerboard-size-from-label = Size From
parameter-mondrian-effect-builtin-checkerboard-corner-label = Corner
parameter-mondrian-effect-builtin-checkerboard-width-label = Width
parameter-mondrian-effect-builtin-checkerboard-height-label = Height
parameter-mondrian-effect-builtin-checkerboard-feather-width-label = Feather Width
parameter-mondrian-effect-builtin-checkerboard-feather-height-label = Feather Height
parameter-mondrian-effect-builtin-checkerboard-color-label = Color
parameter-mondrian-effect-builtin-checkerboard-opacity-label = Opacity
parameter-mondrian-effect-builtin-checkerboard-blend-mode-label = Blending Mode
parameter-mondrian-effect-checkerboard-size-from-corner = Corner
parameter-mondrian-effect-checkerboard-size-from-width-slider = Width Slider
parameter-mondrian-effect-checkerboard-size-from-width-height-sliders = Width & Height Sliders
parameter-mondrian-effect-checkerboard-blend-mode-normal = Normal
parameter-mondrian-effect-checkerboard-blend-mode-darken = Darken
parameter-mondrian-effect-checkerboard-blend-mode-multiply = Multiply
parameter-mondrian-effect-checkerboard-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-checkerboard-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-checkerboard-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-checkerboard-blend-mode-lighten = Lighten
parameter-mondrian-effect-checkerboard-blend-mode-screen = Screen
parameter-mondrian-effect-checkerboard-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-checkerboard-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-checkerboard-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-checkerboard-blend-mode-overlay = Overlay
parameter-mondrian-effect-checkerboard-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-checkerboard-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-checkerboard-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-checkerboard-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-checkerboard-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-checkerboard-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-checkerboard-blend-mode-difference = Difference
parameter-mondrian-effect-checkerboard-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-checkerboard-blend-mode-subtract = Subtract
parameter-mondrian-effect-checkerboard-blend-mode-divide = Divide
parameter-mondrian-effect-checkerboard-blend-mode-hue = Hue
parameter-mondrian-effect-checkerboard-blend-mode-saturation = Saturation
parameter-mondrian-effect-checkerboard-blend-mode-color = Color
parameter-mondrian-effect-checkerboard-blend-mode-luminosity = Luminosity

effect-cell-pattern = Cell Pattern
parameter-group-cell-pattern = Cell Pattern
parameter-mondrian-effect-builtin-cell-pattern-pattern-label = Cell Pattern
parameter-mondrian-effect-builtin-cell-pattern-invert-label = Invert
parameter-mondrian-effect-builtin-cell-pattern-contrast-label = Contrast
parameter-mondrian-effect-builtin-cell-pattern-overflow-label = Overflow
parameter-mondrian-effect-builtin-cell-pattern-disperse-label = Disperse
parameter-mondrian-effect-builtin-cell-pattern-size-label = Size
parameter-mondrian-effect-builtin-cell-pattern-offset-label = Offset
parameter-mondrian-effect-builtin-cell-pattern-tiling-label = Enable Tiling
parameter-mondrian-effect-builtin-cell-pattern-cells-horizontal-label = Cells Horizontal
parameter-mondrian-effect-builtin-cell-pattern-cells-vertical-label = Cells Vertical
parameter-mondrian-effect-builtin-cell-pattern-evolution-label = Evolution
parameter-mondrian-effect-builtin-cell-pattern-cycle-evolution-label = Cycle Evolution
parameter-mondrian-effect-builtin-cell-pattern-cycle-revolutions-label = Cycle (in Revolutions)
parameter-mondrian-effect-builtin-cell-pattern-seed-label = Random Seed
parameter-mondrian-effect-cell-pattern-pattern-bubbles = Bubbles
parameter-mondrian-effect-cell-pattern-pattern-crystals = Crystals
parameter-mondrian-effect-cell-pattern-pattern-plates = Plates
parameter-mondrian-effect-cell-pattern-pattern-static-plates = Static Plates
parameter-mondrian-effect-cell-pattern-pattern-crystallize = Crystallize
parameter-mondrian-effect-cell-pattern-pattern-pillow = Pillow
parameter-mondrian-effect-cell-pattern-pattern-crystals-hq = Crystals HQ
parameter-mondrian-effect-cell-pattern-pattern-plates-hq = Plates HQ
parameter-mondrian-effect-cell-pattern-pattern-static-plates-hq = Static Plates HQ
parameter-mondrian-effect-cell-pattern-pattern-crystallize-hq = Crystallize HQ
parameter-mondrian-effect-cell-pattern-pattern-mixed-crystals = Mixed Crystals
parameter-mondrian-effect-cell-pattern-pattern-tubular = Tubular
parameter-mondrian-effect-cell-pattern-overflow-clip = Clip
parameter-mondrian-effect-cell-pattern-overflow-soft-clamp = Soft Clamp
parameter-mondrian-effect-cell-pattern-overflow-wrap-back = Wrap Back

effect-grid = Grid
parameter-group-grid = Grid
parameter-mondrian-effect-builtin-grid-anchor-label = Anchor
parameter-mondrian-effect-builtin-grid-size-from-label = Size From
parameter-mondrian-effect-builtin-grid-corner-label = Corner
parameter-mondrian-effect-builtin-grid-width-label = Width
parameter-mondrian-effect-builtin-grid-height-label = Height
parameter-mondrian-effect-builtin-grid-border-label = Border
parameter-mondrian-effect-builtin-grid-feather-width-label = Feather Width
parameter-mondrian-effect-builtin-grid-feather-height-label = Feather Height
parameter-mondrian-effect-builtin-grid-invert-label = Invert Grid
parameter-mondrian-effect-builtin-grid-color-label = Color
parameter-mondrian-effect-builtin-grid-opacity-label = Opacity
parameter-mondrian-effect-builtin-grid-blend-mode-label = Blending Mode
parameter-mondrian-effect-grid-size-from-corner = Corner
parameter-mondrian-effect-grid-size-from-width-slider = Width Slider
parameter-mondrian-effect-grid-size-from-width-height-sliders = Width & Height Sliders
parameter-mondrian-effect-grid-blend-mode-normal = Normal
parameter-mondrian-effect-grid-blend-mode-darken = Darken
parameter-mondrian-effect-grid-blend-mode-multiply = Multiply
parameter-mondrian-effect-grid-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-grid-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-grid-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-grid-blend-mode-lighten = Lighten
parameter-mondrian-effect-grid-blend-mode-screen = Screen
parameter-mondrian-effect-grid-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-grid-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-grid-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-grid-blend-mode-overlay = Overlay
parameter-mondrian-effect-grid-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-grid-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-grid-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-grid-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-grid-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-grid-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-grid-blend-mode-difference = Difference
parameter-mondrian-effect-grid-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-grid-blend-mode-subtract = Subtract
parameter-mondrian-effect-grid-blend-mode-divide = Divide
parameter-mondrian-effect-grid-blend-mode-hue = Hue
parameter-mondrian-effect-grid-blend-mode-saturation = Saturation
parameter-mondrian-effect-grid-blend-mode-color = Color
parameter-mondrian-effect-grid-blend-mode-luminosity = Luminosity

effect-circle-ellipse = Circle and Ellipse
parameter-group-circle-ellipse = Circle and Ellipse
parameter-mondrian-effect-builtin-circle-ellipse-center-label = Center
parameter-mondrian-effect-builtin-circle-ellipse-radius-label = Radius
parameter-mondrian-effect-builtin-circle-ellipse-edge-label = Edge
parameter-mondrian-effect-builtin-circle-ellipse-context-slider-label = Context Slider
parameter-mondrian-effect-builtin-circle-ellipse-feather-outer-label = Outer Feather
parameter-mondrian-effect-builtin-circle-ellipse-feather-inner-label = Inner Feather
parameter-mondrian-effect-builtin-circle-ellipse-invert-label = Invert Circle
parameter-mondrian-effect-builtin-circle-ellipse-color-label = Color
parameter-mondrian-effect-builtin-circle-ellipse-opacity-label = Opacity
parameter-mondrian-effect-builtin-circle-ellipse-blend-mode-label = Blending Mode
parameter-mondrian-effect-circle-ellipse-blend-mode-normal = Normal
parameter-mondrian-effect-circle-ellipse-blend-mode-darken = Darken
parameter-mondrian-effect-circle-ellipse-blend-mode-multiply = Multiply
parameter-mondrian-effect-circle-ellipse-blend-mode-color-burn = Color Burn
parameter-mondrian-effect-circle-ellipse-blend-mode-linear-burn = Linear Burn
parameter-mondrian-effect-circle-ellipse-blend-mode-darker-color = Darker Color
parameter-mondrian-effect-circle-ellipse-blend-mode-lighten = Lighten
parameter-mondrian-effect-circle-ellipse-blend-mode-screen = Screen
parameter-mondrian-effect-circle-ellipse-blend-mode-color-dodge = Color Dodge
parameter-mondrian-effect-circle-ellipse-blend-mode-linear-dodge = Linear Dodge (Add)
parameter-mondrian-effect-circle-ellipse-blend-mode-lighter-color = Lighter Color
parameter-mondrian-effect-circle-ellipse-blend-mode-overlay = Overlay
parameter-mondrian-effect-circle-ellipse-blend-mode-soft-light = Soft Light
parameter-mondrian-effect-circle-ellipse-blend-mode-hard-light = Hard Light
parameter-mondrian-effect-circle-ellipse-blend-mode-vivid-light = Vivid Light
parameter-mondrian-effect-circle-ellipse-blend-mode-linear-light = Linear Light
parameter-mondrian-effect-circle-ellipse-blend-mode-pin-light = Pin Light
parameter-mondrian-effect-circle-ellipse-blend-mode-hard-mix = Hard Mix
parameter-mondrian-effect-circle-ellipse-blend-mode-difference = Difference
parameter-mondrian-effect-circle-ellipse-blend-mode-exclusion = Exclusion
parameter-mondrian-effect-circle-ellipse-blend-mode-subtract = Subtract
parameter-mondrian-effect-circle-ellipse-blend-mode-divide = Divide
parameter-mondrian-effect-circle-ellipse-blend-mode-hue = Hue
parameter-mondrian-effect-circle-ellipse-blend-mode-saturation = Saturation
parameter-mondrian-effect-circle-ellipse-blend-mode-color = Color
parameter-mondrian-effect-circle-ellipse-blend-mode-luminosity = Luminosity

effect-stroke = Stroke
parameter-group-stroke = Stroke
parameter-mondrian-effect-builtin-stroke-path-label = Path
parameter-mondrian-effect-builtin-stroke-all-masks-label = All Masks
parameter-mondrian-effect-builtin-stroke-sequential-stroke-label = Sequential Stroke
parameter-mondrian-effect-builtin-stroke-color-label = Color
parameter-mondrian-effect-builtin-stroke-brush-size-label = Brush Size
parameter-mondrian-effect-builtin-stroke-brush-hardness-label = Brush Hardness
parameter-mondrian-effect-builtin-stroke-opacity-label = Opacity
parameter-mondrian-effect-builtin-stroke-start-label = Start
parameter-mondrian-effect-builtin-stroke-end-label = End
parameter-mondrian-effect-builtin-stroke-spacing-label = Spacing
parameter-mondrian-effect-builtin-stroke-paint-style-label = Paint Style
parameter-mondrian-effect-stroke-paint-style-on-original = On Original Image
parameter-mondrian-effect-stroke-paint-style-on-transparent = On Transparent
parameter-mondrian-effect-stroke-paint-style-reveal-original = Reveal Original Image

effect-fill = Fill
parameter-group-fill = Fill
parameter-mondrian-effect-builtin-fill-fill-mask-label = Clip Mask Index (0 = None)
parameter-mondrian-effect-builtin-fill-all-masks-label = Use All Clip Masks
parameter-mondrian-effect-builtin-fill-color-label = Color
parameter-mondrian-effect-builtin-fill-invert-label = Invert
parameter-mondrian-effect-builtin-fill-feather-x-label = Horizontal Feather
parameter-mondrian-effect-builtin-fill-feather-y-label = Vertical Feather
parameter-mondrian-effect-builtin-fill-opacity-label = Opacity

## Bezier Warp 贝塞尔曲线变形
effect-bezier-warp = Bezier Warp
parameter-group-bezier-warp = Bezier Warp
parameter-mondrian-effect-builtin-bezier-warp-vertex-top-left-x-label = Top Left Vertex X
parameter-mondrian-effect-builtin-bezier-warp-vertex-top-left-y-label = Top Left Vertex Y
parameter-mondrian-effect-builtin-bezier-warp-top-handle-left-x-label = Top Left Tangent X
parameter-mondrian-effect-builtin-bezier-warp-top-handle-left-y-label = Top Left Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-top-handle-right-x-label = Top Right Tangent X
parameter-mondrian-effect-builtin-bezier-warp-top-handle-right-y-label = Top Right Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-vertex-top-right-x-label = Top Right Vertex X
parameter-mondrian-effect-builtin-bezier-warp-vertex-top-right-y-label = Top Right Vertex Y
parameter-mondrian-effect-builtin-bezier-warp-right-handle-top-x-label = Right Top Tangent X
parameter-mondrian-effect-builtin-bezier-warp-right-handle-top-y-label = Right Top Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-right-handle-bottom-x-label = Right Bottom Tangent X
parameter-mondrian-effect-builtin-bezier-warp-right-handle-bottom-y-label = Right Bottom Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-vertex-bottom-right-x-label = Bottom Right Vertex X
parameter-mondrian-effect-builtin-bezier-warp-vertex-bottom-right-y-label = Bottom Right Vertex Y
parameter-mondrian-effect-builtin-bezier-warp-bottom-handle-right-x-label = Bottom Right Tangent X
parameter-mondrian-effect-builtin-bezier-warp-bottom-handle-right-y-label = Bottom Right Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-bottom-handle-left-x-label = Bottom Left Tangent X
parameter-mondrian-effect-builtin-bezier-warp-bottom-handle-left-y-label = Bottom Left Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-vertex-bottom-left-x-label = Bottom Left Vertex X
parameter-mondrian-effect-builtin-bezier-warp-vertex-bottom-left-y-label = Bottom Left Vertex Y
parameter-mondrian-effect-builtin-bezier-warp-left-handle-bottom-x-label = Left Bottom Tangent X
parameter-mondrian-effect-builtin-bezier-warp-left-handle-bottom-y-label = Left Bottom Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-left-handle-top-x-label = Left Top Tangent X
parameter-mondrian-effect-builtin-bezier-warp-left-handle-top-y-label = Left Top Tangent Y
parameter-mondrian-effect-builtin-bezier-warp-quality-label = Quality
parameter-mondrian-effect-bezier-warp-quality-1 = 1
parameter-mondrian-effect-bezier-warp-quality-2 = 2
parameter-mondrian-effect-bezier-warp-quality-3 = 3
parameter-mondrian-effect-bezier-warp-quality-4 = 4
parameter-mondrian-effect-bezier-warp-quality-5 = 5
parameter-mondrian-effect-bezier-warp-quality-6 = 6
parameter-mondrian-effect-bezier-warp-quality-7 = 7
parameter-mondrian-effect-bezier-warp-quality-8 = 8
parameter-mondrian-effect-bezier-warp-quality-9 = 9
parameter-mondrian-effect-bezier-warp-quality-10 = 10

## VR Rotate Sphere VR 旋转球面
effect-vr-rotate-sphere = VR Rotate Sphere
parameter-group-vr-rotate-sphere = VR Rotate Sphere
parameter-mondrian-effect-builtin-vr-rotate-sphere-auto-vr-label = Auto VR Properties
parameter-mondrian-effect-builtin-vr-rotate-sphere-frame-layout-label = Frame Layout
parameter-mondrian-effect-builtin-vr-rotate-sphere-horizontal-fov-label = Horizontal FOV
parameter-mondrian-effect-builtin-vr-rotate-sphere-vertical-fov-label = Vertical FOV
parameter-mondrian-effect-builtin-vr-rotate-sphere-tilt-x-label = Tilt (X Axis)
parameter-mondrian-effect-builtin-vr-rotate-sphere-pan-y-label = Pan (Y Axis)
parameter-mondrian-effect-builtin-vr-rotate-sphere-roll-z-label = Roll (Z Axis)
parameter-mondrian-effect-builtin-vr-rotate-sphere-invert-label = Invert Rotation
parameter-mondrian-effect-vr-rotate-sphere-frame-layout-monoscopic = Monoscopic
parameter-mondrian-effect-vr-rotate-sphere-frame-layout-over-under = Over-Under
parameter-mondrian-effect-vr-rotate-sphere-frame-layout-side-by-side = Side-by-Side

## VR Sphere to Plane VR 球面到平面
effect-vr-sphere-to-plane = VR Sphere to Plane
parameter-group-vr-sphere-to-plane = VR Sphere to Plane
parameter-mondrian-effect-builtin-vr-sphere-to-plane-horizontal-fov-label = Horizontal FOV
parameter-mondrian-effect-builtin-vr-sphere-to-plane-projection-tilt-x-label = Projection Tilt (X Axis)
parameter-mondrian-effect-builtin-vr-sphere-to-plane-projection-tilt-y-label = Projection Tilt (Y Axis)
parameter-mondrian-effect-builtin-vr-sphere-to-plane-projection-tilt-z-label = Projection Tilt (Z Axis)
parameter-mondrian-effect-builtin-vr-sphere-to-plane-invert-label = Invert Rotation

## VR Project 2D VR 平面到球面
effect-vr-project-2d = VR Project 2D
parameter-group-vr-project-2d = VR Project 2D
parameter-mondrian-effect-builtin-vr-project-2d-auto-vr-label = Auto VR Properties
parameter-mondrian-effect-builtin-vr-project-2d-frame-layout-label = Frame Layout
parameter-mondrian-effect-builtin-vr-project-2d-horizontal-fov-label = Horizontal FOV
parameter-mondrian-effect-builtin-vr-project-2d-vertical-fov-label = Vertical FOV
parameter-mondrian-effect-builtin-vr-project-2d-scale-label = Scale (Degrees)
parameter-mondrian-effect-builtin-vr-project-2d-stereo-disparity-label = Stereo Disparity (Degrees)
parameter-mondrian-effect-builtin-vr-project-2d-feather-label = Feather
parameter-mondrian-effect-builtin-vr-project-2d-source-tilt-x-label = Source Tilt (X Axis)
parameter-mondrian-effect-builtin-vr-project-2d-source-pan-y-label = Source Pan (Y Axis)
parameter-mondrian-effect-builtin-vr-project-2d-source-roll-z-label = Source Roll (Z Axis)
parameter-mondrian-effect-builtin-vr-project-2d-projection-tilt-x-label = Projection Tilt (X Axis)
parameter-mondrian-effect-builtin-vr-project-2d-projection-pan-y-label = Projection Pan (Y Axis)
parameter-mondrian-effect-builtin-vr-project-2d-projection-roll-z-label = Projection Roll (Z Axis)
parameter-mondrian-effect-vr-project-2d-frame-layout-monoscopic = Monoscopic
parameter-mondrian-effect-vr-project-2d-frame-layout-over-under = Over-Under
parameter-mondrian-effect-vr-project-2d-frame-layout-side-by-side = Side-by-Side

## HDR Tone Mapping HDR 色调映射
effect-hdr-tone-mapping = HDR Tone Mapping
parameter-group-hdr-tone-mapping = HDR Tone Mapping
parameter-mondrian-effect-builtin-hdr-tone-mapping-gain-label = Exposure Gain
parameter-mondrian-effect-builtin-hdr-tone-mapping-gamma-label = Display Gamma
parameter-mondrian-effect-builtin-hdr-tone-mapping-mode-label = Tone Mapping Mode
parameter-mondrian-effect-hdr-tone-mapping-mode-hable = Hable Filmic
parameter-mondrian-effect-hdr-tone-mapping-mode-reinhard = Reinhard

## Fractal 分形
effect-fractal = Fractal
parameter-group-fractal = Fractal
parameter-group-mandelbrot = Mandelbrot
parameter-group-julia = Julia
parameter-group-post-inversion-offset = Post-Inversion Offset
parameter-group-color = Colour
parameter-group-quality = High Quality
parameter-mondrian-effect-builtin-fractal-set-choice-label = Set Choice
parameter-mondrian-effect-builtin-fractal-equation-label = Equation
parameter-mondrian-effect-builtin-fractal-mandelbrot-x-label = Mandelbrot X (Real)
parameter-mondrian-effect-builtin-fractal-mandelbrot-y-label = Mandelbrot Y (Imaginary)
parameter-mondrian-effect-builtin-fractal-mandelbrot-magnification-label = Mandelbrot Magnification
parameter-mondrian-effect-builtin-fractal-mandelbrot-escape-limit-label = Mandelbrot Escape Limit
parameter-mondrian-effect-builtin-fractal-julia-x-label = Julia X (Real)
parameter-mondrian-effect-builtin-fractal-julia-y-label = Julia Y (Imaginary)
parameter-mondrian-effect-builtin-fractal-julia-magnification-label = Julia Magnification
parameter-mondrian-effect-builtin-fractal-julia-escape-limit-label = Julia Escape Limit
parameter-mondrian-effect-builtin-fractal-post-inversion-offset-x-label = Post-Inversion Offset X (Real)
parameter-mondrian-effect-builtin-fractal-post-inversion-offset-y-label = Post-Inversion Offset Y (Imaginary)
parameter-mondrian-effect-builtin-fractal-overlay-label = Overlay
parameter-mondrian-effect-builtin-fractal-transparency-label = Transparency
parameter-mondrian-effect-builtin-fractal-palette-label = Palette
parameter-mondrian-effect-builtin-fractal-hue-label = Hue
parameter-mondrian-effect-builtin-fractal-cycle-steps-label = Cycle Steps
parameter-mondrian-effect-builtin-fractal-cycle-offset-label = Cycle Offset
parameter-mondrian-effect-builtin-fractal-edge-highlight-label = Edge Highlight (Force LQ)
parameter-mondrian-effect-builtin-fractal-oversample-method-label = Oversample Method
parameter-mondrian-effect-builtin-fractal-oversample-factor-label = Oversample Factor
parameter-mondrian-effect-fractal-set-choice-mandelbrot = Mandelbrot
parameter-mondrian-effect-fractal-set-choice-mandelbrot-inverse = Mandelbrot Inverse
parameter-mondrian-effect-fractal-set-choice-mandelbrot-over-julia = Mandelbrot over Julia
parameter-mondrian-effect-fractal-set-choice-mandelbrot-inverse-over-julia = Mandelbrot Inverse over Julia
parameter-mondrian-effect-fractal-set-choice-julia = Julia
parameter-mondrian-effect-fractal-set-choice-julia-inverse = Julia Inverse
parameter-mondrian-effect-fractal-equation-power-2 = z = z² + c
parameter-mondrian-effect-fractal-equation-power-3 = z = z³ + c
parameter-mondrian-effect-fractal-equation-power-5 = z = z⁵ + c
parameter-mondrian-effect-fractal-equation-power-7 = z = z⁷ + c
parameter-mondrian-effect-fractal-palette-lightness-gradient = Lightness Gradient
parameter-mondrian-effect-fractal-palette-hue-wheel = Hue Wheel
parameter-mondrian-effect-fractal-palette-black-white = Black & White
parameter-mondrian-effect-fractal-palette-solid-color = Solid Colour
parameter-mondrian-effect-fractal-palette-escape-angle = Escape Angle
parameter-mondrian-effect-fractal-palette-grayscale = Grayscale
parameter-mondrian-effect-fractal-palette-apple-ii = Apple ][
parameter-mondrian-effect-fractal-oversample-method-edge-detect = Edge Detect (Fast, May Miss Pixels)
parameter-mondrian-effect-fractal-oversample-method-brute-force = Brute Force (Slow, Per-Pixel)

## Remove Color Matting 移除颜色遮罩
effect-remove-color-matting = Remove Color Matting
parameter-group-remove-color-matting = Remove Color Matting
parameter-mondrian-effect-builtin-remove-color-matting-background-color-label = Background Colour
parameter-mondrian-effect-builtin-remove-color-matting-clip-label = Clip

## Photo Filter 照片滤镜
effect-photo-filter = Photo Filter
parameter-group-photo-filter = Photo Filter
parameter-mondrian-effect-builtin-photo-filter-filter-label = Filter
parameter-mondrian-effect-builtin-photo-filter-color-label = Color
parameter-mondrian-effect-builtin-photo-filter-density-label = Density
parameter-mondrian-effect-builtin-photo-filter-preserve-luminosity-label = Preserve Luminosity
parameter-mondrian-effect-photo-filter-filter-warming-85 = Warming Filter (85)
parameter-mondrian-effect-photo-filter-filter-warming-lba = Warming Filter (LBA)
parameter-mondrian-effect-photo-filter-filter-warming-81 = Warming Filter (81)
parameter-mondrian-effect-photo-filter-filter-cooling-80 = Cooling Filter (80)
parameter-mondrian-effect-photo-filter-filter-cooling-lbb = Cooling Filter (LBB)
parameter-mondrian-effect-photo-filter-filter-cooling-82 = Cooling Filter (82)
parameter-mondrian-effect-photo-filter-filter-red = Red
parameter-mondrian-effect-photo-filter-filter-orange = Orange
parameter-mondrian-effect-photo-filter-filter-yellow = Yellow
parameter-mondrian-effect-photo-filter-filter-green = Green
parameter-mondrian-effect-photo-filter-filter-cyan = Cyan
parameter-mondrian-effect-photo-filter-filter-blue = Blue
parameter-mondrian-effect-photo-filter-filter-violet = Violet
parameter-mondrian-effect-photo-filter-filter-magenta = Magenta
parameter-mondrian-effect-photo-filter-filter-sepia = Sepia
parameter-mondrian-effect-photo-filter-filter-deep-red = Deep Red
parameter-mondrian-effect-photo-filter-filter-deep-blue = Deep Blue
parameter-mondrian-effect-photo-filter-filter-deep-emerald = Deep Emerald
parameter-mondrian-effect-photo-filter-filter-deep-yellow = Deep Yellow
parameter-mondrian-effect-photo-filter-filter-underwater = Underwater
parameter-mondrian-effect-photo-filter-filter-custom = Custom

## JPEG Damage JPEG 损坏
effect-jpeg-damage = JPEG Damage
parameter-group-jpeg-damage = JPEG Damage
parameter-mondrian-effect-builtin-jpeg-damage-quality-label = Quality
parameter-mondrian-effect-builtin-jpeg-damage-resolution-label = Resolution
parameter-mondrian-effect-builtin-jpeg-damage-macroblock-aspect-label = Macroblock Aspect
parameter-mondrian-effect-builtin-jpeg-damage-frequency-scale-label = Frequency Scale
parameter-mondrian-effect-builtin-jpeg-damage-scaling-component-label = Scaling Component
parameter-mondrian-effect-jpeg-damage-scaling-component-luma = Luma
parameter-mondrian-effect-jpeg-damage-scaling-component-chroma = Chroma
parameter-mondrian-effect-jpeg-damage-scaling-component-both = Both

## Colorama 色光
effect-colorama = Colorama
parameter-group-colorama-input-phase = Input Phase
parameter-group-colorama-output-cycle = Output Cycle
parameter-group-colorama-modify = Modify
parameter-group-colorama-pixel-selection = Pixel Selection
parameter-group-colorama-masking = Masking
parameter-mondrian-effect-builtin-colorama-get-phase-from-label = Get Phase From
parameter-mondrian-effect-builtin-colorama-add-phase-label = Add Phase
parameter-mondrian-effect-builtin-colorama-add-phase-from-label = Add Phase From
parameter-mondrian-effect-builtin-colorama-add-mode-label = Add Mode
parameter-mondrian-effect-builtin-colorama-phase-shift-label = Phase Shift
parameter-mondrian-effect-builtin-colorama-preset-palette-label = Use Preset Palette
parameter-mondrian-effect-builtin-colorama-cycle-repetitions-label = Cycle Repetitions
parameter-mondrian-effect-builtin-colorama-interpolate-label = Interpolate Palette
parameter-mondrian-effect-builtin-colorama-modify-label = Modify
parameter-mondrian-effect-builtin-colorama-modify-alpha-label = Modify Alpha
parameter-mondrian-effect-builtin-colorama-change-empty-pixels-label = Change Empty Pixels
parameter-mondrian-effect-builtin-colorama-matching-color-label = Matching Color
parameter-mondrian-effect-builtin-colorama-matching-tolerance-label = Matching Tolerance
parameter-mondrian-effect-builtin-colorama-matching-softness-label = Matching Softness
parameter-mondrian-effect-builtin-colorama-matching-mode-label = Matching Mode
parameter-mondrian-effect-builtin-colorama-mask-layer-label = Mask Layer
parameter-mondrian-effect-builtin-colorama-mask-mode-label = Mask Mode
parameter-mondrian-effect-builtin-colorama-composite-on-layer-label = Composite On Layer
parameter-mondrian-effect-builtin-colorama-blend-with-original-label = Blend With Original
parameter-mondrian-effect-colorama-get-phase-from-intensity = Intensity
parameter-mondrian-effect-colorama-get-phase-from-red = Red
parameter-mondrian-effect-colorama-get-phase-from-green = Green
parameter-mondrian-effect-colorama-get-phase-from-blue = Blue
parameter-mondrian-effect-colorama-get-phase-from-hue = Hue
parameter-mondrian-effect-colorama-get-phase-from-lightness = Lightness
parameter-mondrian-effect-colorama-get-phase-from-saturation = Saturation
parameter-mondrian-effect-colorama-get-phase-from-value = Value
parameter-mondrian-effect-colorama-get-phase-from-alpha = Alpha
parameter-mondrian-effect-colorama-get-phase-from-zero = Zero
parameter-mondrian-effect-colorama-add-phase-from-intensity = Intensity
parameter-mondrian-effect-colorama-add-phase-from-red = Red
parameter-mondrian-effect-colorama-add-phase-from-green = Green
parameter-mondrian-effect-colorama-add-phase-from-blue = Blue
parameter-mondrian-effect-colorama-add-phase-from-hue = Hue
parameter-mondrian-effect-colorama-add-phase-from-lightness = Lightness
parameter-mondrian-effect-colorama-add-phase-from-saturation = Saturation
parameter-mondrian-effect-colorama-add-phase-from-value = Value
parameter-mondrian-effect-colorama-add-phase-from-alpha = Alpha
parameter-mondrian-effect-colorama-add-phase-from-zero = Zero
parameter-mondrian-effect-colorama-add-mode-wrap = Wrap
parameter-mondrian-effect-colorama-add-mode-clamp = Clamp
parameter-mondrian-effect-colorama-add-mode-average = Average
parameter-mondrian-effect-colorama-add-mode-screen = Screen
parameter-mondrian-effect-colorama-interpolate-on = On
parameter-mondrian-effect-colorama-interpolate-off = Off
parameter-mondrian-effect-colorama-matching-mode-off = Off
parameter-mondrian-effect-colorama-matching-mode-rgb = RGB
parameter-mondrian-effect-colorama-matching-mode-hue = Hue
parameter-mondrian-effect-colorama-matching-mode-chroma = Chroma
parameter-mondrian-effect-colorama-mask-mode-off = Off
parameter-mondrian-effect-colorama-mask-mode-intensity = Intensity
parameter-mondrian-effect-colorama-mask-mode-alpha = Alpha
parameter-mondrian-effect-colorama-mask-mode-inverted-intensity = Inverted Intensity
parameter-mondrian-effect-colorama-mask-mode-inverted-alpha = Inverted Alpha
parameter-mondrian-effect-colorama-modify-all = All
parameter-mondrian-effect-colorama-modify-red = Red
parameter-mondrian-effect-colorama-modify-green = Green
parameter-mondrian-effect-colorama-modify-blue = Blue
parameter-mondrian-effect-colorama-modify-rg = Red Green
parameter-mondrian-effect-colorama-modify-gb = Green Blue
parameter-mondrian-effect-colorama-modify-rb = Red Blue
parameter-mondrian-effect-colorama-modify-none = None
parameter-mondrian-effect-colorama-modify-hue = Hue
parameter-mondrian-effect-colorama-modify-lightness = Lightness
parameter-mondrian-effect-colorama-modify-saturation = Saturation
parameter-mondrian-effect-colorama-modify-hl = Hue Lightness
parameter-mondrian-effect-colorama-modify-ls = Lightness Saturation
parameter-mondrian-effect-colorama-modify-hs = Hue Saturation
parameter-mondrian-effect-colorama-preset-palette-hue-cycle = Hue Cycle
parameter-mondrian-effect-colorama-preset-palette-alpha-ramp = Alpha Ramp
parameter-mondrian-effect-colorama-preset-palette-negative = Negative
parameter-mondrian-effect-colorama-preset-palette-rgb = RGB
parameter-mondrian-effect-colorama-preset-palette-ramp-red = Ramp Red
parameter-mondrian-effect-colorama-preset-palette-ramp-green = Ramp Green
parameter-mondrian-effect-colorama-preset-palette-ramp-blue = Ramp Blue
parameter-mondrian-effect-colorama-preset-palette-ramp-grey = Ramp Grey
parameter-mondrian-effect-colorama-preset-palette-saturation-ramp = Saturation Ramp
parameter-mondrian-effect-colorama-preset-palette-solarize-red = Solarize Red
parameter-mondrian-effect-colorama-preset-palette-solarize-green = Solarize Green
parameter-mondrian-effect-colorama-preset-palette-solarize-blue = Solarize Blue
parameter-mondrian-effect-colorama-preset-palette-solarize-grey = Solarize Grey
parameter-mondrian-effect-colorama-preset-palette-caribbean = Carribean
parameter-mondrian-effect-colorama-preset-palette-clay = Clay
parameter-mondrian-effect-colorama-preset-palette-copper = Copper
parameter-mondrian-effect-colorama-preset-palette-deep-ocean = Deep Ocean
parameter-mondrian-effect-colorama-preset-palette-earthenware = Earthenware
parameter-mondrian-effect-colorama-preset-palette-fire = Fire
parameter-mondrian-effect-colorama-preset-palette-fire-and-smoke = Fire And Smoke
parameter-mondrian-effect-colorama-preset-palette-golden-1 = Golden 1
parameter-mondrian-effect-colorama-preset-palette-golden-2 = Golden 2
parameter-mondrian-effect-colorama-preset-palette-granite = Granite
parameter-mondrian-effect-colorama-preset-palette-horizon = Horizon
parameter-mondrian-effect-colorama-preset-palette-leather = Leather
parameter-mondrian-effect-colorama-preset-palette-moldy = Moldy
parameter-mondrian-effect-colorama-preset-palette-mossy = Mossy
parameter-mondrian-effect-colorama-preset-palette-old-glory = Old Glory
parameter-mondrian-effect-colorama-preset-palette-rusty = Rusty
parameter-mondrian-effect-colorama-preset-palette-sepia-1 = Sepia 1
parameter-mondrian-effect-colorama-preset-palette-sepia-2 = Sepia 2
parameter-mondrian-effect-colorama-preset-palette-skink = Skink
parameter-mondrian-effect-colorama-preset-palette-sunset = Sunset
parameter-mondrian-effect-colorama-preset-palette-ramp-cyan = Ramp Cyan
parameter-mondrian-effect-colorama-preset-palette-none = None

## Smart Blur 智能模糊
effect-smart-blur = Smart Blur
parameter-group-smart-blur = Smart Blur
parameter-mondrian-effect-builtin-smart-blur-radius-label = Radius
parameter-mondrian-effect-builtin-smart-blur-threshold-label = Threshold
parameter-mondrian-effect-builtin-smart-blur-mode-label = Mode
parameter-mondrian-effect-smart-blur-mode-normal = Normal
parameter-mondrian-effect-smart-blur-mode-edge-only = Edge Only
parameter-mondrian-effect-smart-blur-mode-overlay-edge = Overlay Edge

## Chromatic Aberration Removal
effect-chromatic-aberration-removal = Chromatic Aberration Removal
parameter-group-chromatic-aberration-removal = Chromatic Aberration Removal
parameter-mondrian-effect-builtin-chromatic-aberration-removal-lateral-label = Lateral Chromatic Aberration
parameter-mondrian-effect-builtin-chromatic-aberration-removal-longitudinal-label = Longitudinal Chromatic Aberration
parameter-mondrian-effect-builtin-chromatic-aberration-removal-edge-scale-label = Edge Scale
parameter-mondrian-effect-builtin-chromatic-aberration-removal-centre-label = Centre

## Basic 3D
effect-basic-3d = Basic 3D
parameter-group-basic-3d = Basic 3D
parameter-mondrian-effect-builtin-basic-3d-swivel-label = Swivel
parameter-mondrian-effect-builtin-basic-3d-tilt-label = Tilt
parameter-mondrian-effect-builtin-basic-3d-distance-label = Distance to Image
parameter-mondrian-effect-builtin-basic-3d-specular-label = Specular Highlight

## Camera View
effect-camera-view = Camera View
parameter-group-camera-view = Camera View
parameter-mondrian-effect-builtin-camera-view-longitude-label = Longitude
parameter-mondrian-effect-builtin-camera-view-latitude-label = Latitude
parameter-mondrian-effect-builtin-camera-view-roll-label = Roll
parameter-mondrian-effect-builtin-camera-view-focal-length-label = Focal Length
parameter-mondrian-effect-builtin-camera-view-distance-label = Distance
parameter-mondrian-effect-builtin-camera-view-zoom-label = Zoom
parameter-mondrian-effect-builtin-camera-view-fill-color-label = Fill Color
parameter-mondrian-effect-builtin-camera-view-fill-alpha-label = Fill Alpha
