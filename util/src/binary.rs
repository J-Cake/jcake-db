//! Runtime half of `#[binary(...)]`.

use crate::Error;
use crate::Result;
use crate::io_utils::Device;
use crate::io_utils::Reader;
use crate::io_utils::Writer;
use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

/// A type with a known encoded size. Implement this to define a custom
/// representation, which can then be used as `Type(name)` in `#[binary(...)]`.
pub trait Fixed: Sized {
    const SIZE: usize;

    /// `buf` must contain exactly `Self::SIZE` bytes.
    fn parse(buf: &[u8]) -> Result<Self>;

    /// `buf` must contain exactly `Self::SIZE` bytes.
    fn serialize(&self, buf: &mut [u8]) -> Result<()>;
}

/// A type that can be read from and written to a device at its cursor.
pub trait Binary: Sized {
    fn read(dev: &mut Reader<'_, impl Device>) -> Result<Self>;
    fn write(&self, dev: &mut Writer<impl Device>) -> Result<()>;
}

macro_rules! impl_number {
	($($t:ty),* $(,)?) => {$(
		impl Fixed for $t {
			const SIZE: usize = size_of::<$t>();

			fn parse(buf: &[u8]) -> Result<Self> {
				let bytes = buf.first_chunk().ok_or(Error::new("Buffer too short"))?;
				Ok(<$t>::from_le_bytes(*bytes))
			}

			fn serialize(&self, buf: &mut [u8]) -> Result<()> {
				let out = buf.first_chunk_mut().ok_or(Error::new("Buffer too short"))?;
				*out = self.to_le_bytes();
				Ok(())
			}
		}
	)*};
}

impl_number!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64);

impl<const N: usize> Fixed for [u8; N] {
    const SIZE: usize = N;

    fn parse(buf: &[u8]) -> Result<Self> {
        buf.first_chunk().copied().ok_or(Error::new("Buffer too short"))
    }

    fn serialize(&self, buf: &mut [u8]) -> Result<()> {
        let out = buf.first_chunk_mut().ok_or(Error::new("Buffer too short"))?;
        *out = *self;
        Ok(())
    }
}

impl Fixed for Uuid {
    const SIZE: usize = 16;

    fn parse(buf: &[u8]) -> Result<Self> {
        Ok(Uuid::from_bytes(Fixed::parse(buf)?))
    }

    fn serialize(&self, buf: &mut [u8]) -> Result<()> {
        self.as_bytes().serialize(buf)
    }
}

/// A 24-bit unsigned integer, stored as 3 little-endian bytes.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct u24(u32);

impl u24 {
    pub const MAX: u32 = 0xFF_FFFF;

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Fixed for u24 {
    const SIZE: usize = 3;

    fn parse(buf: &[u8]) -> Result<Self> {
        let [a, b, c] = <[u8; 3]>::parse(buf)?;
        Ok(Self(u32::from_le_bytes([a, b, c, 0])))
    }

    fn serialize(&self, buf: &mut [u8]) -> Result<()> {
        let [a, b, c, _] = self.0.to_le_bytes();
        [a, b, c].serialize(buf)
    }
}

impl TryFrom<u32> for u24 {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        if value > Self::MAX {
            return Err(Error::new("Value doesn't fit in 24 bits"));
        }
        Ok(Self(value))
    }
}

impl TryFrom<usize> for u24 {
    type Error = Error;

    fn try_from(value: usize) -> Result<Self> {
        let value = u32::try_from(value).map_err(|_| Error::new("Value doesn't fit in 24 bits"))?;
        Self::try_from(value)
    }
}

impl From<u24> for u32 {
    fn from(value: u24) -> Self {
        value.0
    }
}

impl From<u24> for usize {
    fn from(value: u24) -> Self {
        value.0 as usize
    }
}

/// An alias so `DateTime<Utc>` can be named as `Timestamp(field)` in `#[binary(...)]`.
pub type Timestamp = DateTime<Utc>;

/// Stored as microseconds since the Unix epoch, so anything finer is dropped.
impl Fixed for Timestamp {
    const SIZE: usize = i64::SIZE;

    fn parse(buf: &[u8]) -> Result<Self> {
        DateTime::from_timestamp_micros(i64::parse(buf)?).ok_or(Error::new("Invalid timestamp"))
    }

    fn serialize(&self, buf: &mut [u8]) -> Result<()> {
        self.timestamp_micros().serialize(buf)
    }
}

/// Used by the generated code; not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use super::Binary;
    pub use super::Fixed;
    pub use crate::Error;
    pub use crate::Result;
    pub use crate::crc::check_crc;
    pub use crate::crc::crc;
    pub use crate::io_utils::Device;
    pub use crate::io_utils::Reader;
    pub use crate::io_utils::Writer;

    /// Reads everything left in the reader's window.
    pub fn read_window<D: Device>(dev: &mut Reader<'_, D>) -> Result<Box<[u8]>> {
        let remaining = dev.remaining();

        // `Reader::new` sets `end` to u64::MAX. Reading that window would abort
        // on allocation, so catch it here.
        if dev.position().checked_add(remaining) == Some(u64::MAX) {
            return Err(Error::new(
                "Variable-length types need a bounded reader; use Reader::slice or Reader::with_end",
            ));
        }

        let len = usize::try_from(remaining).map_err(|_| Error::new("Window too large"))?;
        dev.read(len)
    }

    pub fn parse_vec<T: Fixed>(buf: &[u8]) -> Result<Vec<T>> {
        if T::SIZE == 0 {
            return Err(Error::new("Can't read a vec of zero-sized entries"));
        }

        let chunks = buf.chunks_exact(T::SIZE);
        if !chunks.remainder().is_empty() {
            return Err(Error::new("Partial entry"));
        }

        chunks.map(T::parse).collect()
    }

    pub fn vec_len<T: Fixed>(items: &[T]) -> usize {
        items.len() * T::SIZE
    }

    /// `buf` must contain exactly `vec_len(items)` bytes.
    pub fn serialize_vec<T: Fixed>(items: &[T], buf: &mut [u8]) -> Result<()> {
        if T::SIZE == 0 {
            return Ok(());
        }

        for (item, out) in items.iter().zip(buf.chunks_exact_mut(T::SIZE)) {
            item.serialize(out)?;
        }
        Ok(())
    }
}
