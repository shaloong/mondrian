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
new-project-custom-ocio = Custom OpenColorIO
color-select-custom-ocio = Choose custom OpenColorIO…
new-project-create-proxies = Create proxies
new-project-preview-cache = Preview cache
new-project-cancel = Cancel
new-project-create = Create...
color-choose-ocio-config = Choose OpenColorIO configuration
color-ocio-config-filter = OpenColorIO configuration
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
