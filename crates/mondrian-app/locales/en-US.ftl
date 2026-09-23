app-name = Mondrian
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
panel-viewer = Viewer
panel-scopes = Scopes
panel-timeline = Timeline
panel-inspector = Inspector
panel-mixer = Mixer
panel-effects = Effects
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
