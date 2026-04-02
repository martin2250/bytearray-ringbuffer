#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

/// Fixed-capacity FIFO of variable-length byte slices, backed by `[u8; N]` with no heap allocation.
///
/// Each stored packet uses `data.len() + 8` bytes: a leading `u32` length (native endian), the
/// payload, then the same length again. The queue is a ring: `head` is where the next `push` writes;
/// `tail` is the oldest packet. Payloads may wrap across the end of the array; most accessors return
/// two slices `(a, b)` that concatenate to the full packet.
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

/// Returned when a [`BytearrayRingbuffer::push`] cannot store `data` without dropping older packets.
///
/// For [`BytearrayRingbuffer::push`], this means the unused region is too small. For
/// [`BytearrayRingbuffer::push_force`], this is only returned when `data.len() > N - 8` (a single
/// packet cannot fit in the buffer at all).
#[derive(Copy, Clone, Debug)]
pub struct NotEnoughSpaceError;

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
    /// The payload may be split across the end of the backing array; concatenate the two slices to
    /// reconstruct `data`. If the payload is contiguous, the second slice is empty.
    pub fn pop_front(&mut self) -> Option<(&[u8], &[u8])> {
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
        Some((a, b))
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
    pub fn nth(&self, n: usize) -> Option<(&[u8], &[u8])> {
        self.iter().nth(n)
    }

    /// Returns the `n`-th packet in newest-to-oldest order (`n == 0` is the newest).
    ///
    /// Same as [`Iterator::nth`] on [`Self::iter_backwards`].
    pub fn nth_reverse(&self, n: usize) -> Option<(&[u8], &[u8])> {
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
    type Item = (&'a [u8], &'a [u8]);

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

        Some((slice_a, slice_b))
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
    type Item = (&'a [u8], &'a [u8]);

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

        Some((slice_a, slice_b))
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

            let (a, b) = buf.pop_front().unwrap();
            let mut out = Vec::new();
            out.extend_from_slice(a);
            out.extend_from_slice(b);

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
                let (a, b) = it.next().unwrap();
                let mut ab = Vec::new();
                ab.extend_from_slice(a);
                ab.extend_from_slice(b);
                let ab = ab.as_slice();
                assert_eq!(d, ab);
            }
            assert_eq!(it.next(), None);

            // test backward iteration
            let mut it = buf.iter_backwards();
            for &d in data.iter().rev() {
                let (a, b) = it.next().unwrap();
                let mut ab = Vec::new();
                ab.extend_from_slice(a);
                ab.extend_from_slice(b);
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

            for (a, b) in buf.iter_backwards().zip(current_words.iter().rev()) {
                eprintln!("read back {b:?}");
                let mut st = String::new();
                st.push_str(core::str::from_utf8(a.0).unwrap());
                st.push_str(core::str::from_utf8(a.1).unwrap());
                assert_eq!(st, *b);
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
            for (&r, (a, b)) in data.iter().zip(buf.iter()) {
                let mut out = Vec::new();
                out.extend_from_slice(a);
                out.extend_from_slice(b);
                assert_eq!(out.as_slice(), r);
            }
        }
    }
}
