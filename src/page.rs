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
use crate::Result;
use crate::Error;
use crate::BinaryRepresentation;
use crate::crc::{check_crc, Crc};
use crate::crc::crc;
use crate::file::Offset;
use crate::io_utils::Device;
use crate::io_utils::StickyFile;
use crate::object::{Object, ObjectHeader, ObjectKind};

pub(crate) struct Page {
	pub(crate) seq: u64,
	pub(crate) page_offset: u64,
	page_size: usize,

	pub(crate) contents: Option<Vec<ObjectHeader>>,

	trailer: Option<PageTrailer>,
	created: chrono::DateTime<chrono::Utc>,
}

impl Page {
	pub(crate) const FIRST_PAGE_OFFSET: u64 = 0x1_0000;
	pub(crate) const PAGE_HEADER_SIZE: u64 = 28;
	pub(crate) const PAGE_FOOTER_SIZE: u64 = 12;
	pub(crate) const PAGE_SUMMARY_HEADER_SIZE: u64 = 4;
	pub(crate) const PAGE_SUMMARY_ENTRY_SIZE: u64 = 20;
	pub(crate) const OBJECT_HEADER_SIZE: u64 = 0;

	pub(crate) fn new(seq: u64, size: usize) -> Page {
		Self {
			seq,
			page_offset: Self::FIRST_PAGE_OFFSET + seq * size as u64,
			page_size: size,

			contents: None,

			trailer: None,
			created: chrono::Utc::now()
		}
	}

	pub(crate) fn is_sealed(&self) -> bool {
		self.trailer.is_some()
	}

	pub(crate) fn count_records(&self, read: &StickyFile<impl Device>) -> usize {
		if let Some(summary) = self.trailer.as_ref()
			.and_then(|trailer| trailer.summary.as_ref()) {
			return summary.len();
		} else if let Some(ref contents) = self.contents {
			return contents.len();
		}

		read.seek_by(Self::PAGE_HEADER_SIZE as i64);

		let mut count = 0usize;
		for obj in 0usize..(self.page_size / 24) {
			if let Ok(obj) = ObjectHeader::read_struct(read) {
				count += obj.len();
				read.seek_by(obj.len() as i64);
			} else {
				break;
			}
		}

		count
	}

	pub(crate) fn preload_record(&mut self, dev: &StickyFile<impl Device>) -> Result<()> {
		self.contents.replace(self.iter(dev).collect::<Result<_>>()?);
		Ok(())
	}

	pub(crate) fn iter<'a, Dev: Device>(&'a self, dev: &'a StickyFile<Dev>) -> PageIter<'a, StickyFile<Dev>> {
		PageIter {
			offset: Self::PAGE_HEADER_SIZE as Offset,
			page: self,
			backing: &dev
		}
	}

	pub(crate) fn next_free(&self, dev: &StickyFile<impl Device>) -> Option<Offset> {
		if let Some(last) = self.contents.as_ref().and_then(|content| content.last()) {
			return Some(last.offset + last.length as Offset + Self::PAGE_HEADER_SIZE as Offset);
		}

		self.iter(dev)
			.last()
			.and_then(|i| i.ok())
			.map(|i| i.offset + i.length as Offset + Self::OBJECT_HEADER_SIZE as Offset)
	}

	pub(crate) fn get_object_at(&self, offset: Offset, dev: &StickyFile<impl Device>) -> Result<Option<Object>> {
		dev.seek_to(self.page_offset as i64 + offset as i64);
		let header = ObjectHeader::read_struct(dev)?;

		Object::from_header(header, dev)
	}

	pub(crate) fn can_fit_object_of_length(&self, len: usize) -> bool {
		let Some(c) = self.contents.as_ref() else {
			// If the `contents` is empty, we can be fairly certain the page is sealed.
			// Returning `false` on a false negative is fine since we'd rather be safe than writing to an over-full page.

			return false;
		};

		let offset = c.last()
			.map(|i| i.offset + i.length as Offset + Self::OBJECT_HEADER_SIZE as Offset)
			.unwrap_or(Self::PAGE_SUMMARY_HEADER_SIZE as Offset);

		let theoretical_summary_offset = (self.page_size as u64 - Self::PAGE_FOOTER_SIZE - Self::PAGE_SUMMARY_HEADER_SIZE - (c.len() as u64 + 1) * Self::PAGE_SUMMARY_ENTRY_SIZE) as Offset;

		offset + len as Offset + Self::OBJECT_HEADER_SIZE as Offset > theoretical_summary_offset
	}
}

pub(crate) struct PageIter<'a, Dev: Device> {
	offset: Offset,
	page: &'a Page,
	backing: &'a Dev
}

impl<'a, Dev: Device> Iterator for PageIter<'a, StickyFile<Dev>> {
	type Item = Result<ObjectHeader>;

