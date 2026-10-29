use std::collections::HashMap;
use nom::IResult;
use nom::bytes::*;
use nom::combinator::map;
use nom::error::ErrorKind;
use nom::number::le_u32;
use nom::Parser;
use nom::number::le_u64;
use nom::multi::count;
use nom::sequence::preceded;
use uuid::Uuid;
use crate::{BinaryRepresentation, StorageBackend};
use crate::file::BlockDev;

const PAGE_SIZE: usize = 16_777_216; // 16MiB

pub(crate) struct Page {
	seq: u64,
	// objects: Vec<Object>,
	trailer: Option<PageTrailer>
}

impl<Backend: BlockDev> BinaryRepresentation<Backend> for Page {
	fn read_struct(backend: &mut Backend) -> crate::Result<Self> where Self: Sized {
		let header = backend.block_read(16)?;
		let seq = match parse_header(&header) {
			Ok((_, seq)) => seq,
			Err(err) => return Err(crate::Error::new(err.to_string()))
		};

		backend.block_seek(PAGE_SIZE as isize - 16)?;
		let trailer = PageTrailer::read_struct(backend).ok();

		Ok(Self {
			seq,
			// objects: vec![],
			trailer,
		})
	}

	fn write_struct(&self, backend: &mut Backend) -> crate::Result<()> {
		todo!()
	}
}

fn parse_header(input: &[u8]) -> IResult<&[u8], u64> {
	let (input, (seq, crc)) = preceded(tag(&b"page"[..]), (le_u64(), le_u32()))
		.parse(input)?;

	Ok((input, seq))
}

struct PageTrailer {
	summary: Option<HashMap<Uuid, u32>>
}

impl<Block: BlockDev> BinaryRepresentation<Block> for PageTrailer {
	fn read_struct(backend: &mut Block) -> crate::Result<Self> where Self: Sized {
		let trailer = backend.block_read(12)?;
		let len = match parse_trailer(&trailer) {
			Ok((_, len)) => len,
			Err(err) => return Err(crate::Error::new(err.to_string()))
		};

		if len <= 0 {
			return Ok(Self {
				summary: None,
			});
		}

		backend.block_seek(0 - (20 * len + 16) as isize)?;
		let summary = backend.block_read(20 * len + 4)?;
		let summary = match parse_summary(&summary, len) {
			Ok((_, summary)) => summary,
			Err(err) => return Err(crate::Error::new(err.to_string()))
		};

		Ok(Self {
			summary: if summary.is_empty() { None } else { Some(summary) },
		})
	}

	fn write_struct(&self, backend: &mut Block) -> crate::Result<()> {
		todo!()
	}
}

fn parse_trailer(input: &[u8]) -> IResult<&[u8], usize> {
	let (input, (summary_len, crc)) = preceded(tag(&b"foot"[..]), (le_u32(), le_u32()))
		.parse(input)?;

	Ok((input, summary_len as usize))
}

fn parse_summary(input: &[u8], entries: usize) -> IResult<&[u8], HashMap<Uuid, u32>> {
	let (input, records) = preceded(tag(&b"smry"[..]), count((map(take(16usize), Uuid::from_slice), le_u32()), entries))
		.parse(input)?;

	let map = records.into_iter()
		.map(|(uuid, offset)| uuid.map(|i| (i, offset)))
		.collect::<Result<HashMap<Uuid, u32>, _>>()
		.map_err(|err| nom::Err::Error(nom::error::Error::new(input, ErrorKind::IsNot)))?;

	Ok((input, map))
}