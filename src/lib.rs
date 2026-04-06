#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

/// Fixed-capacity FIFO of variable-length byte slices, backed by `[u8; N]` with no heap allocation.
///
/// Each stored packet uses `data.len() + 8` bytes: a leading `u32` length (native endian), the
/// payload, then the same length again. The queue is a ring: `head` is where the next `push` writes;
/// `tail` is the oldest packet. Payloads may wrap across the end of the array; most accessors return
/// a [`Packet`] whose slices `a` and `b` concatenate to the full payload.
///
/// The backing array is only modified by this crate's own logic (the field is private). Methods
/// maintain consistent framing; [`Self::pop_front`] and iterators rely on that.
///
/// Compile-time requirements: `N > 8` and `N < u32::MAX` (see [`Self::new`]).
pub struct BytearrayRingbuffer<const N: usize> {
    buffer: [u8; N],
    /// Byte index in `buffer` where the next [`Self::push`] will begin writing.
    head: usize,
    /// Byte index in `buffer` where the oldest packet begins.
    tail: usize,
    /// Number of packets currently stored.
    count: usize,
}

/// A borrowed view of a single packet from the ring buffer.
///
/// Because a packet's payload may wrap across the end of the backing array, it is represented as
/// two contiguous slices. `a` is the first (or only) part of the payload; `b` is the second part
/// and is empty when the payload is contiguous.
///
/// Use [`Self::copy_into`] or [`Self::copy_part_into`] to copy the payload into a flat buffer.
#[derive(Debug, PartialEq)]
pub struct Packet<'a> {
    /// First (or only) slice of the packet payload.
    pub a: &'a [u8],
    /// Second slice of the packet payload; empty when the payload is contiguous.
    pub b: &'a [u8],
}

impl<'a> Packet<'a> {
    /// Returns the total length of the packet payload (`a.len() + b.len()`).
    pub fn len(&self) -> usize {
        self.a.len() + self.b.len()
    }

    /// Returns `true` if the packet payload is empty.
    pub fn is_empty(&self) -> bool {
        self.a.is_empty() && self.b.is_empty()
    }

    /// Copies the full packet payload into `buffer`.
    ///
    /// # Panics
    ///
    /// Panics if `buffer.len() != self.a.len() + self.b.len()`.
    pub fn copy_into(&self, buffer: &mut [u8]) {
        assert_eq!(
            buffer.len(),
            self.len(),
            "buffer length must equal packet length"
        );
        buffer[..self.a.len()].copy_from_slice(self.a);
        buffer[self.a.len()..].copy_from_slice(self.b);
    }

    /// Copies the bytes in `range` of the packet payload into `buffer`.
    ///
    /// The range is interpreted over the logical concatenation `[a | b]`.
    /// Any type implementing [`RangeBounds<usize>`](core::ops::RangeBounds) is accepted,
    /// including `0..4`, `2..=5`, `1..`, `..3`, and `..`.
    ///
    /// # Panics
    ///
    /// Panics if `buffer.len()` does not equal the length implied by `range`, or if the resolved
    /// range end exceeds `self.a.len() + self.b.len()`.
    pub fn copy_part_into(&self, range: impl core::ops::RangeBounds<usize>, buffer: &mut [u8]) {
        use core::ops::Bound;

        let total = self.len();

        let start = match range.start_bound() {
            Bound::Included(&s) => s,
            Bound::Excluded(&s) => s + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&e) => e + 1,
            Bound::Excluded(&e) => e,
            Bound::Unbounded => total,
        };

        assert!(start <= end, "range start must not be greater than end");
        assert_eq!(
            buffer.len(),
            end - start,
            "buffer length must equal range length"
        );
        assert!(end <= total, "range out of bounds");
        let a_len = self.a.len();
        let mut buf_pos = 0;

        // Copy the portion of `a` that falls within [start, end).
        if start < a_len {
            let a_end = end.min(a_len);
            let chunk = &self.a[start..a_end];
            buffer[buf_pos..buf_pos + chunk.len()].copy_from_slice(chunk);
            buf_pos += chunk.len();
        }

        // Copy the portion of `b` that falls within [start, end).
        if end > a_len {
            let b_start = start.saturating_sub(a_len);
            let b_end = end.saturating_sub(a_len);
            let chunk = &self.b[b_start..b_end];
            buffer[buf_pos..buf_pos + chunk.len()].copy_from_slice(chunk);
        }
    }

    /// Extends `target` with the full packet payload.
    ///
    /// Appends the bytes from `a` followed by `b` to `target`. Works with any collection
    /// that implements [`Extend<u8>`](core::iter::Extend), such as `Vec<u8>` or
    /// `heapless::Vec<u8, N>`.
    pub fn extend_into<E: Extend<u8>>(&self, target: &mut E) {
        target.extend(self.a.iter().copied());
        target.extend(self.b.iter().copied());
    }
}

/// Returned when a [`BytearrayRingbuffer::push`] cannot store `data` without dropping older packets.
///
/// For [`BytearrayRingbuffer::push`], this means the unused region is too small. For
/// [`BytearrayRingbuffer::push_force`], this is only returned when `data.len() > N - 8` (a single
/// packet cannot fit in the buffer at all).
#[derive(Copy, Clone, Debug)]
pub struct NotEnoughSpaceError;

/// Guard returned by [`BytearrayRingbuffer::push_multipart`] and
/// [`BytearrayRingbuffer::push_multipart_force`].
///
/// Accumulates payload bytes written via repeated [`Self::push`] calls. When dropped, the
/// completed packet (header + payload + footer) is committed to the ring buffer. Call
/// [`Self::cancel`] to discard the in-progress write without committing a packet.
///
/// In force mode any existing packets that were displaced to make room are permanently lost, even
/// if the write is cancelled.
pub struct MultipartPush<'a, const N: usize> {
    buf: &'a mut BytearrayRingbuffer<N>,
    /// Ring index where the 4-byte header will be written on finalise.
    start: usize,
    /// Payload bytes written so far.
    len: usize,
    /// Whether to drop old packets when space is tight.
    force: bool,
    /// Set by [`Self::cancel`]; prevents [`Drop`] from committing the packet.
    cancelled: bool,
}

