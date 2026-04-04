# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-04-04

### Added

- `push_multipart()` and `push_multipart_force()` methods that return a `MultipartPush<'_, N>` guard, allowing a packet to be built incrementally across multiple `push` calls. The packet is committed when the guard is dropped. Call `cancel(self)` to discard the in-progress write without committing a packet (the head is rewound; in force mode any packets already displaced are permanently lost).

## [0.3.1] - 2026-04-02

### Changed

- `nth_contiguous(n)` now returns `None` when `n >= count()` (previously it could read past the queue and return an empty slice).

### Added

- **MSRV** declared as Rust 1.85 (`rust-version` in `Cargo.toml`; matches `edition = "2024"`).

### Fixed

- Documentation and internal comment cleanups (`push_force`, `Iter::next`).

## [0.3.0] - 2026-02-04

### Added

- `nth_reverse(n: usize)` function

### Changed

- `BytearrayRingbuffer` now keeps count of the number of entries stored, making `count()` much faster

## [0.2.0] - 2026-02-04

### Added

- Documentation
- `nth_contiguous(n: usize)` function to read an element as contiguous slice without allocating another buffer.

### Changed

- `nth()` now interprets the index from oldest to newest

## [0.1.0] - 2026-02-04

### Added

- Initial release
