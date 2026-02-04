# Bytearray-Ringbuffer

An embedded-friendly `VecDeque<Vec<u8>>`.

`BytearrayRingbuffer` stores byte slices in a fixed-size array, similar to `heapless::Vec`.
However, the elements in `BytearrayRingbuffer` are not required to have a fixed size.
Instead, information about each slice (ie. length) is stored alongside the payload.
Each slice `data` uses up `data.len() + 8` bytes in the buffer.

This is useful for efficiently storing elements of very different lengths, as short elements do not have to be padded.

One downside is that elements may always wrap around at the end of the buffer.
When reading, therefore, always two slices `(a, b)` are returned.
If the element does not wrap around, `a` will contain all data and `b` will be empty.
Otherwise, `a` and `b` have to be concatenated to yield the full result.

## Usage
```rust
use bytearray_ringbuffer::BytearrayRingbuffer;
// Create a buffer with a capacity of 64 bytes.
let mut buffer = BytearrayRingbuffer::<64>::new();
// store some packets
buffer.push(b"hello world").unwrap();
buffer.push(b"").unwrap();
buffer.push(b"testing").unwrap();
// number of packets
assert_eq!(buffer.count(), 3);
// retrieve 
for (a, b) in buffer.iter() {
    // a and b are &[u8]
    let mut output = Vec::new();
    output.extend_from_slice(a);
    output.extend_from_slice(b);
    // now output contains the original elements
}
```

## Performance
The tradeoff here is that elements can't be accessed without iterating through the buffer.

|operation|time complexity|
|---------|---------------|
|`push()` | O(1)
|`pop()` | O(1)
|`iter()` | O(1)
|`count()` | O(N)
|`nth(n)` | O(N)