	fn next(&mut self) -> Option<Self::Item> {
		Some(try {
			let pos = self.backing.position();
			let header = ObjectHeader::read_struct(&self.backing)?;

			self.offset += (self.backing.position() - pos) as u32 + header.len() as u32;

			header
		})
	}
}

impl<Dev: Device> BinaryRepresentation<StickyFile<Dev>> for Page {
	fn read_struct(backend: &StickyFile<Dev>) -> Result<Self> where Self: Sized {
		let page_offset = backend.position() as u64;
		let input = backend.read(Self::PAGE_HEADER_SIZE as usize)?;
		let header: IResult<&[u8], (u64, u32, u64, u32)> = preceded(tag(&b"page"[..]), (le_u64(), le_u32(), le_u64(), le_u32()))
				.parse_complete(&input);

		let mut header = match header {
			Ok((_, (seq, page_size, created, crc))) if check_crc(&input[0..(Self::PAGE_HEADER_SIZE as usize - 4)], crc) => Self {
				seq,
				page_offset,
				page_size: page_size as usize,

				contents: None,

				trailer: None,
				created: chrono::DateTime::from_timestamp_micros(created as i64)
					.ok_or_else(|| Error::new("Unable to get current time"))?
			},
			Ok((_, (seq, _, _, _))) => return Err(Error::new(format!("Page {seq} corrupt."))),
			Err(err) => return Err(Error::new(err.to_string()))
		};

		backend.seek_to((page_offset - Self::PAGE_FOOTER_SIZE) as i64);
		if let Some(trailer) = PageTrailer::read_struct(backend).ok() {
			header.trailer.replace(trailer);
		}

		Ok(header)
	}

	fn write_struct(&self, backend: &mut StickyFile<Dev>) -> crate::Result<()> {
		let start = backend.position();
		let mut buf = vec![];

		buf.extend_from_slice(&b"page"[..]);
		buf.extend_from_slice(&self.seq.to_le_bytes());
		buf.extend_from_slice(&(self.page_size as u32).to_le_bytes());
		buf.extend_from_slice(&(self.created.timestamp_micros() as u64).to_le_bytes());
		buf.extend_from_slice(crc(&buf).as_slice());

		backend.write(&buf)?;

		let Some(ref trailer) = self.trailer else {
			return Ok(());
		};

		if let Some(ref summary) = trailer.summary && summary.len() != trailer.num_items {
			return Err(crate::Error::new("Summary length does not match reported object count."));
		}

		let travelled = backend.position() - start;

		let mut buf = StickyFile::new(if let Some(ref summary) = trailer.summary {
			serialise_summary(summary)
		} else {
			// TODO: While we're at it, we might as well produce a summary
			vec![]
		});

		buf.seek_to(buf.len() as i64 - 1);
		trailer.write_struct(&mut buf)?;

		let buf = buf.into_inner();

		dbg!(&buf);

		backend.seek_by(self.page_size as i64 - travelled);
		backend.write(&buf)?;

		Ok(())
	}
}

struct PageTrailer {
	summary: Option<HashMap<Uuid, u32>>,
	num_items: usize
}

impl<Dev: Device> BinaryRepresentation<StickyFile<Dev>> for PageTrailer {
	fn read_struct(backend: &StickyFile<Dev>) -> crate::Result<Self> where Self: Sized {
		let trailer = backend.read(Page::PAGE_FOOTER_SIZE as usize)?;
		let len = match parse_trailer(&trailer) {
			Ok((_, len)) => len,
			Err(err) => return Err(crate::Error::new(err.to_string()))
		};

		if len <= 0 {
			return Ok(Self {
				summary: None,
				num_items: 0
			});
		}

		backend.seek_by(0 - (20 * len + 16) as i64);
		let summary = backend.read(20 * len + 4)?;
		let summary = match parse_summary(&summary, len) {
			Ok((_, summary)) => summary,
			Err(err) => return Err(crate::Error::new(err.to_string()))
		};

		Ok(Self {
			summary: if summary.is_empty() { None } else { Some(summary) },
			num_items: len
		})
	}

	/// PageTrailer does not serialise the summary.
	fn write_struct(&self, backend: &mut StickyFile<Dev>) -> crate::Result<()> {
		let mut buf = vec![];

		buf.extend_from_slice(&b"foot"[..]);
		buf.extend_from_slice(&(self.num_items as u32).to_le_bytes());
		buf.extend_from_slice(crc(&buf[..]).as_slice());

		backend.write(&buf)?;

		Ok(())
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

fn serialise_summary(summary: &HashMap<Uuid, u32>) -> Vec<u8> {
	let mut buf = vec![];

	if summary.is_empty() {
		return buf;
	}

	buf.extend_from_slice(&b"smry"[..]);

	for (uuid, offset) in summary {
		buf.extend_from_slice(uuid.as_bytes());
		buf.extend_from_slice(&offset.to_le_bytes());
	}

	buf.extend_from_slice(crc(&buf).as_slice());

	buf
}