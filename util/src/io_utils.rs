use std::fs::File;
use std::io;
use std::io::ErrorKind;
use crate::{Error, Result};

pub trait Device {
	fn read_at(&self, offset: u64, len: usize) -> Result<Box<[u8]>>;
	fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()>;
	fn sync(&mut self) -> Result<()>;
}

impl<Dev: Device + ?Sized> Device for &mut Dev {
	fn read_at(&self, offset: u64, len: usize) -> Result<Box<[u8]>> {
		(**self).read_at(offset, len)
	}

	fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
		(**self).write_at(offset, data)
	}

	fn sync(&mut self) -> Result<()> {
		(**self).sync()
	}
}

impl Device for File {
	fn read_at(&self, offset: u64, len: usize) -> Result<Box<[u8]>> {
		let mut buf = vec![0u8; len].into_boxed_slice();
		if let Err(err) = sys::read_exact_at(self, &mut buf, offset) && err.kind() == ErrorKind::UnexpectedEof {
			return Err(Error::eof());
		};

		Ok(buf)
	}

	fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
		sys::write_all_at(self, data, offset)?;
		Ok(())
	}

	fn sync(&mut self) -> Result<()> {
		self.sync_data()?;
		Ok(())
	}
}

/// In-memory device for tests; grows like a sparse file.
impl Device for Vec<u8> {
	fn read_at(&self, offset: u64, len: usize) -> Result<Box<[u8]>> {
		let src = usize::try_from(offset)
			.ok()
			.and_then(|s| self.get(s..s.checked_add(len)?))
			.ok_or(Error::eof())?;

		Ok(src.to_vec().into_boxed_slice())
	}

	fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
		let start = usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
		let end = start.checked_add(data.len()).ok_or(io::Error::from(io::ErrorKind::InvalidInput))?;
		if self.len() < end {
			self.resize(end, 0);
		}
		self[start..end].copy_from_slice(data);
		Ok(())
	}

	fn sync(&mut self) -> Result<()> {
		Ok(())
	}
}

#[cfg(unix)]
mod sys {
	pub use std::os::unix::fs::FileExt;

	pub fn read_exact_at(f: &std::fs::File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
		f.read_exact_at(buf, off)
	}

	pub fn write_all_at(f: &std::fs::File, buf: &[u8], off: u64) -> std::io::Result<()> {
		f.write_all_at(buf, off)
	}
}

#[cfg(windows)]
mod sys {
	use std::fs::File;
	use std::io::{self, ErrorKind};
	use std::os::windows::fs::FileExt;

	pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
		while !buf.is_empty() {
			match f.seek_read(buf, off) {
				Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
				Ok(n) => { buf = &mut buf[n..]; off += n as u64; }
				Err(e) if e.kind() == ErrorKind::Interrupted => {}
				Err(e) => return Err(e),
			}
		}
		Ok(())
	}

	pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
		while !buf.is_empty() {
			match f.seek_write(buf, off) {
				Ok(0) => return Err(ErrorKind::WriteZero.into()),
				Ok(n) => { buf = &buf[n..]; off += n as u64; }
				Err(e) if e.kind() == ErrorKind::Interrupted => {}
				Err(e) => return Err(e),
			}
		}
		Ok(())
	}
}

/// The only handle that can write. Append-only: `pos` never moves backwards.
pub struct Writer<D: Device> {
	dev: D,
	pos: u64,
}

impl<D: Device> Writer<D> {
	/// A new writer with effectively no end
	pub fn new(dev: D) -> Self {
		Self { dev, pos: 0 }
	}

	/// `end` = current end of valid data (0 for a fresh file, or found by scanning).
	pub fn with_end(dev: D, end: u64) -> Self {
		Self { dev, pos: end }
	}

	/// Appends `data`, returns the offset it was written at.
	pub fn append(&mut self, data: &[u8]) -> Result<u64> {
		let at = self.pos;
		self.dev.write_at(at, data)?;
		self.pos += data.len() as u64;
		Ok(at)
	}

	pub fn position(&self) -> u64 {
		self.pos
	}

	pub fn advance(&mut self, by: u64) -> &mut Self {
		self.pos += by;
		self
	}

	pub fn advance_to_alignment(&mut self, interval: u64) -> &mut Self {
		self.pos = self.pos.next_multiple_of(interval);
		self
	}

	pub fn try_seek_to(&mut self, offset: u64) -> Result<&mut Self> {
		if self.pos > offset  {
			return Err(Error::new("Attempted to move cursor backwards."))
		}

		self.advance(offset - self.pos);
		Ok(self)
	}

	pub fn sync(&mut self) -> Result<()> {
		self.dev.sync()
	}

	/// A reader over everything written so far. Borrows `self`, so no appends while it lives.
	pub fn reader(&self) -> Reader<'_, D> {
		Reader::new(&self.dev)
	}

	pub fn into_inner(self) -> D {
		self.dev
	}
}

/// Cheap, cloneable, read-only cursor. Never reads past `end`.
pub struct Reader<'a, D: Device + ?Sized> {
	dev: &'a D,
	pos: u64,
	end: u64,
}

impl<D: Device> Clone for Reader<'_, D> {
	fn clone(&self) -> Self {
		Self { ..*self }
	}
}

impl<'a, D: Device> Reader<'a, D> {

	/// A new writer with effectively no end
	pub fn new(dev: &'a D) -> Self {
		Self { dev, pos: 0, end: u64::MAX }
	}

	/// Also usable without a writer, e.g. for a read-only open.
	pub fn with_end(dev: &'a D, end: u64) -> Self {
		Self { dev, pos: 0, end }
	}
	pub fn read(&mut self, len: usize) -> Result<Box<[u8]>> {
		if self.pos + len as u64 > self.end {
			return Err(Error::eof());
		}

		let buf = self.dev.read_at(self.pos, len)?;
		self.pos += len as u64;
		Ok(buf)
	}

	pub fn position(&self) -> u64 { self.pos }
	pub fn remaining(&self) -> u64 { self.end - self.pos }
	pub fn seek_to(&mut self, to: u64) { self.pos = to.min(self.end); }
	pub fn seek_by(&mut self, by: i64) { self.seek_to((self.pos as i64 + by) as u64); }
	pub fn with_seek(mut self, seek: u64) -> Self {
		self.seek_to(seek);
		self
	}

	/// A sub-reader bounded to `[start, start + len)`, e.g. one page.
	pub fn slice(&self, start: u64, len: u64) -> Self {
		Self { dev: self.dev, pos: start, end: (start + len).min(self.end) }
	}
}