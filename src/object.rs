use std::ops::Deref;
use std::sync::Arc;
use nom::bytes::{tag, take};
use nom::{IResult, Parser};
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::{le_u24, le_u32, u8};
use nom::sequence::preceded;
use uuid::Uuid;
use crate::Result;
use crate::Error;
use crate::BinaryRepresentation;
use crate::crc::{check_crc, Crc};
use crate::file::Offset;
use crate::io_utils::{Device, StickyFile};
use crate::page::Page;

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

pub struct Object {
	header: ObjectHeader,
	pub(crate) data: ObjectData
}

pub(crate) struct ObjectHeader {
	pub(crate) uuid: Uuid,
	pub(crate) length: usize,
	pub(crate) kind: ObjectKind,
	pub(crate) offset: Offset,
	pub(crate) crc: u32,
}

impl ObjectHeader {
	pub(crate) fn len(&self) -> usize {
		self.length
	}

	pub(crate) fn into_object(self, dev: &StickyFile<impl Device>) -> Result<Object> {
		Object::from_header(self, dev)?
			.ok_or(Error::new("Attempted to construct object from invalid header"))
	}
}
impl Object {
	pub(crate) fn len(&self) -> usize {
		self.header.length
	}

	pub(crate) fn from_header(header: ObjectHeader, dev: &StickyFile<impl Device>) -> Result<Option<Self>> {
		let data = dev.read(header.length)?;

		let mut crc = Crc::new();

		crc.update(header.uuid.as_bytes());
		crc.update((header.length as u32).to_le_bytes());
		crc.update(&data);

		if crc.finish() == header.crc.to_le_bytes() {
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

impl<Dev: Device> BinaryRepresentation<StickyFile<Dev>> for ObjectHeader {
	fn read_struct(backend: &StickyFile<Dev>) -> Result<Self> where Self: Sized {
		let offset = backend.position();
		let input = backend.read(Page::OBJECT_HEADER_SIZE as usize)?;
		let obj = try {
			let (input, uuid) = map(take(16usize), Uuid::from_slice)
				.parse(&input)?;

			let uuid =
				uuid.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

			let (_, (len, flags, crc)) = (le_u24(), u8(), le_u32())
				.parse_complete(input)?;

			(uuid, len as usize, flags, crc)
		};

		let (uuid, len, flags, crc) = obj.map_err(|err| Error::new(err.to_string()))?;

		// TODO: CRC currently doesn't cover the body.
		// In order to make the split between header-only and header-plus-payload work, we'd have to simply skip the check for the header and trust that everything's alright.
		if !check_crc(&input[0..20], crc) {
			return Err(Error::new(format!("Object {uuid} is corrupt.")));
		}

		Ok(ObjectHeader {
			length: len,
			uuid,
			crc,
			kind: flags.try_into()?,
			offset: offset as u32
		})
	}

	fn write_struct(&self, backend: &mut StickyFile<Dev>) -> Result<()> {
		todo!()
	}
}

impl<Dev: Device> BinaryRepresentation<StickyFile<Dev>> for Object {
	fn read_struct(backend: &StickyFile<Dev>) -> Result<Self> where Self: Sized {
		let header = ObjectHeader::read_struct(backend)?;

		let data = backend.read(header.length)?;

		Ok(Object {
			header,
			data: ObjectData::Owned(data),
		})
	}

	fn write_struct(&self, backend: &mut StickyFile<Dev>) -> Result<()> {
		todo!()
	}
}