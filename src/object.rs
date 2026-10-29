use nom::bytes::take;
use nom::{IResult, Parser};
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::le_u32;
use uuid::Uuid;
use crate::BinaryRepresentation;
use crate::file::{BlockRead, BlockSeek, BlockWrite};

pub(crate) struct Object {
	uuid: Uuid,
	length: usize,
}

impl BinaryRepresentation for Object {
	fn read_struct(mut backend: impl BlockRead + BlockSeek) -> crate::Result<Self> where Self: Sized {
		let input = backend.block_read(20)?;
		let obj = try {
			let (input, uuid) = map(take(16usize), Uuid::from_slice)
				.parse(&input)?;

			let uuid =
				uuid.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

			let (_, len) = le_u32().parse_complete(input)?;

			Object { length: len as usize, uuid }
		};

		obj.map_err(|err| crate::Error::new(err.to_string()))
	}

	fn write_struct(&self, backend: impl BlockWrite + BlockSeek) -> crate::Result<()> {
		todo!()
	}
}