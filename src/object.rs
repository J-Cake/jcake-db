use std::sync::Arc;
use nom::bytes::{tag, take};
use nom::{IResult, Parser};
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::{le_u24, u8};
use nom::sequence::preceded;
use uuid::Uuid;
use crate::BinaryRepresentation;
use crate::io_utils::{Device, StickyFile};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKind {
	Structured = 1,
	Raw = 2,
	Tombstone = 3,
	Reference = 4,
}

impl TryFrom<u8> for ObjectKind {
	type Error = crate::Error; // or your crate::Error variant

	fn try_from(v: u8) -> Result<Self, Self::Error> {
		Ok(match v {
			1 => Self::Structured,
			2 => Self::Raw,
			3 => Self::Tombstone,
			4 => Self::Reference,
			other => return Err(crate::Error::new(format!("Invalid object type marker {:#2x}", other))),
		})
	}
}

pub(crate) struct Object {
	uuid: Uuid,
	length: usize,
	kind: ObjectKind,
	data: ObjectData
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

impl<Dev: Device> BinaryRepresentation<StickyFile<Dev>> for Object {
	fn read_struct(backend: &StickyFile<Dev>) -> crate::Result<Self> where Self: Sized {
		let input = backend.read(20)?;
		let obj = try {
			let (input, uuid) = map(take(16usize), Uuid::from_slice)
				.parse(&input)?;

			let uuid =
				uuid.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

			let (_, (len, flags)) = (le_u24(), u8())
				.parse_complete(input)?;

			(uuid, len as usize, flags)
		};

		let (uuid, len, flags) = obj.map_err(|err| crate::Error::new(err.to_string()))?;

		let data = backend.read(len)?;

		Ok(Object {
			length: len,
			uuid,
			kind: flags.try_into()?,
			data: ObjectData::Owned(data),
		})
	}

	fn write_struct(&self, backend: &mut StickyFile<Dev>) -> crate::Result<()> {
		todo!()
	}
}