impl<'a, const N: usize> MultipartPush<'a, N> {
    /// Appends `data` to the packet currently being written.
    ///
    /// May be called multiple times. The chunks are concatenated in order.
    ///
    /// In normal mode returns [`NotEnoughSpaceError`] when there is not enough space in the buffer
    /// to fit `data` plus the 4-byte footer. In force mode it drops the oldest packets until there
    /// is room.
    ///
    /// # Errors
    ///
    /// Returns [`NotEnoughSpaceError`] if:
    /// - The total accumulated payload would exceed `N - 8` (the maximum for any single packet).
    /// - In normal mode: there is not enough unused space.
    /// - In force mode: even after dropping all existing packets there is still not enough space
    ///   (meaning the total accumulated payload exceeds what can fit in the buffer).
    pub fn push(&mut self, data: &[u8]) -> Result<(), NotEnoughSpaceError> {
        if data.is_empty() {
            return Ok(());
        }

        // Absolute ceiling: a single packet can never hold more than N-8 bytes.
        if self.len + data.len() > N - 8 {
            return Err(NotEnoughSpaceError);
        }

        // Need room for data + 4-byte footer.
        let needed = data.len() + 4;

        if self.force {
            while self.buf.bytes_unused() < needed && !self.buf.empty() {
                self.buf.pop_front();
            }
            if self.buf.bytes_unused() < needed {
                return Err(NotEnoughSpaceError);
            }
        } else if self.buf.bytes_unused() < needed {
            return Err(NotEnoughSpaceError);
        }

        write_wrapping(&mut self.buf.buffer, self.buf.head, data);
        self.buf.head = add_wrapping::<N>(self.buf.head, data.len());
        self.len += data.len();

        Ok(())
    }

    /// Discards the in-progress packet without committing it to the ring buffer.
    ///
    /// Rewinds `head` to the position it had before [`BytearrayRingbuffer::push_multipart`] was
    /// called. Any packets that were already dropped in force mode are permanently lost.
    pub fn cancel(mut self) {
        self.cancelled = true;
        self.buf.head = self.start;
        // Drop runs but the cancelled flag prevents committing.
    }
}

impl<'a, const N: usize> Drop for MultipartPush<'a, N> {
    fn drop(&mut self) {
        if self.cancelled {
            return;
        }
        let len_bytes: [u8; 4] = (self.len as u32).to_ne_bytes();
        // Write header at the reserved slot.
        write_wrapping(&mut self.buf.buffer, self.start, &len_bytes);
        // Write footer immediately after the payload (current head).
        write_wrapping(&mut self.buf.buffer, self.buf.head, &len_bytes);
        self.buf.head = add_wrapping::<N>(self.buf.head, 4);
        self.buf.count += 1;
    }
}

impl<const N: usize> BytearrayRingbuffer<N> {
    /// Creates an empty ring buffer.
    ///
    /// # Panics
    ///
    /// Panics at compile time if `N <= 8` or `N >= u32::MAX`.
    pub const fn new() -> Self {
        assert!(N > 8);
        assert!(N < (u32::MAX as usize));
        Self {
            buffer: [0; N],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Returns the largest payload length that can fit in the currently unused byte range, after
    /// accounting for the 8-byte packet framing (two `u32` lengths).
    ///
    /// Computed from the unused span between write and read positions, minus `8`, saturated at zero.
    pub const fn free(&self) -> usize {
        self.bytes_unused().saturating_sub(8)
    }

    /// Appends `data` as the newest packet.
    ///
    /// # Errors
    ///
    /// Returns [`NotEnoughSpaceError`] if fewer than `data.len() + 8` bytes are unused.
    ///
    /// # Panics
    ///
    /// Panics if `data.len() > u32::MAX` (debug assertion).
    pub fn push(&mut self, data: &[u8]) -> Result<(), NotEnoughSpaceError> {
        self._push(data, false)
    }

    /// Appends `data` as the newest packet, dropping the oldest packets until there is room.
    ///
    /// Unlike [`Self::push`], this never fails for lack of space as long as a single packet can fit
    /// in the backing array (`data.len() <= N - 8`).
    ///
    /// # Errors
    ///
    /// Returns [`NotEnoughSpaceError`] only when `data.len() > N - 8` (one frame cannot fit at all).
    pub fn push_force(&mut self, data: &[u8]) -> Result<(), NotEnoughSpaceError> {
        self._push(data, true)
    }

    /// Begins a multi-part push in normal mode.
    ///
    /// Returns a [`MultipartPush`] guard whose [`MultipartPush::push`] method appends chunks of
    /// payload. When the guard is dropped the completed packet is committed. Call
    /// [`MultipartPush::cancel`] to discard the write.
    ///
    /// # Errors
    ///
    /// Returns [`NotEnoughSpaceError`] if there are fewer than 8 unused bytes (not enough for even
    /// an empty packet).
    pub fn push_multipart(&mut self) -> Result<MultipartPush<'_, N>, NotEnoughSpaceError> {
        // Need at least 8 bytes for the header + footer of an empty packet.
        if self.bytes_unused() < 8 {
            return Err(NotEnoughSpaceError);
        }
        let start = self.head;
        self.head = add_wrapping::<N>(self.head, 4);
        Ok(MultipartPush {
            buf: self,
            start,
            len: 0,
            force: false,
            cancelled: false,
        })
    }

    /// Begins a multi-part push in force mode.
    ///
    /// Like [`Self::push_multipart`] but drops the oldest packets as needed to make room. Dropped
    /// packets are permanently lost even if the write is later cancelled.
    ///
    /// Returns a [`MultipartPush`] guard. Calling [`MultipartPush::push`] will drop further old
    /// packets on demand.
    pub fn push_multipart_force(&mut self) -> MultipartPush<'_, N> {
        // Ensure there are at least 8 bytes free for an empty packet.
        while self.bytes_unused() < 8 && !self.empty() {
            self.pop_front();
        }
        let start = self.head;
        self.head = add_wrapping::<N>(self.head, 4);
        MultipartPush {
            buf: self,
            start,
            len: 0,
            force: true,
            cancelled: false,
        }
    }

