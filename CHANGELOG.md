# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-04-06

### Changed

- `Packet::copy_part_into` now accepts any `impl RangeBounds<usize>` instead of the concrete `Range<usize>`, enabling callers to use `0..4`, `2..=5`, `3..`, `..3`, `..=2`, and `..` directly.

## [1.0.0] - 2026-04-05

### Added

- `Packet<'a>` struct — a named, borrowed view of a single packet from the ring buffer. Contains two public fields `a: &[u8]` and `b: &[u8]`; `b` is empty when the payload is contiguous.
- `Packet::len()` — returns the total payload length (`a.len() + b.len()`).
- `Packet::is_empty()` — returns `true` when the payload is empty.
- `Packet::copy_into(&self, buffer: &mut [u8])` — copies the full payload into a flat `&mut [u8]`.
- `Packet::copy_part_into(&self, range: Range<usize>, buffer: &mut [u8])` — copies a sub-range of the payload into a flat `&mut [u8]`.
- `Packet::extend_into<E: Extend<u8>>(&self, target: &mut E)` — appends the full payload into any `Extend<u8>` collection (e.g. `Vec<u8>`, `heapless::Vec`).

### Changed

- **Breaking:** `pop_front`, `nth`, `nth_reverse`, `iter` (`Iter`), and `iter_backwards` (`IterBackwards`) now return `Packet<'_>` instead of the `(&[u8], &[u8])` tuple. Callers that destructured `(a, b)` should change to `p.a` / `p.b`.

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
