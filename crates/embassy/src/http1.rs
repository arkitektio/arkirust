//! A small HTTP/1.1 client over any `embedded_io_async` stream: one exchange
//! per connection (`Connection: close`), bodies by `Content-Length`, chunked,
//! or until the connection ends. What fakts' and rath's requests need.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use embedded_io_async::{Read, Write};

/// A response's headers must fit in this.
pub const MAX_HEADERS: usize = 16 * 1024;
/// Refuse bodies larger than this.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

/// Send `method path` with `headers` and `body` to `host`, read the response.
pub async fn exchange<S: Read + Write>(
    stream: &mut S,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, &[u8])>,
) -> Result<(u16, Vec<u8>), String> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some((content_type, bytes)) = body {
        head.push_str(&format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
            bytes.len()
        ));
    } else if method == "POST" {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.map_err(io)?;
    if let Some((_, bytes)) = body {
        stream.write_all(bytes).await.map_err(io)?;
    }
    stream.flush().await.map_err(io)?;

    let mut reader = Reader {
        stream,
        buffer: vec![],
    };
    let head = reader.until(b"\r\n\r\n", MAX_HEADERS).await?;
    let head = core::str::from_utf8(&head).map_err(|_| "a response head that is not text")?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("no status in {head}"))?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.parse::<usize>().map_err(|_| "a bad content-length")?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }
    let body = if method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status)
    {
        vec![]
    } else if chunked {
        let mut body = vec![];
        loop {
            let line = reader.until(b"\r\n", 64).await?;
            let size = core::str::from_utf8(&line)
                .ok()
                .and_then(|l| usize::from_str_radix(l.split(';').next()?.trim(), 16).ok())
                .ok_or("a bad chunk size")?;
            if size == 0 {
                break;
            }
            if body.len() + size > MAX_BODY {
                return Err("an oversized body".into());
            }
            body.extend(reader.exactly(size).await?);
            reader.until(b"\r\n", 2).await?;
        }
        body
    } else if let Some(length) = length {
        if length > MAX_BODY {
            return Err("an oversized body".into());
        }
        reader.exactly(length).await?
    } else {
        reader.rest().await?
    };
    Ok((status, body))
}

fn io<E: core::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

/// Reads from `stream`, keeping what it read beyond what was asked.
struct Reader<'s, S> {
    stream: &'s mut S,
    buffer: Vec<u8>,
}

impl<S: Read> Reader<'_, S> {
    async fn fill(&mut self) -> Result<bool, String> {
        let mut chunk = [0u8; 2048];
        let n = self.stream.read(&mut chunk).await.map_err(io)?;
        self.buffer.extend_from_slice(&chunk[..n]);
        Ok(n > 0)
    }

    /// Everything up to `delimiter` (dropped), at most `limit` bytes.
    async fn until(&mut self, delimiter: &[u8], limit: usize) -> Result<Vec<u8>, String> {
        loop {
            if let Some(i) = self
                .buffer
                .windows(delimiter.len())
                .position(|w| w == delimiter)
            {
                let rest = self.buffer.split_off(i + delimiter.len());
                let mut line = core::mem::replace(&mut self.buffer, rest);
                line.truncate(i);
                return Ok(line);
            }
            if self.buffer.len() > limit + delimiter.len() {
                return Err("a response line or head too long".into());
            }
            if !self.fill().await? {
                return Err("the connection ended in the response".into());
            }
        }
    }

    async fn exactly(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buffer.len() < n {
            if !self.fill().await? {
                return Err("the connection ended in the body".into());
            }
        }
        let rest = self.buffer.split_off(n);
        Ok(core::mem::replace(&mut self.buffer, rest))
    }

    async fn rest(&mut self) -> Result<Vec<u8>, String> {
        while self.fill().await? {
            if self.buffer.len() > MAX_BODY {
                return Err("an oversized body".into());
            }
        }
        Ok(core::mem::take(&mut self.buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::convert::Infallible;

    /// A stream that answers with `response`, `chunk` bytes per read, and records what was written.
    struct Scripted {
        response: Vec<u8>,
        at: usize,
        chunk: usize,
        written: Vec<u8>,
    }

    impl embedded_io_async::ErrorType for Scripted {
        type Error = Infallible;
    }

    impl Read for Scripted {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
            let n = buf.len().min(self.chunk).min(self.response.len() - self.at);
            buf[..n].copy_from_slice(&self.response[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    impl Write for Scripted {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    fn run(
        response: &str,
        chunk: usize,
        body: Option<(&str, &[u8])>,
    ) -> (Scripted, (u16, Vec<u8>)) {
        let mut stream = Scripted {
            response: response.as_bytes().to_vec(),
            at: 0,
            chunk,
            written: vec![],
        };
        let result = embassy_futures::block_on(exchange(
            &mut stream,
            if body.is_some() { "POST" } else { "GET" },
            "x:8080",
            "/graphql?a=1",
            &[("authorization", "Bearer t")],
            body,
        ))
        .unwrap();
        (stream, result)
    }

    #[test]
    fn content_length_bodies_however_they_arrive() {
        for chunk in [1, 7, 4096] {
            let (stream, (status, body)) = run(
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX: y\r\n\r\nhello",
                chunk,
                Some(("application/json", b"{}")),
            );
            assert_eq!((status, body.as_slice()), (200, &b"hello"[..]));
            let sent = String::from_utf8(stream.written).unwrap();
            assert!(
                sent.starts_with("POST /graphql?a=1 HTTP/1.1\r\nHost: x:8080\r\n"),
                "{sent}"
            );
            assert!(sent.contains("authorization: Bearer t\r\n"));
            assert!(sent.ends_with("Content-Length: 2\r\n\r\n{}"));
        }
    }

    #[test]
    fn chunked_and_until_close_bodies() {
        let (_, (status, body)) = run(
            "HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\n\r\n",
            3,
            None,
        );
        assert_eq!((status, body.as_slice()), (201, &b"abcde"[..]));
        let (_, (status, body)) = run("HTTP/1.0 302 Found\r\nLocation: /x\r\n\r\nmoved", 2, None);
        assert_eq!((status, body.as_slice()), (302, &b"moved"[..]));
    }
}