    /// Returns `true` if there are no packets stored.
    #[inline(always)]
    pub const fn empty(&self) -> bool {
        self.count == 0
    }

    /// Number of bytes in the ring between `head` and `tail` that do not belong to any packet.
    const fn bytes_unused(&self) -> usize {
        if self.empty() {
            N
        } else if self.head > self.tail {
            N + self.tail - self.head
        } else {
            self.tail - self.head
        }
    }

    fn _push(&mut self, data: &[u8], force: bool) -> Result<(), NotEnoughSpaceError> {
        assert!(data.len() <= u32::MAX as usize);

        // data is longer than entire buffer
        if data.len() > N - 8 {
            return Err(NotEnoughSpaceError);
        }

        // need to overwrite old data to fit new data
        if (data.len() + 8) > self.bytes_unused() {
            if !force {
                return Err(NotEnoughSpaceError);
            }
            while (data.len() + 8) > self.bytes_unused() {
                self.pop_front();
            }
        }

        // write length + data + length
        let addr_a = self.head;
        let addr_b = add_wrapping::<N>(self.head, 4);
        let addr_c = add_wrapping::<N>(self.head, 4 + data.len());
        let len_buffer: [u8; 4] = (data.len() as u32).to_ne_bytes();
        write_wrapping(&mut self.buffer, addr_a, &len_buffer);
        write_wrapping(&mut self.buffer, addr_b, data);
        write_wrapping(&mut self.buffer, addr_c, &len_buffer);

        self.head = add_wrapping::<N>(self.head, 8 + data.len());
        self.count += 1;

        Ok(())
    }

    /// Removes and returns the oldest packet.
    ///
    /// The payload may be split across the end of the backing array; use [`Packet::copy_into`] or
    /// access [`Packet::a`] and [`Packet::b`] directly. If the payload is contiguous, `b` is empty.
    pub fn pop_front(&mut self) -> Option<Packet<'_>> {
        if self.empty() {
            return None;
        }
        let mut len_buffer = [0; 4];
        read_wrapping(&self.buffer, self.tail, &mut len_buffer);
        let len = u32::from_ne_bytes(len_buffer) as usize;

        let index_data = add_wrapping::<N>(self.tail, 4);
        let len_a = (N - index_data).min(len);
        let a = &self.buffer[index_data..index_data + len_a];
        let b = if len_a == len {
            &[]
        } else {
            &self.buffer[..len - len_a]
        };

        self.tail = add_wrapping::<N>(self.tail, len + 8);
        self.count -= 1;
        Some(Packet { a, b })
    }

    /// Borrows the buffer and yields packets from newest to oldest.
    pub fn iter_backwards<'a>(&'a self) -> IterBackwards<'a, N> {
        IterBackwards {
            buffer: &self.buffer,
            head: self.head,
            count: self.count,
        }
    }

    /// Borrows the buffer and yields packets from oldest to newest.
    pub fn iter<'a>(&'a self) -> Iter<'a, N> {
        Iter {
            buffer: &self.buffer,
            head: self.head,
            tail: self.tail,
            count: self.count,
        }
    }

    /// Returns how many packets are stored.
    #[inline(always)]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Returns the `n`-th packet in oldest-to-newest order (`n == 0` is the oldest).
    ///
    /// Same as [`Iterator::nth`] on [`Self::iter`].
    pub fn nth(&self, n: usize) -> Option<Packet<'_>> {
        self.iter().nth(n)
    }

    /// Returns the `n`-th packet in newest-to-oldest order (`n == 0` is the newest).
    ///
    /// Same as [`Iterator::nth`] on [`Self::iter_backwards`].
    pub fn nth_reverse(&self, n: usize) -> Option<Packet<'_>> {
        self.iter_backwards().nth(n)
    }

    /// Returns the `n`-th packet in oldest-to-newest order as a single contiguous slice.
    ///
    /// If the payload already lies in one contiguous range of the backing array, returns that
    /// subslice. If it wraps around the end of the ring, rotates the array in place so the payload is
    /// contiguous at the front, adjusts internal indices, and returns a prefix of the array.
    ///
    /// `n == 0` is the oldest packet. Returns [`None`] if the buffer is empty or if `n >= count()`.
    pub fn nth_contiguous(&mut self, mut n: usize) -> Option<&[u8]> {
        if self.empty() || n >= self.count {
            return None;
        }

        // iterate through buffer until we find this one
        let mut tail = self.tail;
        let len_data = loop {
            let mut buf = [0u8; 4];
            read_wrapping(&self.buffer, tail, &mut buf);
            let len_data = u32::from_ne_bytes(buf) as usize;

            if n == 0 {
                break len_data;
            }
            n -= 1;

            tail = add_wrapping::<N>(tail, len_data + 8);
        };

        let index_data = add_wrapping::<N>(tail, 4);

        // happy path, no rotate necessary
        if index_data + len_data <= N {
            return Some(&self.buffer[index_data..index_data + len_data]);
        }

        // otherwise rotate
        self.buffer.rotate_left(index_data);
        self.tail = sub_wrapping::<N>(self.tail, index_data);
        self.head = sub_wrapping::<N>(self.head, index_data);

        Some(&self.buffer[..len_data])
    }
}

/// Iterator over packets from newest to oldest. See [`BytearrayRingbuffer::iter_backwards`].
pub struct IterBackwards<'a, const N: usize> {
    buffer: &'a [u8; N],
    head: usize,
    count: usize,
}

impl<'a, const N: usize> Iterator for IterBackwards<'a, N> {
    type Item = Packet<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 {
            return None;
        }

