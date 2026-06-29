# Assets Panel

The Assets panel is a project asset browser, not a whole-computer file browser.

## Responsibilities

- show project library assets and folders/bins
- import media
- create generated assets such as adjustment layer and solid color
- rename, delete, move, relink
- expose offline/missing state
- start drag payloads into timeline

## Asset Cards

Cards should show kind, thumbnail/placeholder, name, duration/metadata, and offline status. Generated assets should look intentional, not like missing files.

## Context Menus

Right-click menus use shared menu primitives. Nested submenus need hover state and keyboard navigation matching top-level menus.

## Selection

Asset selection is panel-local unless it triggers timeline/app selection. Dragging an asset must preserve asset identity and kind.
