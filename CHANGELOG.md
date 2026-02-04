# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