        // read length of newest packet
        let index_len = sub_wrapping::<N>(self.head, 4);
        let mut buf = [0u8; 4];
        read_wrapping(self.buffer, index_len, &mut buf);
        let len_data = u32::from_ne_bytes(buf) as usize;
        debug_assert!((len_data + 8) <= N);

        #[cfg(test)]
        {
            let index_len = sub_wrapping::<N>(self.head, 8 + len_data);
            let mut buf = [0u8; 4];
            read_wrapping(self.buffer, index_len, &mut buf);
            let len_2 = u32::from_ne_bytes(buf) as usize;
            assert_eq!(len_data, len_2);
        }

        // read out data
        let index_data = sub_wrapping::<N>(self.head, 4 + len_data);
        let first = (N - index_data).min(len_data);
        let slice_a = &self.buffer[index_data..index_data + first];
        let slice_b = if first < len_data {
            &self.buffer[..len_data - first]
        } else {
            &[]
        };

        self.head = sub_wrapping::<N>(self.head, 8 + len_data);
        self.count -= 1;

        Some(Packet {
            a: slice_a,
            b: slice_b,
        })
    }
}

impl<const N: usize> Default for BytearrayRingbuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over packets from oldest to newest. See [`BytearrayRingbuffer::iter`].
pub struct Iter<'a, const N: usize> {
    buffer: &'a [u8; N],
    head: usize,
    tail: usize,
    count: usize,
}

impl<'a, const N: usize> Iterator for Iter<'a, N> {
    type Item = Packet<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 {
            return None;
        }

        // Occupied span (same as `N - bytes_unused()` for a non-empty queue).
        let bytes_unused = if self.head > self.tail {
            N + self.tail - self.head
        } else {
            self.tail - self.head
        };
        let bytes_occupied = N - bytes_unused;
        debug_assert!(bytes_occupied >= 8);

        // Oldest packet length at `tail`.
        let mut buf = [0u8; 4];
        read_wrapping(self.buffer, self.tail, &mut buf);
        let len_data = u32::from_ne_bytes(buf) as usize;
        debug_assert!((len_data + 8) <= N);
        debug_assert!((len_data + 8) <= bytes_occupied);

        // read out data
        let index_data = add_wrapping::<N>(self.tail, 4);
        let first = (N - index_data).min(len_data);
        let slice_a = &self.buffer[index_data..index_data + first];
        let slice_b = if first < len_data {
            &self.buffer[..len_data - first]
        } else {
            &[]
        };

        self.tail = add_wrapping::<N>(self.tail, 8 + len_data);
        self.count -= 1;

        Some(Packet {
            a: slice_a,
            b: slice_b,
        })
    }
}

fn add_wrapping<const N: usize>(addr: usize, offset: usize) -> usize {
    debug_assert!(addr < N);
    debug_assert!(offset <= N);
    let s = addr + offset;
    if s < N { s } else { s - N }
}

fn sub_wrapping<const N: usize>(addr: usize, offset: usize) -> usize {
    debug_assert!(addr < N);
    debug_assert!(offset <= N);
    if addr >= offset {
        addr - offset
    } else {
        N + addr - offset
    }
}

/// Copies `data` into `buffer` starting at `index`, continuing at index `0` if the write crosses the end.
fn write_wrapping(buffer: &mut [u8], index: usize, data: &[u8]) {
    let first = (buffer.len() - index).min(data.len());
    buffer[index..index + first].copy_from_slice(&data[..first]);
    if first < data.len() {
        buffer[..data.len() - first].copy_from_slice(&data[first..]);
    }
}

