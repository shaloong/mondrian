app-name = Mondrian
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
notification-action-failed = Action failed: { $reason }
