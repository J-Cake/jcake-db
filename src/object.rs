use crate::Error;
use crate::Result;
use format::binary;
use std::convert::Infallible;
use std::sync::Arc;
use util::binary::Fixed;
use util::binary::u24;
use util::Device;
use util::Reader;
use uuid::Uuid;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKind {
	Structured = 1,
	Raw = 2,
	Tombstone = 3,
	Reference = 4,
}

impl TryFrom<u8> for ObjectKind {
	type Error = Error; // or your crate::Error variant

	fn try_from(v: u8) -> Result<Self, Self::Error> {
		Ok(match v {
			1 => Self::Structured,
			2 => Self::Raw,
			3 => Self::Tombstone,
			4 => Self::Reference,
			other => return Err(Error::new(format!("Invalid object type marker {:#2x}", other))),
		})
	}
}

impl TryFrom<ObjectKind> for u8 {
	type Error = Infallible;

	fn try_from(value: ObjectKind) -> std::result::Result<Self, Self::Error> {
		Ok(value as u8)
	}
}

impl Fixed for ObjectKind {
	const SIZE: usize = u8::SIZE;

	fn parse(buf: &[u8]) -> Result<Self> {
		u8::parse(buf).and_then(Self::try_from)
	}

	fn serialize(&self, buf: &mut [u8]) -> Result<()> {
		u8::serialize(&(*self as u8), buf)
	}
}

pub struct Object {
	header: ObjectHeader,
	pub(crate) data: ObjectData
}

#[binary(Uuid(uuid), u24(len), u8(kind), u32(crc), ptr(offset))]
#[derive(Copy, Clone, Debug)]
pub(crate) struct ObjectHeader {
	#[from(uuid)]
	pub(crate) uuid: Uuid,

	#[from(len)]
	pub(crate) length: usize,

	#[from(kind)]
	pub(crate) kind: ObjectKind,

	#[from(crc)]
	pub(crate) crc: u32,

	#[from(offset)]
	pub(crate) position: u64
}

impl ObjectHeader {
	pub(crate) fn len(&self) -> usize {
		self.length
	}

	/// Expects the reader to be seeked to the correct position
	pub(crate) fn into_object(self, dev: &mut Reader<impl Device>) -> Result<Object> {
		Object::from_header(self, dev)?
			.ok_or(Error::new("Attempted to construct object from invalid header"))
	}
}
impl Object {
	pub(crate) fn len(&self) -> usize {
		self.header.length
	}

	pub(crate) fn from_header(header: ObjectHeader, dev: &mut Reader<impl Device>) -> Result<Option<Self>> {
		let data = dev.read(header.length)?;

		let mut crc = util::Crc::new();

		crc.update(header.uuid.as_bytes());
		crc.update((header.length as u32).to_le_bytes());
		crc.update(&data);

		if crc.into_u32() == header.crc {
			Ok(Some(Self {
				header,
				data: ObjectData::Owned(data)
			}))
		} else {
			Ok(None)
		}
	}
}

pub(crate) enum ObjectData {
	Owned(Box<[u8]>),
	Shared(Arc<dyn AsRef<[u8]> + Send + Sync>),
}

impl std::ops::Deref for Object {
	type Target = [u8];

	fn deref(&self) -> &[u8] {
		match &self.data {
			ObjectData::Owned(v) => v,
			ObjectData::Shared(s) => (**s).as_ref(),
		}
	}
}