/// Fills `data` from `buffer` starting at `index`, wrapping to index `0` when the read crosses the end.
fn read_wrapping(buffer: &[u8], index: usize, data: &mut [u8]) {
    let first = (buffer.len() - index).min(data.len());
    data[..first].copy_from_slice(&buffer[index..index + first]);
    if first < data.len() {
        let remaining = data.len() - first;
        data[first..].copy_from_slice(&buffer[..remaining]);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::BytearrayRingbuffer;

    #[test]
    fn push_some_packets() {
        const N: usize = 64;
        for start_offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = start_offset;
            buf.tail = start_offset;

            let free = 64 - 8;
            assert_eq!(buf.free(), free);

            buf.push(b"01234567").unwrap();
            let free = free - 8 - 8;
            assert_eq!(buf.free(), free);

            buf.push(b"").unwrap();
            let free = free - 8;
            assert_eq!(buf.free(), free);

            buf.push(b"0123").unwrap();
            let free = free - 4 - 8;
            assert_eq!(buf.free(), free);

            buf.push(b"0123").unwrap();
            let free = free - 4 - 8;
            assert_eq!(buf.free(), free);
        }
    }

    #[test]
    fn push_force() {
        let mut buf = BytearrayRingbuffer::<16>::new();
        assert_eq!(buf.bytes_unused(), 16);

        let a = b"012345";
        let b = b"0123";

        buf.push(a).unwrap();
        assert_eq!(buf.bytes_unused(), 16 - a.len() - 8);

        buf.push(b).unwrap_err();
        assert_eq!(buf.bytes_unused(), 16 - a.len() - 8);

        buf.push_force(b).unwrap();
        assert_eq!(buf.bytes_unused(), 16 - b.len() - 8);
    }

    #[test]
    fn push_all_data_lengths() {
        for n in 0..(32 - 8) {
            let mut buf = BytearrayRingbuffer::<32>::new();
            // push n bytes
            let data = (0..n as u8).collect::<Vec<u8>>();

            assert_eq!(buf.free(), 32 - 8);
            buf.push(&data).unwrap();
            assert_eq!(buf.free(), (32usize - 16).saturating_sub(n));
        }
    }

    #[test]
    fn push_sum_of_lengths_possible() {
        let mut buf = BytearrayRingbuffer::<32>::new();
        // push 2 x 8 bytes
        assert_eq!(buf.free(), 32 - 8);
        buf.push(b"01234567").unwrap();
        assert_eq!(buf.free(), 32 - 8 - 16);
        buf.push(b"01234567").unwrap();
        assert_eq!(buf.free(), 0);
    }

    #[test]
    fn push_pop() {
        const N: usize = 64;
        for start_offset in 0..N {
            eprintln!("--------------");
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = start_offset;
            buf.tail = start_offset;

            let data = b"01234567";
            buf.push(data).unwrap();

            let p = buf.pop_front().unwrap();
            let mut out = Vec::new();
            out.extend_from_slice(p.a);
            out.extend_from_slice(p.b);

            dbg!(out.as_slice());
            assert!(data == out.as_slice());

            assert_eq!(buf.head, buf.tail);
            assert_eq!(buf.bytes_unused(), N);
        }
    }

    #[test]
    fn push_read_back() {
        let data = [b"hello world" as &[u8], b"", b"test"];

        const N: usize = 64;
        for start_offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = start_offset;
            buf.tail = start_offset;

            for &d in &data {
                buf.push(d).unwrap();
            }

            // test forward iteration
            let mut it = buf.iter();
            for &d in data.iter() {
                let p = it.next().unwrap();
                let mut ab = Vec::new();
                ab.extend_from_slice(p.a);
                ab.extend_from_slice(p.b);
                let ab = ab.as_slice();
                assert_eq!(d, ab);
            }
            assert_eq!(it.next(), None);

            // test backward iteration
            let mut it = buf.iter_backwards();
            for &d in data.iter().rev() {
                let p = it.next().unwrap();
                let mut ab = Vec::new();
                ab.extend_from_slice(p.a);
                ab.extend_from_slice(p.b);
                let ab = ab.as_slice();
                assert_eq!(d, ab);
            }
            assert_eq!(it.next(), None);
        }
    }

    #[test]
    fn push_count() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"1234").unwrap();
        assert_eq!(buf.count(), 1);
        buf.push(b"1234").unwrap();
        assert_eq!(buf.count(), 2);
        buf.push(b"1234").unwrap();
        assert_eq!(buf.count(), 3);
    }

    fn test_with_readback<const N: usize>(words: &[&'static str]) {
        eprintln!("--------------------------");
        let mut buf = BytearrayRingbuffer::<N>::new();
        let mut current_words = VecDeque::new();
        for &word in words {
            eprintln!("adding {word:?}");
            let word = word.to_owned();
            let current_bytes: usize = current_words.iter().map(|w: &String| w.len() + 8).sum();
            if current_bytes + 8 + word.len() > N {
                current_words.pop_front();
            }

            buf.push_force(word.as_bytes()).unwrap();
            current_words.push_back(word);

            for (p, word) in buf.iter_backwards().zip(current_words.iter().rev()) {
                eprintln!("read back {word:?}");
                let mut st = String::new();
                st.push_str(core::str::from_utf8(p.a).unwrap());
                st.push_str(core::str::from_utf8(p.b).unwrap());
                assert_eq!(st, *word);
            }
        }
    }

    #[test]
    fn readback_various() {
        test_with_readback::<32>(&["ab", "123", "hello", "world"]);
        test_with_readback::<32>(&["", "", "a", "", "", ""]);
        test_with_readback::<32>(&["", "", "ab", "", "", ""]);
        test_with_readback::<32>(&["", "", "abc", "", "", ""]);
        test_with_readback::<32>(&["", "", "abcd", "", "", ""]);
        test_with_readback::<32>(&["", "", "abcde", "", "", ""]);

        test_with_readback::<24>(&["0", "1", "a", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "ab", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "abc", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "abcd", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "abcde", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "abcdef", "2", "3", "4"]);
        test_with_readback::<24>(&["0", "1", "abcdefg", "2", "3", "4"]);
    }

    #[test]
    fn nth_contiguous_out_of_range_returns_none() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        assert_eq!(buf.count(), 1);

        assert_eq!(buf.nth_contiguous(1), None);
    }

    #[test]
    fn rotate_contiguous() {
        const N: usize = 48;
        let data: [&[u8]; _] = [b"012345", b"hello world", b"xyz"];

        for offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = offset;
            buf.tail = offset;

            for &d in &data {
                buf.push(d).unwrap();
            }

            let read = buf.nth_contiguous(1).unwrap();
            assert_eq!(data[1], read);

            // check if the contents are still the same
            for (&r, p) in data.iter().zip(buf.iter()) {
                let mut out = Vec::new();
                out.extend_from_slice(p.a);
                out.extend_from_slice(p.b);
                assert_eq!(out.as_slice(), r);
            }
        }
    }

    // ---- multipart push tests ----

    fn collect(a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v
    }

    #[test]
    fn multipart_normal_fits() {
        const N: usize = 64;
        for offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = offset;
            buf.tail = offset;

            let mut mp = buf.push_multipart().unwrap();
            mp.push(b"hello").unwrap();
            mp.push(b" ").unwrap();
            mp.push(b"world").unwrap();
            drop(mp);

            assert_eq!(buf.count(), 1);
            let p = buf.pop_front().unwrap();
            assert_eq!(collect(p.a, p.b), b"hello world");
            assert_eq!(buf.count(), 0);
        }
    }

    #[test]
    fn multipart_empty_packet() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        let mp = buf.push_multipart().unwrap();
        drop(mp); // no push calls
        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), b"");
    }

    #[test]
    fn multipart_normal_overflow_returns_err() {
        // Buffer: 24 bytes. One existing packet of 8 bytes (payload 0, 8 bytes overhead).
        // That leaves 16 bytes unused. Starting multipart reserves 4 for the header → 12 bytes
        // free for payload+footer. First push of 4 bytes is fine (leaves 8 for footer). Second
        // push of 5 bytes should fail (needs 9 for data+footer, only 8 remain).
        let mut buf = BytearrayRingbuffer::<24>::new();
        buf.push(b"").unwrap(); // occupies 8 bytes, leaving 16 unused

        let mut mp = buf.push_multipart().unwrap();
        mp.push(b"abcd").unwrap(); // 4 bytes payload + 4 footer still needed → OK
        let err = mp.push(b"12345"); // would need 9 bytes (5 data + 4 footer), only 8 unused → Err
        assert!(err.is_err());

        // The guard is still usable; drop it and confirm the partial write is committed.
        drop(mp);
        assert_eq!(buf.count(), 2);
        // The committed packet contains only the first successful chunk.
        let p = buf.nth(1).unwrap();
        assert_eq!(collect(p.a, p.b), b"abcd");
    }

    #[test]
    fn multipart_cancel_normal_mode() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        let original_unused = buf.bytes_unused();
        let original_count = buf.count();

        let mut mp = buf.push_multipart().unwrap();
        mp.push(b"data that will be discarded").unwrap();
        mp.cancel();

        assert_eq!(buf.count(), original_count);
        assert_eq!(buf.bytes_unused(), original_unused);
    }

    #[test]
    fn multipart_normal_no_room_for_start() {
        // 24 - 8 = 16 max payload; filling with 16 bytes leaves 0 bytes unused.
        let mut buf = BytearrayRingbuffer::<24>::new();
        buf.push(&[0u8; 16]).unwrap(); // fills entire buffer
        assert_eq!(buf.bytes_unused(), 0);
        assert!(buf.push_multipart().is_err());
    }

    #[test]
    fn multipart_force_drops_old_packets() {
        // Buffer: 24 bytes. Push two 2-byte packets (10 bytes each → 20 used, 4 free).
        // push_multipart_force initially pops "AA" to get 8+ bytes for the header reservation.
        // Then push "hello world" (11 bytes) needs 11+4=15 bytes; only 10 available after the
        // header reservation, so "BB" is also dropped, leaving 24 bytes free.
        let mut buf = BytearrayRingbuffer::<24>::new();
        buf.push(b"AA").unwrap(); // 10 bytes
        buf.push(b"BB").unwrap(); // 10 bytes → 20 bytes used, 4 bytes free
        assert_eq!(buf.count(), 2);

        let mut mp = buf.push_multipart_force();
        mp.push(b"hello world").unwrap(); // 11+4=15 needed; forces drop of both old packets
        drop(mp);

        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), b"hello world");
    }

    #[test]
    fn multipart_force_cancel_drops_are_permanent() {
        // Same setup as multipart_force_drops_old_packets but we cancel instead of committing.
        let mut buf = BytearrayRingbuffer::<24>::new();
        buf.push(b"AA").unwrap();
        buf.push(b"BB").unwrap();
        let count_before = buf.count(); // 2

        let mut mp = buf.push_multipart_force();
        mp.push(b"hello world").unwrap(); // forces drops of both "AA" and "BB"
        mp.cancel(); // discard the new packet

        // New packet is not committed, but both dropped packets are permanently gone.
        assert!(buf.count() < count_before);
        assert_eq!(buf.count(), 0);
    }

    #[test]
    fn multipart_push_after_multipart() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        {
            let mut mp = buf.push_multipart().unwrap();
            mp.push(b"first").unwrap();
        }
        buf.push(b"second").unwrap();

        assert_eq!(buf.count(), 2);
        let p = buf.nth(0).unwrap();
        assert_eq!(collect(p.a, p.b), b"first");
        let p = buf.nth(1).unwrap();
        assert_eq!(collect(p.a, p.b), b"second");
    }

    #[test]
    fn multipart_force_max_payload() {
        // Push exactly N-8 bytes in force mode.
        const N: usize = 32;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.push(b"old").unwrap(); // will be displaced

        let payload: Vec<u8> = (0..((N - 8) as u8)).collect();
        let mut mp = buf.push_multipart_force();
        mp.push(&payload).unwrap();
        drop(mp);

        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), payload);
    }

    #[test]
    fn multipart_wraparound_all_offsets() {
        const N: usize = 48;
        for offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = offset;
            buf.tail = offset;

            // Push a normal packet first.
            buf.push(b"prefix").unwrap();

            // Multi-part push with two chunks that together wrap the ring.
            let mut mp = buf.push_multipart().unwrap();
            mp.push(b"foo").unwrap();
            mp.push(b"bar").unwrap();
            drop(mp);

            // Push another after.
            buf.push(b"suffix").unwrap();

            assert_eq!(buf.count(), 3);
            let p = buf.nth(0).unwrap();
            assert_eq!(collect(p.a, p.b), b"prefix");
            let p = buf.nth(1).unwrap();
            assert_eq!(collect(p.a, p.b), b"foobar");
            let p = buf.nth(2).unwrap();
            assert_eq!(collect(p.a, p.b), b"suffix");
        }
    }

    // ---- pop_front / empty / count lifecycle ----

    #[test]
    fn pop_front_empty_returns_none() {
        let mut buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.pop_front(), None);
    }

    #[test]
    fn empty_and_count_lifecycle() {
        let mut buf = BytearrayRingbuffer::<32>::new();

        // Fresh buffer is empty with count 0.
        assert!(buf.empty());
        assert_eq!(buf.count(), 0);

        buf.push(b"a").unwrap();
        assert!(!buf.empty());
        assert_eq!(buf.count(), 1);

        buf.push(b"b").unwrap();
        assert!(!buf.empty());
        assert_eq!(buf.count(), 2);

        buf.pop_front().unwrap();
        assert!(!buf.empty());
        assert_eq!(buf.count(), 1);

        buf.pop_front().unwrap();
        assert!(buf.empty());
        assert_eq!(buf.count(), 0);

        // pop on now-empty buffer returns None.
        assert_eq!(buf.pop_front(), None);
    }

    #[test]
    fn default_creates_empty_buffer() {
        let buf = BytearrayRingbuffer::<32>::default();
        assert!(buf.empty());
        assert_eq!(buf.count(), 0);
        assert_eq!(buf.free(), 32 - 8);
    }

    // ---- oversized payload rejection ----

    #[test]
    fn push_oversized_returns_error() {
        let mut buf = BytearrayRingbuffer::<16>::new();
        // Maximum payload is N-8 = 8 bytes; 9 bytes must be rejected.
        let oversized = [0u8; 9];
        assert!(buf.push(&oversized).is_err());
        // Buffer must remain unmodified.
        assert!(buf.empty());
    }

    #[test]
    fn push_force_oversized_returns_error() {
        let mut buf = BytearrayRingbuffer::<16>::new();
        // Even push_force must reject payloads larger than N-8.
        let oversized = [0u8; 9];
        assert!(buf.push_force(&oversized).is_err());
        assert!(buf.empty());
    }

    // ---- iter / iter_backwards on empty buffer ----

    #[test]
    fn iter_empty_buffer() {
        let buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.iter().next(), None);
    }

    #[test]
    fn iter_backwards_empty_buffer() {
        let buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.iter_backwards().next(), None);
    }

    // ---- nth / nth_reverse ----

    #[test]
    fn nth_empty_returns_none() {
        let buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.nth(0), None);
    }

    #[test]
    fn nth_out_of_bounds_returns_none() {
        let buf = {
            let mut b = BytearrayRingbuffer::<64>::new();
            b.push(b"x").unwrap();
            b.push(b"y").unwrap();
            b
        };
        assert_eq!(buf.nth(2), None);
        assert_eq!(buf.nth(100), None);
    }

    #[test]
    fn nth_reverse_basic() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"oldest").unwrap();
        buf.push(b"middle").unwrap();
        buf.push(b"newest").unwrap();

        let p = buf.nth_reverse(0).unwrap();
        assert_eq!(collect(p.a, p.b), b"newest");
        let p = buf.nth_reverse(1).unwrap();
        assert_eq!(collect(p.a, p.b), b"middle");
        let p = buf.nth_reverse(2).unwrap();
        assert_eq!(collect(p.a, p.b), b"oldest");
    }

    #[test]
    fn nth_reverse_empty_returns_none() {
        let buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.nth_reverse(0), None);
    }

    #[test]
    fn nth_reverse_out_of_bounds_returns_none() {
        let buf = {
            let mut b = BytearrayRingbuffer::<64>::new();
            b.push(b"only").unwrap();
            b
        };
        assert_eq!(buf.nth_reverse(1), None);
        assert_eq!(buf.nth_reverse(99), None);
    }

    // ---- nth_contiguous ----

    #[test]
    fn nth_contiguous_empty_returns_none() {
        let mut buf = BytearrayRingbuffer::<32>::new();
        assert_eq!(buf.nth_contiguous(0), None);
    }

    #[test]
    fn nth_contiguous_n0_no_rotation() {
        // Packet at the beginning of the buffer – no rotation needed.
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        buf.push(b"world").unwrap();
        // n=0 is the oldest packet, which starts at index 4 (after its 4-byte header).
        // It is contiguous, so the buffer should not be rotated.
        let slice = buf.nth_contiguous(0).unwrap();
        assert_eq!(slice, b"hello");
        // Remaining packets are still accessible.
        let slice = buf.nth_contiguous(1).unwrap();
        assert_eq!(slice, b"world");
    }

    #[test]
    fn nth_contiguous_called_twice() {
        // After the first nth_contiguous triggers a rotation, the second call must
        // still return the correct data for the (now relocated) packet.
        const N: usize = 48;
        for offset in 0..N {
            let mut buf = BytearrayRingbuffer::<N>::new();
            buf.head = offset;
            buf.tail = offset;

            buf.push(b"alpha").unwrap();
            buf.push(b"beta").unwrap();
            buf.push(b"gamma").unwrap();

            // First call (may or may not trigger a rotation depending on offset).
            let slice = buf.nth_contiguous(1).unwrap();
            assert_eq!(slice, b"beta");

            // Second call on the same (possibly rotated) buffer.
            let slice = buf.nth_contiguous(2).unwrap();
            assert_eq!(slice, b"gamma");

            // Forward iteration must still be consistent.
            let payloads: Vec<Vec<u8>> = buf
                .iter()
                .map(|p| {
                    let mut v = p.a.to_vec();
                    v.extend_from_slice(p.b);
                    v
                })
                .collect();
            assert_eq!(payloads[0], b"alpha");
            assert_eq!(payloads[1], b"beta");
            assert_eq!(payloads[2], b"gamma");
        }
    }

    // ---- free() edge cases ----

    #[test]
    fn free_returns_zero_when_full() {
        // N=32, two packets of 8-byte payload each fill the buffer exactly (2×16 = 32).
        let mut buf = BytearrayRingbuffer::<32>::new();
        buf.push(b"01234567").unwrap();
        buf.push(b"01234567").unwrap();
        assert_eq!(buf.free(), 0);
        // A third push must fail.
        assert!(buf.push(b"x").is_err());
    }

    // ---- push_force dropping multiple packets ----

    #[test]
    fn push_force_drops_multiple_packets() {
        // N=32; fill with three 1-byte packets (each 9 bytes → 27 bytes used, 5 bytes free).
        // A 16-byte payload needs 24 bytes (16 + 8 overhead).  Dropping "a" frees 9 → 14 free
        // (still < 24); dropping "b" → 23 free (still < 24); dropping "c" → 32 free (≥ 24).
        // So all three old packets are evicted and only the new one survives.
        let mut buf = BytearrayRingbuffer::<32>::new();
        buf.push(b"a").unwrap();
        buf.push(b"b").unwrap();
        buf.push(b"c").unwrap();
        assert_eq!(buf.count(), 3);

        let payload = b"0123456789abcdef"; // 16 bytes
        buf.push_force(payload).unwrap();

        // Only the newly pushed packet must survive.
        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), payload);
    }

    // ---- multipart: total payload exceeds N-8 ----

    #[test]
    fn multipart_push_total_exceeds_max_returns_error() {
        // Buffer size 16: max payload = 16-8 = 8 bytes.
        // Push 5 bytes then try to push 4 more (total 9 > 8) – must fail.
        let mut buf = BytearrayRingbuffer::<16>::new();
        let mut mp = buf.push_multipart().unwrap();
        mp.push(b"abcde").unwrap(); // 5 bytes – fine
        let err = mp.push(b"wxyz"); // would bring total to 9 > 8
        assert!(err.is_err());
        drop(mp); // commits the 5-byte partial packet
        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), b"abcde");
    }

    // ---- push_multipart_force starting from a completely full buffer ----

    #[test]
    fn multipart_force_full_buffer_start() {
        // N=16, fill with the maximum-size single packet (8-byte payload).
        // push_multipart_force must drop it to reserve space for the header.
        let mut buf = BytearrayRingbuffer::<16>::new();
        buf.push(&[0u8; 8]).unwrap(); // fills the entire 16-byte buffer
        assert_eq!(buf.bytes_unused(), 0);

        let mut mp = buf.push_multipart_force();
        mp.push(b"hi").unwrap();
        drop(mp);

        assert_eq!(buf.count(), 1);
        let p = buf.pop_front().unwrap();
        assert_eq!(collect(p.a, p.b), b"hi");
    }

    // ---- Packet::copy_into ----

    #[test]
    fn copy_into_contiguous() {
        // Packet starting at offset 0: payload lies in a single contiguous slice (b is empty).
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        let p = buf.pop_front().unwrap();
        assert!(p.b.is_empty());
        let mut out = [0u8; 5];
        p.copy_into(&mut out);
        assert_eq!(&out, b"hello");
    }

    #[test]
    fn copy_into_wrapped() {
        // N=16, head=tail=9: a 5-byte payload wraps (3 bytes at end, 2 bytes at start).
        const N: usize = 16;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.head = 9;
        buf.tail = 9;
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        assert!(!p.b.is_empty(), "expected a wrapped packet");
        let mut out = [0u8; 5];
        p.copy_into(&mut out);
        assert_eq!(&out, b"abcde");
    }

    #[test]
    #[should_panic(expected = "buffer length must equal packet length")]
    fn copy_into_wrong_length_panics() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 4]; // one byte too short
        p.copy_into(&mut out);
    }

    // ---- Packet::copy_part_into ----

    #[test]
    fn copy_part_into_in_a() {
        // Range fully within the first slice.
        // N=16, head=tail=9: a="abc" (3 bytes), b="de" (2 bytes).
        const N: usize = 16;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.head = 9;
        buf.tail = 9;
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        assert!(!p.b.is_empty(), "expected a wrapped packet");
        let mut out = [0u8; 2];
        p.copy_part_into(0..2, &mut out);
        assert_eq!(&out, b"ab");
    }

    #[test]
    fn copy_part_into_in_b() {
        // Range fully within the second slice.
        // a="abc" (3 bytes), b="de" (2 bytes); range 3..4 selects "d".
        const N: usize = 16;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.head = 9;
        buf.tail = 9;
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 1];
        p.copy_part_into(3..4, &mut out);
        assert_eq!(&out, b"d");
    }

    #[test]
    fn copy_part_into_spanning() {
        // Range spanning the boundary between a and b.
        // a="abc" (3 bytes), b="de" (2 bytes); range 1..4 selects "bcd".
        const N: usize = 16;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.head = 9;
        buf.tail = 9;
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 3];
        p.copy_part_into(1..4, &mut out);
        assert_eq!(&out, b"bcd");
    }

    #[test]
    #[should_panic(expected = "buffer length must equal range length")]
    fn copy_part_into_wrong_buffer_length_panics() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 3]; // range is 0..2 (2 bytes) but buffer is 3 bytes
        p.copy_part_into(0..2, &mut out);
    }

    #[test]
    #[should_panic(expected = "range out of bounds")]
    fn copy_part_into_out_of_bounds_panics() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap(); // 5 bytes
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 2];
        p.copy_part_into(4..6, &mut out); // end=6 > total=5
    }

    // ---- Packet::copy_part_into (non-Range variants) ----

    #[test]
    fn copy_part_into_range_inclusive() {
        // 1..=3 selects "bcd" from "abcde".
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 3];
        p.copy_part_into(1..=3, &mut out);
        assert_eq!(&out, b"bcd");
    }

    #[test]
    fn copy_part_into_range_from() {
        // 3.. selects "de" from "abcde".
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 2];
        p.copy_part_into(3.., &mut out);
        assert_eq!(&out, b"de");
    }

    #[test]
    fn copy_part_into_range_to() {
        // ..3 selects "abc" from "abcde".
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 3];
        p.copy_part_into(..3, &mut out);
        assert_eq!(&out, b"abc");
    }

    #[test]
    fn copy_part_into_range_full() {
        // .. selects the entire payload "abcde".
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 5];
        p.copy_part_into(.., &mut out);
        assert_eq!(&out, b"abcde");
    }

    #[test]
    fn copy_part_into_range_to_inclusive() {
        // ..=2 selects "abc" from "abcde".
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        let mut out = [0u8; 3];
        p.copy_part_into(..=2, &mut out);
        assert_eq!(&out, b"abc");
    }

    // ---- Packet::len / is_empty ----

    #[test]
    fn packet_len_contiguous() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"hello").unwrap();
        let p = buf.pop_front().unwrap();
        assert_eq!(p.len(), 5);
        assert!(!p.is_empty());
    }

    #[test]
    fn packet_len_wrapped() {
        const N: usize = 16;
        let mut buf = BytearrayRingbuffer::<N>::new();
        buf.head = 9;
        buf.tail = 9;
        buf.push(b"abcde").unwrap();
        let p = buf.pop_front().unwrap();
        assert!(!p.b.is_empty(), "expected a wrapped packet");
        assert_eq!(p.len(), 5);
        assert!(!p.is_empty());
    }

    #[test]
    fn packet_len_empty_payload() {
        let mut buf = BytearrayRingbuffer::<64>::new();
        buf.push(b"").unwrap();
        let p = buf.pop_front().unwrap();
        assert_eq!(p.len(), 0);
        assert!(p.is_empty());
    }
}
