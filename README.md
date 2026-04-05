[![crates.io](https://img.shields.io/crates/v/bytearray-ringbuffer.svg)](https://crates.io/crates/bytearray-ringbuffer)
[![crates.io](https://img.shields.io/crates/d/bytearray-ringbuffer.svg)](https://crates.io/crates/bytearray-ringbuffer)
# Bytearray-Ringbuffer

An embedded-friendly `VecDeque<Vec<u8>>`.

`BytearrayRingbuffer` stores byte slices in a fixed-size array, similar to `heapless::Vec`.
However, the elements in `BytearrayRingbuffer` are not required to have a fixed size.
Instead, information about each slice (ie. length) is stored alongside the payload.
Each slice `data` uses up `data.len() + 8` bytes in the buffer.

This is useful for efficiently storing elements of very different lengths, as short elements do not have to be padded.

One downside is that elements may wrap around at the end of the buffer.
For `pop_front`, `iter`, `iter_backwards`, `nth`, and `nth_reverse`, each element is returned as a [`Packet`] with fields `a` and `b`.
If the element does not wrap around, `a` will contain all data and `b` will be empty.
Otherwise, `a` and `b` have to be concatenated to yield the full result.
[`Packet::len`] returns the total payload length (`a.len() + b.len()`), [`Packet::copy_into`] copies the full payload into a flat `&mut [u8]`, [`Packet::copy_part_into`] copies a sub-range, and [`Packet::extend_into`] appends the payload into any [`Extend<u8>`](core::iter::Extend) collection such as `Vec<u8>`.
`nth_contiguous` is the exception: it returns a single `&[u8]` and may rotate the backing array so that slice is contiguous. It returns `None` if the queue is empty or if `n >= count()`.

## MSRV

Rust **1.85** or later (`edition = "2024"`).

## Usage
```rust
use bytearray_ringbuffer::BytearrayRingbuffer;
// Create a buffer with a capacity of 64 bytes.
let mut buffer = BytearrayRingbuffer::<64>::new();
// Store some packets.
buffer.push(b"hello world").unwrap();
buffer.push(b"").unwrap();
buffer.push(b"testing").unwrap();
// Number of packets currently stored.
assert_eq!(buffer.count(), 3);

// Pop the oldest packet and copy it into a flat Vec<u8>.
let p = buffer.pop_front().unwrap();
assert_eq!(p.len(), 11); // "hello world"
let mut out = vec![0u8; p.len()];
p.copy_into(&mut out);
assert_eq!(out, b"hello world");

// Iterate oldest-first without removing entries; extend into a Vec<u8>.
for p in buffer.iter() {
    let mut collected = Vec::new();
    p.extend_into(&mut collected); // works with any Extend<u8>
    // `collected` now contains the full packet payload
}
```

### Multipart push

When the full payload is not available up-front, use `push_multipart` (or `push_multipart_force`) to build a packet incrementally:

```rust
use bytearray_ringbuffer::BytearrayRingbuffer;
let mut buffer = BytearrayRingbuffer::<64>::new();
// begin an in-progress packet
let mut mp = buffer.push_multipart().unwrap();
mp.push(b"hello").unwrap();
mp.push(b" world").unwrap();
drop(mp); // commits the packet "hello world"
assert_eq!(buffer.count(), 1);

// or discard the in-progress write:
let mut mp = buffer.push_multipart().unwrap();
mp.push(b"discarded").unwrap();
mp.cancel(); // head is rewound; count is unchanged
assert_eq!(buffer.count(), 1);
```

## Performance
The tradeoff here is that random access by index (`nth`, `nth_reverse`, `nth_contiguous`) walks packets from an end of the queue, so it is linear in the number of packets skipped.

|operation|time complexity|
|---------|---------------|
|`push()` | O(1)
|`push_force()` | O(1) amortized per push; worst case O(m) if m oldest packets are dropped to make room
|`push_multipart()` | O(1) to create the guard; each `push` call is O(1); `drop`/`cancel` is O(1)
|`push_multipart_force()` | O(1) to create the guard; each `push` call is O(1) amortized; `drop`/`cancel` is O(1)
|`pop_front()` | O(1)
|`iter()` / `iter_backwards()` | O(1) to create the iterator; O(1) per `next()`; O(k) to visit all k packets
|`count()` | O(1)
|`nth(n)` / `nth_reverse(n)` | O(n) in the index n (packets walked)
|`nth_contiguous(n)` | O(n) to locate the packet, plus O(N) if the buffer is rotated

## Changelog

See [`CHANGELOG.md`](https://github.com/martin2250/bytearray-ringbuffer/blob/master/CHANGELOG.md) in the repository.
