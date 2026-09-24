app-name = Mondrian
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
