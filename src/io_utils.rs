use std::fs::File;
use std::io;
use std::io::Cursor;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicI64, Ordering};
use crate::Result;

pub(crate) trait Device {
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
		sys::read_exact_at(self, &mut buf, offset)?;
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
			.ok_or(io::Error::from(io::ErrorKind::UnexpectedEof))?;

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

pub(crate) struct StickyFile<Dev: Device>(Dev, AtomicI64);

impl<T: Device> StickyFile<T> {
	pub(crate) fn new(dev: T) -> Self {
		Self(dev, Default::default())
	}

	pub(crate) fn read(&self, len: usize) -> Result<Box<[u8]>> {
		self.read_at(self.1.load(Ordering::Relaxed) as u64, len)
	}

	pub(crate) fn write(&mut self, data: &[u8]) -> Result<()> {
		self.write_at(self.1.load(Ordering::Relaxed) as u64, data)
	}

	pub(crate) fn seek_by(&self, by: i64) {
		self.1.fetch_add(by, Ordering::Relaxed);
	}

	pub(crate) fn seek_to(&self, to: i64) {
		self.1.store(to, Ordering::Relaxed);
	}

	pub(crate) fn position(&self) -> i64 {
		self.1.load(Ordering::Relaxed)
	}

	pub(crate) fn into_inner(self) -> T {
		self.0
	}
}

/// Deliberately no `DerefMut` because that allows writing without tracking a cursor position.
/// To write without a cursor, destroy the object and recreate it.
impl<T: Device> Deref for StickyFile<T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl<Backing: Device> Device for StickyFile<Backing> {
	fn read_at(&self, offset: u64, len: usize) -> Result<Box<[u8]>> {
		self.1.store(offset as i64 + len as i64, Ordering::Relaxed);
		self.0.read_at(offset, len)
	}

	fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
		self.1.store(offset as i64 + data.len() as i64, Ordering::Relaxed);
		self.0.write_at(offset, data)
	}

	fn sync(&mut self) -> Result<()> {
		self.0.sync()
	}
}

pub(crate) trait IntoSticky where Self: Device + Sized {
	fn into_sticky(self) -> StickyFile<Self> {
		StickyFile::new(self)
	}
}

impl<Dev: Device> IntoSticky for Dev {}