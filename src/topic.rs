//! BYOND `world/Topic()` packets: the only channel into a running world.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{bail, Context, Result};

const MAGIC: [u8; 2] = [0x00, 0x83];
/// Five padding bytes and the trailing NUL that the length field counts besides the query.
const FRAMING: usize = 6;

/// Sends `query` (with its leading `?`) and returns the text `world/Topic()` answered with, if it was text.
pub fn send(port: u16, query: &str, timeout: Duration) -> Result<Option<String>> {
	let length = u16::try_from(query.len() + FRAMING).context("topic query exceeds the 64 KiB packet limit")?;
	let mut packet = Vec::with_capacity(query.len() + 10);
	packet.extend_from_slice(&MAGIC);
	packet.extend_from_slice(&length.to_be_bytes());
	packet.extend_from_slice(&[0; 5]);
	packet.extend_from_slice(query.as_bytes());
	packet.push(0);

	let address = SocketAddr::from(([127, 0, 0, 1], port));
	let mut stream = TcpStream::connect_timeout(&address, timeout).context("world is not accepting connections")?;
	stream.set_read_timeout(Some(timeout))?;
	stream.set_write_timeout(Some(timeout))?;
	stream.write_all(&packet)?;

	let mut header = [0u8; 4];
	stream.read_exact(&mut header).context("world did not answer the topic")?;
	if header[..2] != MAGIC {
		bail!("unexpected topic reply header {header:02x?}");
	}
	let mut body = vec![0u8; usize::from(u16::from_be_bytes([header[2], header[3]]))];
	stream.read_exact(&mut body)?;

	// 0x06 marks a text reply; numbers and null are of no use to the harness protocol.
	Ok(match body.split_first() {
		Some((0x06, text)) => {
			let end = text.iter().position(|&byte| byte == 0).unwrap_or(text.len());
			Some(String::from_utf8_lossy(&text[..end]).into_owned())
		}
		_ => None,
	})
}

/// Percent-encodes everything `params2list` could misread.
pub fn url_encode(text: &str) -> String {
	let mut out = String::with_capacity(text.len() * 3);
	for byte in text.bytes() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
			out.push(char::from(byte));
		} else {
			out.push('%');
			out.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
			out.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
		}
	}
	out
}
