// Protocol module - HOT PATH
// RESP protocol parser and writer - performance critical
// All functions are marked #[inline(always)] where appropriate

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::CONFIG;
use crate::store::{get_buffer, return_buffer};

// Security limits for RESP protocol parsing (prevent DoS attacks)
pub const MAX_ARRAY_LEN: usize = 1_000_000;
pub const MAX_STRING_LEN: usize = 512_000_000;
pub const MAX_BUFFER_SIZE: usize = 1_073_741_824;

// ==================== RESP Parser ====================

pub struct RespParser {
    pub buffer: BytesMut,
}

impl RespParser {
    #[inline]
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(CONFIG.server.buffer_size),
        }
    }

    #[inline(always)]
    pub fn has_buffered_data(&self) -> bool {
        !self.buffer.is_empty()
    }

    pub async fn parse_command<S>(&mut self, stream: &mut S) -> Result<Vec<Bytes>, ()>
    where
        S: AsyncRead + Unpin,
    {
        loop {
            match self.try_parse() {
                Ok(Some(cmd)) => return Ok(cmd),
                Ok(None) => {
                    if self.buffer.len() > MAX_BUFFER_SIZE {
                        return Err(());
                    }
                    if stream.read_buf(&mut self.buffer).await.is_err() {
                        return Err(());
                    }
                    if self.buffer.is_empty() {
                        return Err(());
                    }
                }
                Err(_) => return Err(()),
            }
        }
    }

    fn try_parse(&mut self) -> Result<Option<Vec<Bytes>>, ()> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }

        let mut cursor = 0;
        let len = self.buffer.len();

        if self.buffer[cursor] != b'*' {
            return Err(());
        }
        cursor += 1;

        let mut array_len = 0usize;
        loop {
            if cursor >= len {
                return Ok(None);
            }
            let byte = self.buffer[cursor];
            if byte == b'\r' {
                break;
            }
            if !byte.is_ascii_digit() {
                return Err(());
            }
            array_len = array_len * 10 + (byte - b'0') as usize;

            if array_len > MAX_ARRAY_LEN {
                return Err(());
            }

            cursor += 1;
        }

        if cursor + 1 >= len || self.buffer[cursor + 1] != b'\n' {
            return Ok(None);
        }
        cursor += 2;

        let mut result = Vec::with_capacity(array_len);

        for _ in 0..array_len {
            if cursor >= len || self.buffer[cursor] != b'$' {
                return Ok(None);
            }
            cursor += 1;

            let mut str_len = 0usize;
            loop {
                if cursor >= len {
                    return Ok(None);
                }
                let byte = self.buffer[cursor];
                if byte == b'\r' {
                    break;
                }
                if !byte.is_ascii_digit() {
                    return Err(());
                }
                str_len = str_len * 10 + (byte - b'0') as usize;

                if str_len > MAX_STRING_LEN {
                    return Err(());
                }

                cursor += 1;
            }

            if cursor + 1 >= len || self.buffer[cursor + 1] != b'\n' {
                return Ok(None);
            }
            cursor += 2;

            if cursor + str_len + 2 > len {
                return Ok(None);
            }

            let start = cursor;
            let end = cursor + str_len;
            result.push(Bytes::copy_from_slice(&self.buffer[start..end]));
            cursor += str_len + 2;
        }

        self.buffer.advance(cursor);
        Ok(Some(result))
    }
}

// ==================== RESP Writer ====================

pub struct RespWriter {
    buffer: Vec<u8>,
}

impl RespWriter {
    pub fn new() -> Self {
        Self {
            buffer: get_buffer(),
        }
    }

    #[inline(always)]
    pub fn write_u64(&mut self, mut n: u64) {
        if n == 0 {
            self.buffer.push(b'0');
            return;
        }

        let mut buf = [0u8; 20];
        let mut i = 20;

        while n > 0 {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }

        self.buffer.extend_from_slice(&buf[i..]);
    }

    #[inline(always)]
    pub fn write_simple_string(&mut self, s: &[u8]) {
        self.buffer.push(b'+');
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_bulk_string(&mut self, s: &[u8]) {
        self.buffer.push(b'$');
        self.write_u64(s.len() as u64);
        self.buffer.extend_from_slice(b"\r\n");
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_null(&mut self) {
        self.buffer.extend_from_slice(b"$-1\r\n");
    }

    #[inline(always)]
    pub fn write_integer(&mut self, i: usize) {
        self.buffer.push(b':');
        self.write_u64(i as u64);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_signed_integer(&mut self, i: i64) {
        self.buffer.push(b':');
        if i < 0 {
            self.buffer.push(b'-');
            self.write_u64((-i) as u64);
        } else {
            self.write_u64(i as u64);
        }
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_error(&mut self, s: &[u8]) {
        self.buffer.extend_from_slice(b"-ERR ");
        self.buffer.extend_from_slice(s);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_array(&mut self, arr: &[Bytes]) {
        self.buffer.push(b'*');
        self.write_u64(arr.len() as u64);
        self.buffer.extend_from_slice(b"\r\n");
        for item in arr {
            self.write_bulk_string(item);
        }
    }

    #[inline(always)]
    pub fn write_array_header(&mut self, len: usize) {
        self.buffer.push(b'*');
        self.write_u64(len as u64);
        self.buffer.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn should_flush(&self) -> bool {
        self.buffer.len() >= 8192
    }

    #[inline(always)]
    pub async fn flush<S>(&mut self, stream: &mut S) -> Result<(), ()>
    where
        S: AsyncWrite + Unpin,
    {
        if !self.buffer.is_empty() {
            stream.write_all(&self.buffer).await.map_err(|_| ())?;
            self.buffer.clear();
        }
        Ok(())
    }
}

impl Default for RespWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RespWriter {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.buffer);
        return_buffer(buf);
    }
}

// ==================== Parsing Helpers ====================

#[inline(always)]
pub fn parse_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut val = 0u64;
    for &b in bytes.iter() {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(val)
}

#[inline(always)]
pub fn parse_i64(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (negative, start) = if bytes[0] == b'-' {
        (true, 1)
    } else {
        (false, 0)
    };
    if start >= bytes.len() {
        return None;
    }
    let mut val = 0i64;
    for &b in bytes[start..].iter() {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    if negative { Some(-val) } else { Some(val) }
}

#[inline(always)]
pub fn eq_ignore_case_3(a: &[u8], b: &[u8; 3]) -> bool {
    a.len() == 3 && (a[0] | 0x20) == b[0] && (a[1] | 0x20) == b[1] && (a[2] | 0x20) == b[2]
}

#[inline(always)]
pub fn eq_ignore_case_6(a: &[u8], b: &[u8; 6]) -> bool {
    a.len() == 6
        && (a[0] | 0x20) == b[0]
        && (a[1] | 0x20) == b[1]
        && (a[2] | 0x20) == b[2]
        && (a[3] | 0x20) == b[3]
        && (a[4] | 0x20) == b[4]
        && (a[5] | 0x20) == b[5]
}
