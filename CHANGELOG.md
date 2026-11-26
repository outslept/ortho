# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/outslept/ortho/releases/tag/v0.1.0) - 2026-11-26

### Added

- *(tile)* add vertical third cycles, 2-step maximize/center, and grid-aware monitor hops
- *(tray)* add dark-mode system tray menu, focus hook, and startup toggle
- *(core)* add JSON config with lazy mtime hot-reload and global hotkeys
- *(win)* add stack-allocated restore ring buffer for pre-tiled windows
- *(win)* add window filtering, DWM shadow compensation, and instant unmaximize
- *(tile)* add pure integer tiling geometry and 1/2-2/3-1/3 cycle

### Changed

- *(main)* use checked hotkey ID conversion and document Win32 safety
- *(tray)* document FFI safety invariants, uxtheme ordinals, and rustdoc
- *(win)* model restore history without sentinels and document Win32 safety
- *(hotkey)* avoid heap allocation in token parsing and document unsafe blocks
- *(tile)* replace boolean side flags with enums and add rustdoc
- *(config)* convert comments to rustdoc and document gap bounds

### Documentation

- *(readme)* expand usage, hotkeys, config, and cli documentation

### Fixed

- *(log,config,tray,win)* add file logger and harden win32 edge cases
- *(win,tile,tray)* harden win32 window placement, restore history, and tray ipc

### Internal

- add release workflow
- add release-plz configuration file
- update release binary size
- add ci workflow
- update settings
- remove unfulfilled lint expectation
- update project icon
- add dependabot configuration
- update root readme
- initial commit

### Other

- *(config)* fix clock-tick stamp collision in reload recovery test
- resolve clippy::same_item_push lint in icon generator
- add license
- *(release)* embed bento tray icon resource and configure windows subsystem
