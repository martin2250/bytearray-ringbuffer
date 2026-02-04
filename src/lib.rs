#![cfg_attr(not(test), no_std)]

pub struct BytearrayRingbuffer<const N: usize> {
    buffer: [u8; N],
    /// points to where the next packet will be written
    head: usize,
    /// points to where the oldest packet starts
    tail: usize,
    /// resolves ambiguity between head == tail for empty and full
    empty: bool,
}

#[derive(Copy, Clone, Debug)]
pub struct NotEnoughSpaceError;

impl<const N: usize> BytearrayRingbuffer<N> {
    pub const fn new() -> Self {
        assert!(N > 8);
        assert!(N < (u32::MAX as usize));
        Self {
            buffer: [0; N],
            head: 0,
            tail: 0,
            empty: true,
        }
    }

    /// number of bytes available for payload, 8 bytes for header + end are already subtracted
    pub const fn free(&self) -> usize {
        self.bytes_unused().saturating_sub(8)
    }

    /// add entry, returns false if there was not enough space
    pub fn push(&mut self, data: &[u8]) -> Result<(), NotEnoughSpaceError> {
        self._push(data, false)
    }

    /// add entry, discard old entries if there was not enough space
    pub fn push_force(&mut self, data: &[u8]) -> Result<(), NotEnoughSpaceError> {
        self._push(data, true)
    }

    /// number of bytes are currently not in use
    const fn bytes_unused(&self) -> usize {
        if self.empty {
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
        self.empty = false;

        Ok(())
    }

    pub fn pop_front(&mut self) -> Option<(&[u8], &[u8])> {
        if self.empty {
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
        self.empty = self.head == self.tail;
        Some((a, b))
    }

    pub fn iter_backwards<'a>(&'a self) -> IterBackwards<'a, N> {
        IterBackwards {
            buffer: &self.buffer,
            head: self.head,
            tail: self.tail,
            empty: self.empty,
        }
    }

    pub fn iter<'a>(&'a self) -> Iter<'a, N> {
        Iter {
            buffer: &self.buffer,
            head: self.head,
            tail: self.tail,
            empty: self.empty,
        }
    }

    /// return the number of valid entries
    pub fn count(&self) -> usize {
        self.iter_backwards().count()
    }

    pub fn nth(&self, n: usize) -> Option<(&[u8], &[u8])> {
        self.iter_backwards().nth(n)
    }
}

pub struct IterBackwards<'a, const N: usize> {
    buffer: &'a [u8; N],
    head: usize,
    tail: usize,
    empty: bool,
}

impl<'a, const N: usize> Iterator for IterBackwards<'a, N> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.empty {
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
        self.empty = self.head == self.tail;

        Some((slice_a, slice_b))
    }
}

impl<const N: usize> Default for BytearrayRingbuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Iter<'a, const N: usize> {
    buffer: &'a [u8; N],
    head: usize,
    tail: usize,
    empty: bool,
}

impl<'a, const N: usize> Iterator for Iter<'a, N> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.empty {
            return None;
        }

        // check how many bytes are valid
        let bytes_used = if self.head > self.tail {
            N + self.tail - self.head
        } else {
            self.tail - self.head
        };
        let bytes_valid = N - bytes_used;
        debug_assert!(bytes_valid >= 8);

        // read length of newest packet
        let mut buf = [0u8; 4];
        read_wrapping(self.buffer, self.tail, &mut buf);
        let len_data = u32::from_ne_bytes(buf) as usize;
        debug_assert!((len_data + 8) <= N);
        debug_assert!((len_data + 8) <= bytes_valid);

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
        self.empty = self.head == self.tail;

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

/// write data to buffer, starting at index and wrapping around at the end of the buffer
fn write_wrapping(buffer: &mut [u8], index: usize, data: &[u8]) {
    let first = (buffer.len() - index).min(data.len());
    buffer[index..index + first].copy_from_slice(&data[..first]);
    if first < data.len() {
        buffer[..data.len() - first].copy_from_slice(&data[first..]);
    }
}

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
}
