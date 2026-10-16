# Changelog

## 0.11.0 - 2026-10-15

Sorry.

### Changed

- Rewrite the GUI in Rust using egui and eframe, replacing the HTML/JS front
  end and the unmaintained web-view dependency
- Replace the web-view, wfd, tinyfiledialogs and dirs-sys dependencies with eframe and rfd
- Migrate from winapi to windows-sys
- Update all remaining dependencies and move to the 2024 edition
- Use the standard library's file locking instead of the fs2 crate
- Replace the unmaintained filesize crate with a direct `GetCompressedFileSizeW` call
- Replace the backtrace crate with `std::backtrace`
- Replace lazy_static with `std::sync::LazyLock`
- Replace tempdir with tempfile in tests

### Fixed

- Report `DeviceIoControl` failures instead of treating every result as a success, so incompressible and unbacked files are now handled correctly

### Removed

- Unused glob dependency

## [0.10.1] - 2020-12-22

### Fixed

- Avoid high CPU usage in GUI loop ([#42])

## [0.10.0] - 2020-12-19

### Changed

- Update dependencies
- Minor UI tweaks due to changes in DPI handling
- Migrate to new dialog crates
- More small internal improvements by @Dr-Emann, thanks! ([#30], [#32])

### Fixed

- Exclusively lock files prior to compaction (should fix [#40], thanks @A-H-M)

## [0.9.0] - 2020-03-03

### Added

- Preserve file timestamps following compression/decompression ([#16])

## [0.8.0] - 2020-02-29

### Added

- Excluded directories now get skipped entirely ([#8])

### Changed

- Paused jobs no longer poll ([#10], @Dr-Emann)
- Less refcounting ([#9], @Dr-Emann)

### Fixed

- Tests ([#11], @Dr-Emann)

### Removed

- WofUtil.dll version check ([#6])

## [0.7.1] - 2019-07-17

### Added

- Initial release

[0.7.1]: https://github.com/Freaky/Compactor/releases/tag/v0.7.1
[0.8.0]: https://github.com/Freaky/Compactor/releases/tag/v0.8.0
[0.9.0]: https://github.com/Freaky/Compactor/releases/tag/v0.9.0
[0.10.0]: https://github.com/Freaky/Compactor/releases/tag/v0.10.0
[0.10.1]: https://github.com/Freaky/Compactor/releases/tag/v0.10.1
[#6]: https://github.com/Freaky/Compactor/issues/6
[#8]: https://github.com/Freaky/Compactor/issues/8
[#9]: https://github.com/Freaky/Compactor/pull/9
[#10]: https://github.com/Freaky/Compactor/pull/10
[#11]: https://github.com/Freaky/Compactor/pull/11
[#16]: https://github.com/Freaky/Compactor/issues/16
[#30]: https://github.com/Freaky/Compactor/pull/30
[#32]: https://github.com/Freaky/Compactor/pull/32
[#40]: https://github.com/Freaky/Compactor/issues/40
[#42]: https://github.com/Freaky/Compactor/issues/42
